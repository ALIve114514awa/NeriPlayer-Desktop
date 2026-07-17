use rodio::Source;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, TryLockError};
use std::thread::{self, JoinHandle};
use std::time::Duration;

pub const DEFAULT_PREBUFFER_DURATION: Duration = Duration::from_millis(800);
pub const MIN_BUFFER_CAPACITY: Duration = Duration::from_millis(1_500);
pub const DEFAULT_BUFFER_CAPACITY: Duration = Duration::from_secs(15);

const TRANSFER_CHUNK_SAMPLES: usize = 4_096;

type PcmSource = Box<dyn Source<Item = i16> + Send>;

struct BufferState {
    samples: VecDeque<i16>,
    producer_finished: bool,
}

struct SharedBuffer {
    state: Mutex<BufferState>,
    not_full: Condvar,
    data_available: Condvar,
    cancelled: AtomicBool,
    capacity_samples: usize,
}

/// 将上游解码与音频消费线程隔离的有界 PCM 缓冲
///
/// 构造阶段同步预读指定时长，之后由独立线程继续解码。消费端只尝试获取锁，
/// 不等待生产端、网络或条件变量；缓冲暂空时返回静音，生产结束且缓冲排空后结束
pub struct AsyncPcmSource {
    shared: Arc<SharedBuffer>,
    producer: Option<JoinHandle<()>>,
    channels: u16,
    sample_rate: u32,
    total_duration: Option<Duration>,
    local_samples: VecDeque<i16>,
}

impl AsyncPcmSource {
    pub fn new(source: PcmSource, prebuffer: Duration, capacity: Duration) -> Self {
        Self::new_cancellable(source, prebuffer, capacity, || false)
            .expect("PCM prebuffer without cancellation cannot be cancelled")
    }

    pub fn new_cancellable(
        source: PcmSource,
        prebuffer: Duration,
        capacity: Duration,
        should_cancel: impl Fn() -> bool,
    ) -> Option<Self> {
        let channels = source.channels();
        let sample_rate = source.sample_rate();
        let total_duration = source.total_duration();
        let effective_capacity = capacity.max(MIN_BUFFER_CAPACITY).max(prebuffer);
        let capacity_samples =
            duration_to_sample_count(effective_capacity, channels, sample_rate).max(1);
        let prebuffer_samples =
            duration_to_sample_count(prebuffer, channels, sample_rate).min(capacity_samples);

        let shared = Arc::new(SharedBuffer {
            state: Mutex::new(BufferState {
                samples: VecDeque::with_capacity(prebuffer_samples.max(TRANSFER_CHUNK_SAMPLES)),
                producer_finished: false,
            }),
            not_full: Condvar::new(),
            data_available: Condvar::new(),
            cancelled: AtomicBool::new(false),
            capacity_samples,
        });
        let producer = Some(spawn_producer(Arc::clone(&shared), source));
        let buffered = Self {
            shared,
            producer,
            channels,
            sample_rate,
            total_duration,
            local_samples: VecDeque::with_capacity(TRANSFER_CHUNK_SAMPLES),
        };
        if buffered.wait_for_prebuffer(prebuffer_samples, should_cancel) {
            Some(buffered)
        } else {
            buffered.cancel();
            None
        }
    }

    pub fn with_default_buffer(source: PcmSource) -> Self {
        Self::new(
            source,
            DEFAULT_PREBUFFER_DURATION,
            DEFAULT_BUFFER_CAPACITY,
        )
    }

    /// 请求生产线程停止，不丢弃已经缓冲的样本
    pub fn cancel(&self) {
        self.shared.cancelled.store(true, Ordering::Release);
        self.shared.not_full.notify_all();
        self.shared.data_available.notify_all();
    }

    fn wait_for_prebuffer(
        &self,
        prebuffer_samples: usize,
        should_cancel: impl Fn() -> bool,
    ) -> bool {
        if prebuffer_samples == 0 {
            return !should_cancel();
        }

        let mut state = lock_state(&self.shared);
        loop {
            if should_cancel() || self.shared.cancelled.load(Ordering::Acquire) {
                return false;
            }
            if state.samples.len() >= prebuffer_samples || state.producer_finished {
                return true;
            }
            state = match self
                .shared
                .data_available
                .wait_timeout(state, Duration::from_millis(20))
            {
                Ok((state, _)) => state,
                Err(error) => error.into_inner().0,
            };
        }
    }

    fn refill_local_samples(&mut self, mut state: MutexGuard<'_, BufferState>) -> Option<i16> {
        let transfer_count = state.samples.len().min(TRANSFER_CHUNK_SAMPLES);
        self.local_samples.extend(state.samples.drain(..transfer_count));
        let producer_finished = state.producer_finished;
        drop(state);

        if transfer_count > 0 {
            self.shared.not_full.notify_one();
            return self.local_samples.pop_front();
        }

        if producer_finished {
            None
        } else {
            Some(0)
        }
    }
}

impl Iterator for AsyncPcmSource {
    type Item = i16;

    fn next(&mut self) -> Option<Self::Item> {
        if let Some(sample) = self.local_samples.pop_front() {
            return Some(sample);
        }
        let shared = Arc::clone(&self.shared);
        let result = match shared.state.try_lock() {
            Ok(state) => self.refill_local_samples(state),
            Err(TryLockError::Poisoned(error)) => {
                self.refill_local_samples(error.into_inner())
            }
            Err(TryLockError::WouldBlock) => Some(0),
        };
        result
    }
}

impl Source for AsyncPcmSource {
    fn current_frame_len(&self) -> Option<usize> {
        None
    }

    fn channels(&self) -> u16 {
        self.channels
    }

    fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    fn total_duration(&self) -> Option<Duration> {
        self.total_duration
    }
}

impl Drop for AsyncPcmSource {
    fn drop(&mut self) {
        self.cancel();
        if self.producer.as_ref().is_some_and(JoinHandle::is_finished) {
            if let Some(producer) = self.producer.take() {
                let _ = producer.join();
            }
        }
    }
}

fn spawn_producer(shared: Arc<SharedBuffer>, mut source: PcmSource) -> JoinHandle<()> {
    thread::Builder::new()
        .name("audio-pcm-buffer".to_string())
        .spawn(move || {
            let _completion = ProducerCompletion::new(Arc::clone(&shared));
            loop {
                let mut state = lock_state(&shared);
                while state.samples.len() >= shared.capacity_samples
                    && !shared.cancelled.load(Ordering::Acquire)
                {
                    state = match shared.not_full.wait(state) {
                        Ok(state) => state,
                        Err(error) => error.into_inner(),
                    };
                }
                if shared.cancelled.load(Ordering::Acquire) {
                    return;
                }
                let available = shared.capacity_samples.saturating_sub(state.samples.len());
                drop(state);

                let mut chunk = Vec::with_capacity(available.min(TRANSFER_CHUNK_SAMPLES));
                for _ in 0..available.min(TRANSFER_CHUNK_SAMPLES) {
                    if shared.cancelled.load(Ordering::Acquire) {
                        return;
                    }
                    let Some(sample) = source.next() else {
                        break;
                    };
                    chunk.push(sample);
                }
                if chunk.is_empty() || shared.cancelled.load(Ordering::Acquire) {
                    return;
                }

                let mut state = lock_state(&shared);
                let remaining = shared.capacity_samples.saturating_sub(state.samples.len());
                state.samples.extend(chunk.into_iter().take(remaining));
                drop(state);
                shared.data_available.notify_all();
            }
        })
        .expect("无法启动 PCM 缓冲生产线程")
}

struct ProducerCompletion {
    shared: Arc<SharedBuffer>,
}

impl ProducerCompletion {
    fn new(shared: Arc<SharedBuffer>) -> Self {
        Self { shared }
    }
}

impl Drop for ProducerCompletion {
    fn drop(&mut self) {
        let mut state = lock_state(&self.shared);
        state.producer_finished = true;
        drop(state);
        self.shared.not_full.notify_all();
        self.shared.data_available.notify_all();
    }
}

fn lock_state(shared: &SharedBuffer) -> MutexGuard<'_, BufferState> {
    match shared.state.lock() {
        Ok(state) => state,
        Err(error) => error.into_inner(),
    }
}

fn duration_to_sample_count(duration: Duration, channels: u16, sample_rate: u32) -> usize {
    let samples = duration
        .as_nanos()
        .saturating_mul(u128::from(channels.max(1)))
        .saturating_mul(u128::from(sample_rate.max(1)))
        .saturating_add(999_999_999)
        / 1_000_000_000;
    usize::try_from(samples).unwrap_or(usize::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::mpsc;
    use std::time::Instant;

    struct TestSource {
        samples: std::vec::IntoIter<i16>,
        channels: u16,
        sample_rate: u32,
        total_duration: Option<Duration>,
    }

    impl TestSource {
        fn new(samples: Vec<i16>, channels: u16, sample_rate: u32) -> Self {
            let sample_count = samples.len() as u64;
            let frames_per_second = u64::from(channels) * u64::from(sample_rate);
            let total_duration = (frames_per_second > 0)
                .then(|| Duration::from_secs_f64(sample_count as f64 / frames_per_second as f64));
            Self {
                samples: samples.into_iter(),
                channels,
                sample_rate,
                total_duration,
            }
        }
    }

    impl Iterator for TestSource {
        type Item = i16;

        fn next(&mut self) -> Option<Self::Item> {
            self.samples.next()
        }
    }

    impl Source for TestSource {
        fn current_frame_len(&self) -> Option<usize> {
            Some(self.samples.len())
        }

        fn channels(&self) -> u16 {
            self.channels
        }

        fn sample_rate(&self) -> u32 {
            self.sample_rate
        }

        fn total_duration(&self) -> Option<Duration> {
            self.total_duration
        }
    }

    struct SlowSource {
        entered: Option<mpsc::Sender<()>>,
        delay: Duration,
        dropped: Arc<AtomicBool>,
    }

    impl Iterator for SlowSource {
        type Item = i16;

        fn next(&mut self) -> Option<Self::Item> {
            if let Some(entered) = self.entered.take() {
                let _ = entered.send(());
            }
            thread::sleep(self.delay);
            Some(7)
        }
    }

    impl Source for SlowSource {
        fn current_frame_len(&self) -> Option<usize> {
            None
        }

        fn channels(&self) -> u16 {
            1
        }

        fn sample_rate(&self) -> u32 {
            10
        }

        fn total_duration(&self) -> Option<Duration> {
            None
        }
    }

    impl Drop for SlowSource {
        fn drop(&mut self) {
            self.dropped.store(true, Ordering::SeqCst);
        }
    }

    struct CountingSource {
        reads: Arc<AtomicUsize>,
        dropped: Option<Arc<AtomicBool>>,
    }

    impl Iterator for CountingSource {
        type Item = i16;

        fn next(&mut self) -> Option<Self::Item> {
            self.reads.fetch_add(1, Ordering::SeqCst);
            Some(1)
        }
    }

    impl Source for CountingSource {
        fn current_frame_len(&self) -> Option<usize> {
            None
        }

        fn channels(&self) -> u16 {
            1
        }

        fn sample_rate(&self) -> u32 {
            10
        }

        fn total_duration(&self) -> Option<Duration> {
            None
        }
    }

    impl Drop for CountingSource {
        fn drop(&mut self) {
            if let Some(dropped) = &self.dropped {
                dropped.store(true, Ordering::SeqCst);
            }
        }
    }

    #[test]
    fn empty_source_ends_immediately() {
        let source = TestSource::new(Vec::new(), 2, 48_000);
        let mut buffered = AsyncPcmSource::with_default_buffer(Box::new(source));

        assert_eq!(buffered.next(), None);
    }

    #[test]
    fn silent_samples_and_source_metadata_are_preserved() {
        let source = TestSource::new(vec![0; 8], 2, 4);
        let expected_duration = source.total_duration();
        let mut buffered = AsyncPcmSource::new(
            Box::new(source),
            Duration::from_secs(2),
            MIN_BUFFER_CAPACITY,
        );

        assert_eq!(buffered.channels(), 2);
        assert_eq!(buffered.sample_rate(), 4);
        assert_eq!(buffered.total_duration(), expected_duration);
        assert_eq!(buffered.by_ref().take(8).collect::<Vec<_>>(), vec![0; 8]);
        assert_eq!(buffered.next(), None);
    }

    #[test]
    fn slow_source_does_not_block_consumer() {
        let (entered_tx, entered_rx) = mpsc::channel();
        let dropped = Arc::new(AtomicBool::new(false));
        let source = SlowSource {
            entered: Some(entered_tx),
            delay: Duration::from_millis(250),
            dropped: Arc::clone(&dropped),
        };
        let mut buffered =
            AsyncPcmSource::new(Box::new(source), Duration::ZERO, MIN_BUFFER_CAPACITY);
        entered_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("生产线程未开始读取慢速源");

        let started = Instant::now();
        assert_eq!(buffered.next(), Some(0));
        assert!(started.elapsed() < Duration::from_millis(100));

        drop(buffered);
        wait_until(Duration::from_secs(1), || dropped.load(Ordering::SeqCst));
        assert!(dropped.load(Ordering::SeqCst));
    }

    #[test]
    fn cancellable_prebuffer_does_not_wait_for_blocked_source() {
        let cancelled = Arc::new(AtomicBool::new(false));
        let dropped = Arc::new(AtomicBool::new(false));
        let source = SlowSource {
            entered: None,
            delay: Duration::from_millis(250),
            dropped: Arc::clone(&dropped),
        };
        let cancel_flag = Arc::clone(&cancelled);
        let canceller = thread::spawn(move || {
            thread::sleep(Duration::from_millis(30));
            cancel_flag.store(true, Ordering::SeqCst);
        });

        let started = Instant::now();
        let buffered = AsyncPcmSource::new_cancellable(
            Box::new(source),
            Duration::from_millis(800),
            MIN_BUFFER_CAPACITY,
            || cancelled.load(Ordering::SeqCst),
        );

        assert!(buffered.is_none());
        assert!(started.elapsed() < Duration::from_millis(150));
        assert!(canceller.join().is_ok());
        wait_until(Duration::from_secs(1), || dropped.load(Ordering::SeqCst));
        assert!(dropped.load(Ordering::SeqCst));
    }

    #[test]
    fn contended_buffer_lock_does_not_block_consumer() {
        let source = TestSource::new(vec![5], 1, 10);
        let mut buffered = AsyncPcmSource::with_default_buffer(Box::new(source));
        let shared = Arc::clone(&buffered.shared);
        let state = lock_state(&shared);

        let started = Instant::now();
        assert_eq!(buffered.next(), Some(0));
        assert!(started.elapsed() < Duration::from_millis(100));

        drop(state);
        assert_eq!(buffered.next(), Some(5));
        assert_eq!(buffered.next(), None);
    }

    #[test]
    fn producer_never_exceeds_bounded_capacity() {
        let reads = Arc::new(AtomicUsize::new(0));
        let source = CountingSource {
            reads: Arc::clone(&reads),
            dropped: None,
        };
        let buffered =
            AsyncPcmSource::new(Box::new(source), Duration::ZERO, Duration::from_millis(1));
        let expected_capacity =
            duration_to_sample_count(MIN_BUFFER_CAPACITY, buffered.channels, buffered.sample_rate);

        wait_until(Duration::from_secs(1), || {
            reads.load(Ordering::SeqCst) == expected_capacity
        });
        thread::sleep(Duration::from_millis(20));

        assert_eq!(buffered.shared.capacity_samples, expected_capacity);
        assert_eq!(reads.load(Ordering::SeqCst), expected_capacity);
        assert_eq!(
            lock_state(&buffered.shared).samples.len(),
            expected_capacity
        );
    }

    #[test]
    fn finite_source_drains_before_ending() {
        let source = TestSource::new(vec![11, 22, 33], 1, 10);
        let mut buffered = AsyncPcmSource::new(
            Box::new(source),
            Duration::from_millis(800),
            MIN_BUFFER_CAPACITY,
        );

        assert_eq!(buffered.next(), Some(11));
        assert_eq!(buffered.next(), Some(22));
        assert_eq!(buffered.next(), Some(33));
        assert_eq!(buffered.next(), None);
    }

    #[test]
    fn drop_wakes_full_producer_and_releases_source() {
        let reads = Arc::new(AtomicUsize::new(0));
        let dropped = Arc::new(AtomicBool::new(false));
        let source = CountingSource {
            reads: Arc::clone(&reads),
            dropped: Some(Arc::clone(&dropped)),
        };
        let buffered =
            AsyncPcmSource::new(Box::new(source), Duration::ZERO, Duration::from_millis(1));
        let expected_capacity = buffered.shared.capacity_samples;
        wait_until(Duration::from_secs(1), || {
            reads.load(Ordering::SeqCst) == expected_capacity
        });

        drop(buffered);

        wait_until(Duration::from_secs(1), || dropped.load(Ordering::SeqCst));
        assert!(dropped.load(Ordering::SeqCst));
    }

    fn wait_until(timeout: Duration, condition: impl Fn() -> bool) {
        let started = Instant::now();
        while !condition() && started.elapsed() < timeout {
            thread::sleep(Duration::from_millis(1));
        }
    }
}
