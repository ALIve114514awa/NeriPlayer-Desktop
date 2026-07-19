// 系统媒体会话集成 — Windows SMTC / Linux MPRIS
// 通过 souvlaki 库实现跨平台媒体键控制和系统通知中心展示

use souvlaki::{
    MediaControlEvent, MediaControls, MediaMetadata, MediaPlayback, MediaPosition, PlatformConfig,
};
use std::sync::mpsc;
use std::time::Duration;

/// 媒体会话控制器，管理 SMTC/MPRIS 状态
pub struct MediaSessionController {
    cmd_tx: mpsc::Sender<MediaSessionCmd>,
}

enum MediaSessionCmd {
    UpdateMetadata {
        title: String,
        artist: String,
        album: String,
        cover_url: Option<String>,
        duration_ms: u64,
    },
    UpdatePlayback {
        is_playing: bool,
        position_ms: u64,
    },
    Stop,
}

/// 媒体事件回调类型
#[derive(Debug, Clone)]
pub enum MediaAction {
    Play,
    Pause,
    Toggle,
    Next,
    Previous,
    SeekTo(u64), // position_ms
}

impl MediaSessionController {
    /// 创建媒体会话控制器
    /// - `hwnd`: Windows 窗口句柄（Windows 必需）
    /// - `action_tx`: 媒体事件回调通道
    pub fn new(
        hwnd: Option<*mut std::ffi::c_void>,
        action_tx: mpsc::Sender<MediaAction>,
    ) -> Option<Self> {
        let (cmd_tx, cmd_rx) = mpsc::channel::<MediaSessionCmd>();
        // 将 *mut c_void 转为 usize 以满足 Send 要求（线程安全由调用方保证）
        let hwnd_usize: Option<usize> = hwnd.map(|p| p as usize);

        // 在独立线程中运行 MediaControls（Windows SMTC 需要）
        std::thread::Builder::new()
            .name("media-session".into())
            .spawn(move || {
                let config = PlatformConfig {
                    dbus_name: "neri_player",
                    display_name: "Neri Player",
                    hwnd: hwnd_usize.map(|u| u as *mut std::ffi::c_void),
                };

                let mut controls = match MediaControls::new(config) {
                    Ok(c) => c,
                    Err(e) => {
                        log::error!(target: "media-session", "Failed to create MediaControls: {:?}", e);
                        return;
                    }
                };

                // 注册事件回调
                let tx = action_tx;
                if let Err(e) = controls.attach(move |event: MediaControlEvent| {
                    let action = match event {
                        MediaControlEvent::Play => Some(MediaAction::Play),
                        MediaControlEvent::Pause => Some(MediaAction::Pause),
                        MediaControlEvent::Toggle => Some(MediaAction::Toggle),
                        MediaControlEvent::Next => Some(MediaAction::Next),
                        MediaControlEvent::Previous => Some(MediaAction::Previous),
                        MediaControlEvent::SetPosition(pos) => {
                            Some(MediaAction::SeekTo(pos.0.as_millis() as u64))
                        }
                        _ => None,
                    };
                    if let Some(a) = action {
                        let _ = tx.send(a);
                    }
                }) {
                    log::error!(target: "media-session", "Failed to attach handler: {:?}", e);
                    return;
                }

                log::info!(target: "media-session", "MediaControls initialized");

                // 命令循环
                loop {
                    match cmd_rx.recv_timeout(Duration::from_millis(100)) {
                        Ok(MediaSessionCmd::UpdateMetadata {
                            title,
                            artist,
                            album,
                            cover_url,
                            duration_ms,
                        }) => {
                            let dur = if duration_ms > 0 {
                                Some(Duration::from_millis(duration_ms))
                            } else {
                                None
                            };
                            let _ = controls.set_metadata(MediaMetadata {
                                title: Some(&title),
                                artist: Some(&artist),
                                album: Some(&album),
                                cover_url: cover_url.as_deref(),
                                duration: dur,
                            });
                        }
                        Ok(MediaSessionCmd::UpdatePlayback {
                            is_playing,
                            position_ms,
                        }) => {
                            let progress =
                                Some(MediaPosition(Duration::from_millis(position_ms)));
                            let playback = if is_playing {
                                MediaPlayback::Playing { progress }
                            } else {
                                MediaPlayback::Paused { progress }
                            };
                            let _ = controls.set_playback(playback);
                        }
                        Ok(MediaSessionCmd::Stop) => {
                            let _ = controls.set_playback(MediaPlayback::Stopped);
                        }
                        Err(mpsc::RecvTimeoutError::Timeout) => continue,
                        Err(mpsc::RecvTimeoutError::Disconnected) => {
                            log::warn!(target: "media-session", "channel disconnected, exiting");
                            break;
                        }
                    }
                }
            })
            .ok()?;

        Some(Self { cmd_tx })
    }

    /// 更新曲目元数据
    pub fn update_metadata(
        &self,
        title: &str,
        artist: &str,
        album: &str,
        cover_url: Option<&str>,
        duration_ms: u64,
    ) {
        let _ = self.cmd_tx.send(MediaSessionCmd::UpdateMetadata {
            title: title.to_string(),
            artist: artist.to_string(),
            album: album.to_string(),
            cover_url: cover_url.map(|s| s.to_string()),
            duration_ms,
        });
    }

    /// 更新播放状态和位置
    pub fn update_playback(&self, is_playing: bool, position_ms: u64) {
        let _ = self.cmd_tx.send(MediaSessionCmd::UpdatePlayback {
            is_playing,
            position_ms,
        });
    }

    /// 停止媒体会话
    pub fn stop(&self) {
        let _ = self.cmd_tx.send(MediaSessionCmd::Stop);
    }
}
