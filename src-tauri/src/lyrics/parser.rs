// LRC / YRC 歌词解析器
use serde::Serialize;
use regex::Regex;
use std::sync::OnceLock;

#[derive(Debug, Clone, Serialize)]
pub struct LyricLine {
    pub start_ms: u64,
    pub duration_ms: u64,
    pub text: String,
    pub translation: Option<String>,
    pub words: Vec<LyricWord>,
}

#[derive(Debug, Clone, Serialize)]
pub struct LyricWord {
    pub start_ms: u64,
    pub duration_ms: u64,
    pub text: String,
}

/// 自动检测格式并解析（对齐 Android parseNeteaseLyricsAuto）
pub fn parse_auto(content: &str) -> Vec<LyricLine> {
    static YRC_DETECT: OnceLock<Regex> = OnceLock::new();
    let re = YRC_DETECT.get_or_init(|| Regex::new(r"\[\d+,\s*\d+\]\(\d+,").unwrap());
    if re.is_match(content) {
        parse_yrc(content)
    } else {
        parse_lrc(content)
    }
}

fn yrc_word_times_are_relative(
    line_start_ms: u64,
    line_duration_ms: u64,
    words: &[LyricWord],
) -> bool {
    let Some(first_word_start) = words.iter().map(|w| w.start_ms).min() else {
        return false;
    };
    let Some(last_word_end) = words
        .iter()
        .map(|w| w.start_ms.saturating_add(w.duration_ms))
        .max()
    else {
        return false;
    };

    first_word_start < line_start_ms.saturating_sub(250)
        && last_word_end <= line_duration_ms.saturating_add(500)
}

/// 解析网易云 YRC 逐字歌词
/// 格式：[startMs,durationMs](wordStartMs,wordDurationMs,0)文字...
pub fn parse_yrc(content: &str) -> Vec<LyricLine> {
    static LINE_RE: OnceLock<Regex> = OnceLock::new();
    static WORD_RE: OnceLock<Regex> = OnceLock::new();
    let line_re = LINE_RE.get_or_init(|| Regex::new(r"\[(\d+),\s*(\d+)\](.+)").unwrap());
    let word_re = WORD_RE.get_or_init(|| Regex::new(r"\((\d+),\s*(\d+),\s*[-\d]+\)([^()\n\r]+)").unwrap());

    let mut lines: Vec<LyricLine> = Vec::new();

    for line in content.lines() {
        if let Some(caps) = line_re.captures(line) {
            let start_ms: u64 = caps[1].parse().unwrap_or(0);
            let duration_ms: u64 = caps[2].parse().unwrap_or(0);
            let rest = &caps[3];

            let mut words = Vec::new();
            let mut full_text = String::new();

            for wcap in word_re.captures_iter(rest) {
                let ws: u64 = wcap[1].parse().unwrap_or(0);
                let wd: u64 = wcap[2].parse().unwrap_or(0);
                let wt = wcap[3].to_string();
                full_text.push_str(&wt);
                words.push(LyricWord { start_ms: ws, duration_ms: wd, text: wt });
            }

            if full_text.trim().is_empty() { continue; }

            if yrc_word_times_are_relative(start_ms, duration_ms, &words) {
                for word in &mut words {
                    word.start_ms = start_ms.saturating_add(word.start_ms);
                }
            }

            lines.push(LyricLine {
                start_ms,
                duration_ms,
                text: full_text,
                translation: None,
                words,
            });
        }
    }

    lines
}

/// 解析标准 LRC 格式
pub fn parse_lrc(content: &str) -> Vec<LyricLine> {
    static LRC_RE: OnceLock<Regex> = OnceLock::new();
    let re = LRC_RE.get_or_init(|| Regex::new(r"\[(\d{2}):(\d{2})\.(\d{2,3})\](.*)").unwrap());
    let mut lines: Vec<LyricLine> = Vec::new();

    for line in content.lines() {
        if let Some(caps) = re.captures(line) {
            let min: u64 = caps[1].parse().unwrap_or(0);
            let sec: u64 = caps[2].parse().unwrap_or(0);
            let ms_str = &caps[3];
            let ms: u64 = if ms_str.len() == 2 {
                ms_str.parse::<u64>().unwrap_or(0) * 10
            } else {
                ms_str.parse().unwrap_or(0)
            };
            let text = caps[4].trim().to_string();
            if text.is_empty() { continue; }
            let start_ms = min * 60000 + sec * 1000 + ms;
            lines.push(LyricLine {
                start_ms,
                duration_ms: 0,
                text,
                translation: None,
                words: Vec::new(),
            });
        }
    }

    // 计算每行持续时间
    for i in 0..lines.len() {
        if i + 1 < lines.len() {
            lines[i].duration_ms = lines[i + 1].start_ms - lines[i].start_ms;
        } else {
            lines[i].duration_ms = 5000;
        }
    }

    lines
}

/// 合并翻译到已有歌词行（对齐 Android 450ms 容差）
pub fn merge_translation(lines: &mut [LyricLine], translation_lrc: &str) {
    let trans = parse_lrc(translation_lrc);
    for tl in &trans {
        if let Some(line) = lines.iter_mut()
            .min_by_key(|l| (l.start_ms as i64 - tl.start_ms as i64).unsigned_abs())
        {
            if (line.start_ms as i64 - tl.start_ms as i64).unsigned_abs() < 450 {
                line.translation = Some(tl.text.clone());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::parse_yrc;

    #[test]
    fn parse_yrc_normalizes_relative_word_times() {
        let lines = parse_yrc("[10000,2000](0,500,0)你(500,500,0)好");

        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].words[0].start_ms, 10000);
        assert_eq!(lines[0].words[1].start_ms, 10500);
    }

    #[test]
    fn parse_yrc_keeps_absolute_word_times() {
        let lines = parse_yrc("[10000,2000](10000,500,0)你(10500,500,0)好");

        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].words[0].start_ms, 10000);
        assert_eq!(lines[0].words[1].start_ms, 10500);
    }
}
