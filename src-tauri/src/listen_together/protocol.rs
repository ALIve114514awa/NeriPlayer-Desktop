use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LtTrack {
    pub stable_key: String,
    pub channel_id: String,
    pub audio_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sub_audio_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub playlist_context_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub media_uri: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stream_url: Option<String>,
    pub name: String,
    pub artist: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub album: Option<String>,
    #[serde(default)]
    pub duration_ms: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cover_url: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LtRoomSettings {
    #[serde(default = "default_true")]
    pub allow_member_control: bool,
    #[serde(default = "default_true")]
    pub auto_pause_on_member_change: bool,
    #[serde(default = "default_true")]
    pub share_audio_links: bool,
}

fn default_true() -> bool {
    true
}

impl Default for LtRoomSettings {
    fn default() -> Self {
        Self {
            allow_member_control: true,
            auto_pause_on_member_change: true,
            share_audio_links: true,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LtMember {
    #[serde(default)]
    pub user_uuid: String,
    #[serde(default)]
    pub nickname: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user_id: Option<String>,
    pub role: String,
    pub joined_at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LtPlaybackState {
    #[serde(default = "default_paused")]
    pub state: String,
    #[serde(default)]
    pub base_position_ms: i64,
    #[serde(default)]
    pub base_timestamp_ms: i64,
    #[serde(default = "default_rate")]
    pub playback_rate: f64,
}

fn default_paused() -> String {
    "paused".to_string()
}
fn default_rate() -> f64 {
    1.0
}

impl Default for LtPlaybackState {
    fn default() -> Self {
        Self {
            state: "paused".to_string(),
            base_position_ms: 0,
            base_timestamp_ms: 0,
            playback_rate: 1.0,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LtRoomState {
    pub room_id: String,
    pub version: i64,
    #[serde(default = "default_schema")]
    pub schema_version: i32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub controller_user_uuid: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub controller_user_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub controller_heartbeat_at: Option<i64>,
    #[serde(default)]
    pub settings: LtRoomSettings,
    #[serde(default)]
    pub members: Vec<LtMember>,
    #[serde(default)]
    pub queue: Vec<LtTrack>,
    #[serde(default)]
    pub current_index: i32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub track: Option<LtTrack>,
    #[serde(default)]
    pub playback: LtPlaybackState,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub controller_offline_since: Option<i64>,
    #[serde(default = "default_active")]
    pub room_status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub closed_reason: Option<String>,
    #[serde(default)]
    pub updated_at: i64,
}

fn default_schema() -> i32 {
    1
}
fn default_active() -> String {
    "active".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LtCause {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user_uuid: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub nickname: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub event_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub r#type: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LtInitialSnapshot {
    #[serde(default)]
    pub queue: Vec<LtTrack>,
    #[serde(default)]
    pub current_index: i32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub track: Option<LtTrack>,
    #[serde(default)]
    pub settings: LtRoomSettings,
    #[serde(default)]
    pub is_playing: bool,
    #[serde(default)]
    pub position_ms: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LtCreateRoomRequest {
    pub user_uuid: String,
    pub nickname: String,
    pub initial_snapshot: LtInitialSnapshot,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LtJoinRoomRequest {
    pub user_uuid: String,
    pub nickname: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LtRoomResponse {
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub room_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user_uuid: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub nickname: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    #[serde(default)]
    pub auto_pause_on_join: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub token: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub state: Option<LtRoomState>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ws_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LtStateResponse {
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub state: Option<LtRoomState>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expected_position_ms: Option<i64>,
    #[serde(default)]
    pub auto_pause_on_join: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LtEvent {
    pub r#type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub event_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub client_time_ms: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub position_ms: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub current_index: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub track: Option<LtTrack>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub queue: Option<Vec<LtTrack>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub room_settings: Option<LtRoomSettings>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub should_play: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub state: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request_track_stable_key: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LtSocketEnvelope {
    pub r#type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user_uuid: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub nickname: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    #[serde(default)]
    pub auto_pause_on_join: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub state: Option<LtRoomState>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expected_position_ms: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub now_ms: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ok: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub room_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub caused_by: Option<LtCause>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub track: Option<LtTrack>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub queue: Option<Vec<LtTrack>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub position_ms: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub current_index: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request_track_stable_key: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub should_play: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub state_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub client_time_ms: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request_sequence: Option<i64>,
}
