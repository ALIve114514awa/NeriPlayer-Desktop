use std::time::SystemTime;

fn main() {
    // 注入 BUILD_UUID 和 BUILD_TIMESTAMP 环境变量
    let uuid = uuid_v4();
    let timestamp = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| {
            let secs = d.as_secs();
            // 简单 ISO-like 格式: YYYY-MM-DD HH:MM:SS UTC
            let days = secs / 86400;
            let time_of_day = secs % 86400;
            let hours = time_of_day / 3600;
            let minutes = (time_of_day % 3600) / 60;
            let seconds = time_of_day % 60;
            // 简化的日期计算
            let (year, month, day) = days_to_ymd(days);
            format!("{:04}-{:02}-{:02} {:02}:{:02}:{:02} UTC", year, month, day, hours, minutes, seconds)
        })
        .unwrap_or_else(|_| "unknown".to_string());

    println!("cargo:rustc-env=BUILD_UUID={}", uuid);
    println!("cargo:rustc-env=BUILD_TIMESTAMP={}", timestamp);

    tauri_build::build()
}

/// 简易 UUID v4 生成（不依赖外部 crate）
fn uuid_v4() -> String {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};

    let mut hasher = DefaultHasher::new();
    SystemTime::now().hash(&mut hasher);
    std::process::id().hash(&mut hasher);
    let h1 = hasher.finish();

    let mut hasher2 = DefaultHasher::new();
    h1.hash(&mut hasher2);
    std::thread::current().id().hash(&mut hasher2);
    let h2 = hasher2.finish();

    let bytes = [
        (h1 >> 56) as u8, (h1 >> 48) as u8, (h1 >> 40) as u8, (h1 >> 32) as u8,
        (h1 >> 24) as u8, (h1 >> 16) as u8,
        ((h1 >> 8) as u8 & 0x0f) | 0x40, // version 4
        h1 as u8,
        ((h2 >> 56) as u8 & 0x3f) | 0x80, // variant 1
        (h2 >> 48) as u8,
        (h2 >> 40) as u8, (h2 >> 32) as u8, (h2 >> 24) as u8,
        (h2 >> 16) as u8, (h2 >> 8) as u8, h2 as u8,
    ];

    format!(
        "{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        bytes[0], bytes[1], bytes[2], bytes[3],
        bytes[4], bytes[5], bytes[6], bytes[7],
        bytes[8], bytes[9], bytes[10], bytes[11],
        bytes[12], bytes[13], bytes[14], bytes[15]
    )
}

/// 将自 epoch 以来的天数转换为 (年, 月, 日)
fn days_to_ymd(days: u64) -> (u64, u64, u64) {
    let mut y = 1970;
    let mut remaining = days;
    loop {
        let days_in_year = if is_leap(y) { 366 } else { 365 };
        if remaining < days_in_year { break; }
        remaining -= days_in_year;
        y += 1;
    }
    let months = if is_leap(y) {
        [31, 29, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31]
    } else {
        [31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31]
    };
    let mut m = 0;
    for days_in_month in months {
        if remaining < days_in_month { break; }
        remaining -= days_in_month;
        m += 1;
    }
    (y, m + 1, remaining + 1)
}

fn is_leap(y: u64) -> bool {
    (y % 4 == 0 && y % 100 != 0) || y % 400 == 0
}
