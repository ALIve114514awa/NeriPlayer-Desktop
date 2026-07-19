//! 统一日志基础设施
//!
//! 前后端共用同一套日志出口：后端 `log` 宏与前端 `@tauri-apps/plugin-log`
//! 转发的记录都经由此处的 `tauri-plugin-log` 插件输出。每条日志统一带
//! `时间 [作用域] [级别] 内容` 格式；作用域取自 `log` 记录的 target
//! （模块路径或调用方显式指定的语义 target，如 `player`、`sync`）。
//!
//! 文件持久化受设置项控制：`log_to_file` 为真时追加 Folder target，
//! 落到 `<data_dir>/NeriPlayer/logs/`。受插件能力限制，文件 target 只能
//! 在启动时构建，运行时切换开关需重启生效；日志级别则可运行时调整。

use std::path::PathBuf;

use log::LevelFilter;
use tauri::Runtime;
use tauri_plugin_log::{RotationStrategy, Target, TargetKind, TimezoneStrategy};

/// 日志文件所在目录：与项目其余数据一致，统一落到 `<data_dir>/NeriPlayer/logs`
const LOG_SUBDIR: &str = "NeriPlayer";
const LOG_DIR_NAME: &str = "logs";
/// 日志文件基名（插件自动追加 `.log` 与滚动日期后缀）
const LOG_FILE_NAME: &str = "neri-player";
/// 单文件大小上限（字节），超过后按滚动策略切分
const MAX_LOG_FILE_SIZE: u128 = 8 * 1024 * 1024;
/// 保留最近的日志文件个数
const KEEP_LOG_FILES: usize = 8;

/// 解析设置中的字符串级别为 `LevelFilter`；无法识别时回退 `Info`
pub fn parse_level(level: &str) -> LevelFilter {
    match level.trim().to_ascii_lowercase().as_str() {
        "off" => LevelFilter::Off,
        "error" => LevelFilter::Error,
        "warn" => LevelFilter::Warn,
        "info" => LevelFilter::Info,
        "debug" => LevelFilter::Debug,
        "trace" => LevelFilter::Trace,
        _ => LevelFilter::Info,
    }
}

/// 日志文件目录（不保证已创建，插件写入前会自行创建）
pub fn log_dir() -> PathBuf {
    let mut path = dirs_next::data_dir().unwrap_or_else(|| PathBuf::from("."));
    path.push(LOG_SUBDIR);
    path.push(LOG_DIR_NAME);
    path
}

/// 应用标识（与 tauri.conf.json 的 identifier 保持一致）
const APP_IDENTIFIER: &str = "moe.ouom.neriplayer.desktop";
/// tauri-plugin-store 的设置文件名
const SETTINGS_STORE_FILE: &str = "settings.json";
/// settings.json 中的顶层键
const SETTINGS_STORE_KEY: &str = "appSettings";

/// 启动期日志配置：`(是否写文件, 级别)`
pub struct BootstrapConfig {
    pub log_to_file: bool,
    pub level: LevelFilter,
}

impl Default for BootstrapConfig {
    fn default() -> Self {
        // 默认不落文件、级别 Info，与 AppSettings 默认值保持一致
        Self {
            log_to_file: false,
            level: LevelFilter::Info,
        }
    }
}

/// 在插件初始化前读取持久化设置，决定是否写文件及日志级别。
///
/// 此时还没有 Tauri app handle，因此直接按 plugin-store 的落盘路径
/// （`config_dir/{identifier}/settings.json`）解析。任何缺失或解析失败
/// 都回退到默认配置，绝不 panic。
pub fn load_bootstrap_config() -> BootstrapConfig {
    let mut path = match dirs_next::config_dir() {
        Some(p) => p,
        None => return BootstrapConfig::default(),
    };
    path.push(APP_IDENTIFIER);
    path.push(SETTINGS_STORE_FILE);

    let content = match std::fs::read_to_string(&path) {
        Ok(c) => c,
        Err(_) => return BootstrapConfig::default(),
    };
    let root: serde_json::Value = match serde_json::from_str(&content) {
        Ok(v) => v,
        Err(_) => return BootstrapConfig::default(),
    };
    let settings = &root[SETTINGS_STORE_KEY];

    let defaults = BootstrapConfig::default();
    let log_to_file = settings["logToFile"]
        .as_bool()
        .unwrap_or(defaults.log_to_file);
    let level = settings["logLevel"]
        .as_str()
        .map(parse_level)
        .unwrap_or(defaults.level);

    BootstrapConfig {
        log_to_file,
        level,
    }
}

/// 统一日志格式：`2026-07-19 12:00:00.123 [scope] [LEVEL] message`
fn format_record(
    out: tauri_plugin_log::fern::FormatCallback,
    message: &std::fmt::Arguments,
    record: &log::Record,
) {
    // 使用本地时区的墙钟时间，毫秒精度，便于与用户操作时间对齐
    let ts = chrono::Local::now().format("%Y-%m-%d %H:%M:%S%.3f");
    out.finish(format_args!(
        "{} [{}] [{}] {}",
        ts,
        record.target(),
        record.level(),
        message
    ));
}

/// 级别对应的 ANSI 前景色（仅用于 stdout 彩色输出）
fn level_ansi(level: log::Level) -> &'static str {
    match level {
        log::Level::Error => "\x1b[31m", // 红
        log::Level::Warn => "\x1b[33m",  // 黄
        log::Level::Info => "\x1b[32m",  // 绿
        log::Level::Debug => "\x1b[36m", // 青
        log::Level::Trace => "\x1b[90m", // 亮黑（灰）
    }
}

/// 彩色日志格式：时间戳灰、作用域青、级别按级配色。
/// 仅用于 stdout；写文件时改用 `format_record` 以免 ANSI 转义污染日志文件。
fn format_record_colored(
    out: tauri_plugin_log::fern::FormatCallback,
    message: &std::fmt::Arguments,
    record: &log::Record,
) {
    const RESET: &str = "\x1b[0m";
    const DIM: &str = "\x1b[90m";
    const SCOPE: &str = "\x1b[36m";
    let ts = chrono::Local::now().format("%Y-%m-%d %H:%M:%S%.3f");
    out.finish(format_args!(
        "{dim}{ts}{reset} {scope}[{target}]{reset} {lc}[{level}]{reset} {msg}",
        dim = DIM,
        reset = RESET,
        scope = SCOPE,
        ts = ts,
        target = record.target(),
        lc = level_ansi(record.level()),
        level = record.level(),
        msg = message,
    ));
}

/// 构建日志插件。
///
/// - `log_to_file`：为真时追加文件 target，否则仅输出到 stdout
/// - `level`：全局最低日志级别
pub fn build_plugin<R: Runtime>(
    log_to_file: bool,
    level: LevelFilter,
) -> tauri::plugin::TauriPlugin<R> {
    let mut targets = vec![Target::new(TargetKind::Stdout)];
    if log_to_file {
        targets.push(Target::new(TargetKind::Folder {
            path: log_dir(),
            file_name: Some(LOG_FILE_NAME.to_string()),
        }));
    }

    let builder = tauri_plugin_log::Builder::new()
        .level(level)
        // 降噪：第三方库的 target 前缀过滤到 Warn 以上，避免刷屏
        .level_for("hyper", LevelFilter::Warn)
        .level_for("reqwest", LevelFilter::Warn)
        .level_for("rustls", LevelFilter::Warn)
        .level_for("tao", LevelFilter::Warn)
        .level_for("wry", LevelFilter::Warn)
        .timezone_strategy(TimezoneStrategy::UseLocal)
        .max_file_size(MAX_LOG_FILE_SIZE)
        .rotation_strategy(RotationStrategy::KeepSome(KEEP_LOG_FILES))
        .clear_format();

    // 写文件时用纯文本（避免 ANSI 转义落盘），仅 stdout 时用彩色输出
    let builder = if log_to_file {
        builder.format(format_record)
    } else {
        builder.format(format_record_colored)
    };

    builder.targets(targets).build()
}
