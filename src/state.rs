//! Shared application state for the fake receiver.

use std::collections::HashMap;
use std::time::Instant;

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
    /// Playback queue (drives QUEUE_NEXT / QUEUE_PREV navigation).
    pub queue: Vec<MediaInfo>,
    /// Index of the currently playing item in `queue`.
    pub queue_index: usize,
}

/// Global receiver state, shared between all connections.
#[derive(Debug)]
pub struct AppState {
    pub session: Option<Session>,
    /// Live connections bound to each session id (via CONNECT). A session is
    /// torn down only when this reaches 0, so playback survives a sender
    /// re-opening its transport (e.g. VLC auto-advancing to the next track).
    pub session_connections: HashMap<String, usize>,
    /// Monotonic counter bumped on every media LOAD. Used to debounce media
    /// STOP: VLC fires STOP+LOAD bursts, so a LOAD arriving shortly after a
    /// STOP cancels the stop (otherwise mpv's loadfile opening gets aborted).
    pub media_load_generation: u64,
    /// contentId + time of the most recent LOAD. Absorbs VLC's STOP+LOAD
    /// burst: repeated LOADs of the SAME contentId within ~1.5s are ignored so
    /// the burst does not re-issue `loadfile replace` and kill the stream
    /// (mpv log: loadfile -> audio ready -> 26ms later another loadfile ->
    /// EOF -> stream dies before any audio is heard).
    pub last_load: Option<(String, Instant)>,
    pub volume: f32,
    pub muted: bool,
    pub media_session_counter: u32,
}

impl Default for AppState {
    fn default() -> Self {
        Self {
            session: None,
            session_connections: HashMap::new(),
            media_load_generation: 0,
            last_load: None,
            volume: 1.0,
            muted: false,
            media_session_counter: 0,
        }
    }
}
