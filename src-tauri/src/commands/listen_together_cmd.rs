use crate::listen_together::protocol::*;
use crate::listen_together::session::LtSessionUpdate;
use crate::listen_together::ws_client::LtWsClient;
use crate::state::AppState;
use tauri::{AppHandle, State};

#[tauri::command]
pub async fn lt_create_room(
    base_url: String,
    user_uuid: String,
    nickname: String,
    initial_snapshot: LtInitialSnapshot,
    state: State<'_, AppState>,
) -> Result<LtRoomResponse, String> {
    let http = state.transport("listen-together");
    let url = format!("{}/api/rooms", base_url.trim_end_matches('/'));

    let body = LtCreateRoomRequest {
        user_uuid: user_uuid.clone(),
        nickname: nickname.clone(),
        initial_snapshot,
    };

    let resp = http
        // 建房是非幂等写操作：读超时后重发可能创建两个房间，只有连接尚未建立
        // 时才允许换代理重试
        .send_once(|client| client.post(&url).json(&body))
        .await
        .map_err(|e| format!("HTTP error: {e}"))?;

    // 必须先判状态码：房间服务返回 4xx/5xx 时错误体同样可能是 JSON，
    // 直接解析会得到 ok=false 却没有原因，用户只看到一句 Parse error
    let room_resp: LtRoomResponse =
        crate::api::transport::parse_json_response(resp, "listen-together create room")
            .await
            .map_err(|e| e.to_string())?;

    if room_resp.ok {
        let mut session = state.lt_session.lock();
        session.update_room(LtSessionUpdate {
            base_url,
            room_id: room_resp.room_id.clone(),
            user_uuid,
            nickname,
            token: room_resp.token.clone(),
            ws_url: room_resp.ws_url.clone(),
            member_secret: room_resp.member_secret.clone(),
            join_secret: room_resp.join_secret.clone(),
        });
    }

    Ok(room_resp)
}

#[tauri::command]
pub async fn lt_join_room(
    base_url: String,
    room_id: String,
    user_uuid: String,
    nickname: String,
    member_secret: Option<String>,
    join_secret: Option<String>,
    state: State<'_, AppState>,
) -> Result<LtRoomResponse, String> {
    let http = state.transport("listen-together");
    let url = format!(
        "{}/api/rooms/{}/join",
        base_url.trim_end_matches('/'),
        room_id
    );

    let credentials = state
        .lt_session
        .lock()
        .credentials_for_join(&base_url, &room_id, &user_uuid);
    let body = LtJoinRoomRequest {
        user_uuid: user_uuid.clone(),
        nickname: nickname.clone(),
        member_secret: member_secret.or(credentials.member_secret.clone()),
        join_secret: join_secret.or(credentials.join_secret.clone()),
    };

    let resp = http
        // 加入请求同样可能产生服务端成员记录，避免请求体已发出后重复提交
        .send_once(|client| {
            let request = client.post(&url).json(&body);
            match credentials.token.as_deref() {
                Some(token) => request.bearer_auth(token),
                None => request,
            }
        })
        .await
        .map_err(|e| format!("HTTP error: {e}"))?;

    let room_resp: LtRoomResponse =
        crate::api::transport::parse_json_response(resp, "listen-together join room")
            .await
            .map_err(|e| e.to_string())?;

    if room_resp.ok {
        let mut session = state.lt_session.lock();
        session.update_room(LtSessionUpdate {
            base_url,
            room_id: room_resp.room_id.clone().or(Some(room_id)),
            user_uuid,
            nickname,
            token: room_resp.token.clone(),
            ws_url: room_resp.ws_url.clone(),
            member_secret: room_resp.member_secret.clone(),
            join_secret: room_resp.join_secret.clone(),
        });
    }

    Ok(room_resp)
}

#[tauri::command]
pub async fn lt_get_room_state(
    base_url: String,
    room_id: String,
    state: State<'_, AppState>,
) -> Result<LtStateResponse, String> {
    let http = state.transport("listen-together");
    let token = state
        .lt_session
        .lock()
        .token_for_room_state(&base_url, &room_id);
    let url = format!(
        "{}/api/rooms/{}/state",
        base_url.trim_end_matches('/'),
        room_id
    );

    let resp = http
        .send(|client| {
            let request = client.get(&url);
            match token.as_deref() {
                Some(token) => request.bearer_auth(token),
                None => request,
            }
        })
        .await
        .map_err(|e| format!("HTTP error: {e}"))?;

    crate::api::transport::parse_json_response(resp, "listen-together room state")
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn lt_connect_ws(
    ws_url: String,
    app_handle: AppHandle,
    state: State<'_, AppState>,
) -> Result<(), String> {
    // 先断开旧连接
    {
        let ws_arc = state.lt_session.lock().ws_client.clone();
        let mut ws = ws_arc.lock().await;
        if let Some(old) = ws.take() {
            old.disconnect().await;
        }
    }

    let client = LtWsClient::connect(&ws_url, app_handle).await?;
    let ws_arc = state.lt_session.lock().ws_client.clone();
    *ws_arc.lock().await = Some(client);

    Ok(())
}

#[tauri::command]
pub async fn lt_disconnect_ws(state: State<'_, AppState>) -> Result<(), String> {
    let ws_arc = state.lt_session.lock().ws_client.clone();
    let mut ws = ws_arc.lock().await;
    if let Some(client) = ws.take() {
        client.disconnect().await;
    }
    state.lt_session.lock().reset();
    Ok(())
}

#[tauri::command]
pub async fn lt_send_event(event: LtEvent, state: State<'_, AppState>) -> Result<bool, String> {
    let ws_arc = state.lt_session.lock().ws_client.clone();
    let ws = ws_arc.lock().await;
    match ws.as_ref() {
        Some(client) => {
            let json =
                serde_json::to_string(&event).map_err(|e| format!("Serialize error: {e}"))?;
            client.send(&json)?;
            Ok(true)
        }
        None => Ok(false),
    }
}

#[tauri::command]
pub async fn lt_send_ping(t: Option<i64>, state: State<'_, AppState>) -> Result<bool, String> {
    let ws_arc = state.lt_session.lock().ws_client.clone();
    let ws = ws_arc.lock().await;
    match ws.as_ref() {
        Some(client) => {
            client.send_ping(t)?;
            Ok(true)
        }
        None => Ok(false),
    }
}
