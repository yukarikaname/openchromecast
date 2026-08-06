//! Shared application state for the fake receiver.

/// Metadata for the currently loaded media (echoed back in `MEDIA_STATUS`).
#[derive(Clone, Debug)]
pub struct MediaInfo {
    pub content_id: String,
    pub content_type: String,
    pub stream_type: String,
}

/// A running receiver application session.
#[derive(Clone, Debug)]
pub struct Session {
    /// Session id; doubles as the transport id in Cast V2.
    pub id: String,
    pub app_id: String,
    pub display_name: String,
    /// Namespaces the app announces to the sender.
    pub namespaces: Vec<String>,
    pub status_text: String,
    pub media_session_id: u32,
    pub media: Option<MediaInfo>,
}

/// Global receiver state, shared between all connections.
#[derive(Debug)]
pub struct AppState {
    pub session: Option<Session>,
    pub volume: f32,
    pub muted: bool,
    pub media_session_counter: u32,
}

impl Default for AppState {
    fn default() -> Self {
        Self {
            session: None,
            volume: 1.0,
            muted: false,
            media_session_counter: 0,
        }
    }
}
