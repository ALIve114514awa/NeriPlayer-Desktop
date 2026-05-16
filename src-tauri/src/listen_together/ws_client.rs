use futures_util::{SinkExt, StreamExt};
use std::sync::Arc;
use tauri::{AppHandle, Emitter};
use tokio::sync::mpsc;
use tokio::sync::Mutex as TokioMutex;
use tokio_tungstenite::tungstenite::Message;

use super::protocol::LtSocketEnvelope;

/// WebSocket 客户端：管理与一起听服务器的连接
pub struct LtWsClient {
    tx: mpsc::UnboundedSender<String>,
    shutdown: mpsc::Sender<()>,
}

impl LtWsClient {
    /// 建立 WebSocket 连接并启动读写循环
    pub async fn connect(ws_url: &str, app_handle: AppHandle) -> Result<Self, String> {
        let (ws_stream, _) = tokio_tungstenite::connect_async(ws_url)
            .await
            .map_err(|e| format!("WebSocket connect failed: {e}"))?;

        let (mut ws_write, mut ws_read) = ws_stream.split();

        // 写通道：前端 → WS
        let (tx, mut rx) = mpsc::unbounded_channel::<String>();
        // 关闭信号
        let (shutdown_tx, mut shutdown_rx) = mpsc::channel::<()>(1);

        let _ = app_handle.emit("lt:connected", serde_json::json!({}));

        // 写循环
        let handle_w = app_handle.clone();
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    msg = rx.recv() => {
                        match msg {
                            Some(text) => {
                                if let Err(e) = ws_write.send(Message::Text(text.into())).await {
                                    eprintln!("[lt-ws] write error: {e}");
                                    break;
                                }
                            }
                            None => break,
                        }
                    }
                    _ = shutdown_rx.recv() => {
                        let _ = ws_write.close().await;
                        break;
                    }
                }
            }
            let _ = handle_w.emit(
                "lt:disconnected",
                serde_json::json!({
                    "code": 1000, "reason": "client_closed"
                }),
            );
        });

        // 读循环
        let handle_r = app_handle.clone();
        tokio::spawn(async move {
            while let Some(result) = ws_read.next().await {
                match result {
                    Ok(Message::Text(text)) => {
                        // 尝试解析为 envelope 并转发给前端
                        match serde_json::from_str::<LtSocketEnvelope>(&text) {
                            Ok(envelope) => {
                                let _ = handle_r.emit("lt:message", &envelope);
                            }
                            Err(e) => {
                                eprintln!("[lt-ws] parse error: {e}, raw: {text}");
                            }
                        }
                    }
                    Ok(Message::Close(frame)) => {
                        let (code, reason) = frame
                            .map(|f| (f.code.into(), f.reason.to_string()))
                            .unwrap_or((1000u16, "closed".to_string()));
                        let _ = handle_r.emit(
                            "lt:disconnected",
                            serde_json::json!({
                                "code": code, "reason": reason
                            }),
                        );
                        break;
                    }
                    Ok(Message::Ping(data)) => {
                        // tungstenite 自动回 pong，忽略
                        let _ = data;
                    }
                    Err(e) => {
                        let _ = handle_r.emit(
                            "lt:disconnected",
                            serde_json::json!({
                                "code": 1006, "reason": format!("read error: {e}")
                            }),
                        );
                        break;
                    }
                    _ => {}
                }
            }
        });

        Ok(Self {
            tx,
            shutdown: shutdown_tx,
        })
    }

    /// 发送事件到 WebSocket
    pub fn send(&self, json: &str) -> Result<(), String> {
        self.tx
            .send(json.to_string())
            .map_err(|e| format!("send failed: {e}"))
    }

    /// 发送 ping
    pub fn send_ping(&self) -> Result<(), String> {
        self.send(r#"{"type":"ping"}"#)
    }

    /// 断开连接
    pub async fn disconnect(&self) {
        let _ = self.shutdown.send(()).await;
    }
}

/// 全局 WS 客户端引用（存在 AppState 中）
pub type SharedWsClient = Arc<TokioMutex<Option<LtWsClient>>>;
