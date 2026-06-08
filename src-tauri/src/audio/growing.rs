use std::io::{self, Read, Seek, SeekFrom};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Condvar, Mutex,
};
use std::time::{Duration, Instant};

#[derive(Clone)]
pub struct GrowingAudioBuffer {
    inner: Arc<GrowingAudioInner>,
}

struct GrowingAudioInner {
    state: Mutex<GrowingAudioState>,
    cv: Condvar,
    aborted: AtomicBool,
}

struct GrowingAudioState {
    data: Vec<u8>,
    complete: bool,
    error: Option<String>,
    total_len: Option<u64>,
}

#[derive(Clone)]
pub struct GrowingAudioReader {
    inner: Arc<GrowingAudioInner>,
    pos: u64,
}

impl GrowingAudioBuffer {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(GrowingAudioInner {
                state: Mutex::new(GrowingAudioState {
                    data: Vec::new(),
                    complete: false,
                    error: None,
                    total_len: None,
                }),
                cv: Condvar::new(),
                aborted: AtomicBool::new(false),
            }),
        }
    }

    pub fn reader(&self) -> GrowingAudioReader {
        GrowingAudioReader {
            inner: self.inner.clone(),
            pos: 0,
        }
    }

    pub fn set_total_len(&self, total_len: Option<u64>) {
        if let Ok(mut state) = self.inner.state.lock() {
            state.total_len = total_len;
        }
    }

    pub fn append(&self, bytes: &[u8]) {
        if bytes.is_empty() || self.is_aborted() {
            return;
        }
        if let Ok(mut state) = self.inner.state.lock() {
            state.data.extend_from_slice(bytes);
            self.inner.cv.notify_all();
        }
    }

    pub fn finish(&self) {
        if let Ok(mut state) = self.inner.state.lock() {
            state.complete = true;
            self.inner.cv.notify_all();
        }
    }

    pub fn fail(&self, message: String) {
        if let Ok(mut state) = self.inner.state.lock() {
            state.error = Some(message);
            state.complete = true;
            self.inner.cv.notify_all();
        }
    }

    pub fn abort(&self) {
        self.inner.aborted.store(true, Ordering::SeqCst);
        if let Ok(mut state) = self.inner.state.lock() {
            state.complete = true;
            self.inner.cv.notify_all();
        }
    }

    pub fn is_aborted(&self) -> bool {
        self.inner.aborted.load(Ordering::SeqCst)
    }

    pub fn wait_for_buffer(&self, min_bytes: usize, timeout: Duration) -> Result<usize, String> {
        let deadline = Instant::now() + timeout;
        let mut state = self
            .inner
            .state
            .lock()
            .map_err(|_| "stream lock poisoned".to_string())?;
        loop {
            if let Some(err) = &state.error {
                return Err(err.clone());
            }
            if self.is_aborted() {
                return Err("stream aborted".to_string());
            }
            if state.data.len() >= min_bytes || (state.complete && !state.data.is_empty()) {
                return Ok(state.data.len());
            }
            if state.complete {
                return Err("empty audio stream".to_string());
            }

            let now = Instant::now();
            if now >= deadline {
                return Err(format!(
                    "stream startup timeout: buffered {} bytes, need {} bytes",
                    state.data.len(),
                    min_bytes
                ));
            }
            let wait_for = deadline.saturating_duration_since(now);
            let (next_state, timeout_result) = self
                .inner
                .cv
                .wait_timeout(state, wait_for)
                .map_err(|_| "stream lock poisoned".to_string())?;
            state = next_state;
            if timeout_result.timed_out() {
                return Err(format!(
                    "stream startup timeout: buffered {} bytes, need {} bytes",
                    state.data.len(),
                    min_bytes
                ));
            }
        }
    }
}

impl GrowingAudioReader {
    pub fn abort(&self) {
        self.inner.aborted.store(true, Ordering::SeqCst);
        self.inner.cv.notify_all();
    }
}

impl Read for GrowingAudioReader {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        if out.is_empty() {
            return Ok(0);
        }

        let mut state = self
            .inner
            .state
            .lock()
            .map_err(|_| io::Error::new(io::ErrorKind::Other, "stream lock poisoned"))?;
        loop {
            if self.inner.aborted.load(Ordering::SeqCst) {
                return Ok(0);
            }
            if let Some(err) = &state.error {
                return Err(io::Error::new(io::ErrorKind::Other, err.clone()));
            }

            let available = state.data.len() as u64;
            if self.pos < available {
                let start = self.pos as usize;
                let len = out.len().min(state.data.len().saturating_sub(start));
                out[..len].copy_from_slice(&state.data[start..start + len]);
                self.pos += len as u64;
                return Ok(len);
            }

            if state.complete {
                return Ok(0);
            }

            state = self
                .inner
                .cv
                .wait(state)
                .map_err(|_| io::Error::new(io::ErrorKind::Other, "stream lock poisoned"))?;
        }
    }
}

impl Seek for GrowingAudioReader {
    fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
        let state = self
            .inner
            .state
            .lock()
            .map_err(|_| io::Error::new(io::ErrorKind::Other, "stream lock poisoned"))?;

        let next = match pos {
            SeekFrom::Start(offset) => offset as i128,
            SeekFrom::Current(offset) => self.pos as i128 + offset as i128,
            SeekFrom::End(offset) => {
                let len = state
                    .total_len
                    .or_else(|| state.complete.then_some(state.data.len() as u64));
                match len {
                    Some(len) => len as i128 + offset as i128,
                    None => {
                        return Err(io::Error::new(
                            io::ErrorKind::Unsupported,
                            "stream length is not known yet",
                        ));
                    }
                }
            }
        };

        if next < 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid seek before start",
            ));
        }
        self.pos = next as u64;
        Ok(self.pos)
    }
}
