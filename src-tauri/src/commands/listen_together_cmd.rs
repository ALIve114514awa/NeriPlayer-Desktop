use crate::listen_together::protocol::*;
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
    let http = state.http();
    let url = format!("{}/api/rooms", base_url.trim_end_matches('/'));

    let body = LtCreateRoomRequest {
        user_uuid: user_uuid.clone(),
        nickname: nickname.clone(),
        initial_snapshot,
    };

    let resp = http
        .post(&url)
        .json(&body)
        .send()
        .await
        .map_err(|e| format!("HTTP error: {e}"))?;

    let room_resp: LtRoomResponse = resp.json().await.map_err(|e| format!("Parse error: {e}"))?;

    if room_resp.ok {
        let mut session = state.lt_session.lock();
        session.base_url = Some(base_url);
        session.room_id = room_resp.room_id.clone();
        session.token = room_resp.token.clone();
        session.ws_url = room_resp.ws_url.clone();
        session.user_uuid = user_uuid;
        session.nickname = nickname;
    }

    Ok(room_resp)
}

#[tauri::command]
pub async fn lt_join_room(
    base_url: String,
    room_id: String,
    user_uuid: String,
    nickname: String,
    state: State<'_, AppState>,
) -> Result<LtRoomResponse, String> {
    let http = state.http();
    let url = format!(
        "{}/api/rooms/{}/join",
        base_url.trim_end_matches('/'),
        room_id
    );

    let body = LtJoinRoomRequest {
        user_uuid: user_uuid.clone(),
        nickname: nickname.clone(),
    };

    let resp = http
        .post(&url)
        .json(&body)
        .send()
        .await
        .map_err(|e| format!("HTTP error: {e}"))?;

    let room_resp: LtRoomResponse = resp.json().await.map_err(|e| format!("Parse error: {e}"))?;

    if room_resp.ok {
        let mut session = state.lt_session.lock();
        session.base_url = Some(base_url);
        session.room_id = Some(room_id);
        session.token = room_resp.token.clone();
        session.ws_url = room_resp.ws_url.clone();
        session.user_uuid = user_uuid;
        session.nickname = nickname;
    }

    Ok(room_resp)
}

#[tauri::command]
pub async fn lt_get_room_state(
    base_url: String,
    room_id: String,
    state: State<'_, AppState>,
) -> Result<LtStateResponse, String> {
    let http = state.http();
    let token = state.lt_session.lock().token.clone().unwrap_or_default();
    let url = format!(
        "{}/api/rooms/{}/state",
        base_url.trim_end_matches('/'),
        room_id
    );

    let resp = http
        .get(&url)
        .header("Authorization", format!("Bearer {token}"))
        .send()
        .await
        .map_err(|e| format!("HTTP error: {e}"))?;

    resp.json::<LtStateResponse>()
        .await
        .map_err(|e| format!("Parse error: {e}"))
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
pub async fn lt_send_ping(state: State<'_, AppState>) -> Result<bool, String> {
    let ws_arc = state.lt_session.lock().ws_client.clone();
    let ws = ws_arc.lock().await;
    match ws.as_ref() {
        Some(client) => {
            client.send_ping()?;
            Ok(true)
        }
        None => Ok(false),
    }
}
