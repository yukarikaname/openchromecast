//! Player abstraction: a small actor that receives playback commands and
//! publishes a status snapshot.

pub mod mpv;
pub mod vlc;

use anyhow::Result;
use std::sync::Arc;
use tokio::sync::{Mutex, mpsc, oneshot};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlayerState {
    Idle,
    Loading,
    Buffering,
    Playing,
    Paused,
    Ended,
}

/// Latest known playback status, published by the player actor.
#[derive(Debug, Clone, PartialEq)]
pub struct PlayerSnapshot {
    pub state: PlayerState,
    pub position: f32,
    pub duration: f32,
    pub volume: f32,
    pub muted: bool,
    /// `true` when the last file ended with an error (the sender tore the
    /// stream down) rather than at its natural end of file. Senders like VLC
    /// read a FINISHED status as "the load failed" and retry another stream
    /// profile, so an error must never be reported as FINISHED.
    pub end_error: bool,
}

impl Default for PlayerSnapshot {
    fn default() -> Self {
        Self {
            state: PlayerState::Idle,
            position: 0.0,
            duration: 0.0,
            volume: 1.0,
            muted: false,
            end_error: false,
        }
    }
}

/// Commands sent to the player actor.
pub enum PlayerCommand {
    Load {
        url: String,
        position: f32,
        autoplay: bool,
        /// `true` when the cast media has a video track (drives the video window).
        video: bool,
        /// What the sender says the track is called, which is what the platform shows. `None` when it said
        /// nothing, in which case mpv keeps naming the stream after its URL.
        title: Option<String>,
        /// Where the sender picture was fetched to, if it offered one and it arrived.
        cover: Option<String>,
    },
    Play,
    Pause,
    Seek(f32),
    Stop,
    SetVolume(f32),
    SetMute(bool),
    #[allow(dead_code)]
    GetSnapshot(oneshot::Sender<PlayerSnapshot>),
}

/// Cloneable handle to a running player actor.
#[derive(Clone)]
pub struct PlayerHandle {
    tx: mpsc::Sender<PlayerCommand>,
    snapshot: Arc<Mutex<PlayerSnapshot>>,
}

impl PlayerHandle {
    pub(crate) fn new(
        tx: mpsc::Sender<PlayerCommand>,
        snapshot: Arc<Mutex<PlayerSnapshot>>,
    ) -> Self {
        Self { tx, snapshot }
    }

    pub async fn load(
        &self,
        url: &str,
        position: f32,
        autoplay: bool,
        video: bool,
        title: Option<String>,
        cover: Option<String>,
    ) -> Result<()> {
        {
            let mut s = self.snapshot.lock().await;
            // A new load is starting, so forget how the previous one ended: a
            // stale error must not colour the acknowledgement we are about to
            // send back.
            s.end_error = false;
            // Optimistically mark BUFFERING so the LOAD acknowledgement is not
            // IDLE: senders like VLC retry LOAD in a loop (each retry restarting
            // playback, which kills audio) when they see IDLE in the response.
            if matches!(s.state, PlayerState::Idle | PlayerState::Ended) {
                s.state = PlayerState::Buffering;
                s.position = position;
            }
        }
        self.send(PlayerCommand::Load {
            url: url.to_string(),
            position,
            autoplay,
            video,
            title,
            cover,
        })
        .await
    }

    pub async fn play(&self) -> Result<()> {
        self.send(PlayerCommand::Play).await
    }

    pub async fn pause(&self) -> Result<()> {
        self.send(PlayerCommand::Pause).await
    }

    pub async fn seek(&self, t: f32) -> Result<()> {
        self.send(PlayerCommand::Seek(t)).await
    }

    pub async fn stop(&self) -> Result<()> {
        self.send(PlayerCommand::Stop).await
    }

    /// Set volume from a Cast level in `0.0..=1.0`.
    pub async fn set_volume(&self, level: f32) -> Result<()> {
        self.send(PlayerCommand::SetVolume(level)).await
    }

    pub async fn set_mute(&self, muted: bool) -> Result<()> {
        self.send(PlayerCommand::SetMute(muted)).await
    }

    pub async fn snapshot(&self) -> PlayerSnapshot {
        self.snapshot.lock().await.clone()
    }

    async fn send(&self, cmd: PlayerCommand) -> Result<()> {
        self.tx
            .send(cmd)
            .await
            .map_err(|_| anyhow::anyhow!("player actor has shut down"))?;
        Ok(())
    }

    /// A no-op player: useful for protocol testing without any media backend.
    pub fn spawn_null() -> Self {
        let (tx, mut rx) = mpsc::channel(32);
        let snapshot = Arc::new(Mutex::new(PlayerSnapshot::default()));
        let snap = snapshot.clone();
        tokio::spawn(async move {
            while let Some(cmd) = rx.recv().await {
                match cmd {
                    PlayerCommand::GetSnapshot(reply) => {
                        let _ = reply.send(snap.lock().await.clone());
                    }
                    PlayerCommand::SetVolume(v) => snap.lock().await.volume = v,
                    PlayerCommand::SetMute(m) => snap.lock().await.muted = m,
                    PlayerCommand::Load { .. }
                    | PlayerCommand::Play
                    | PlayerCommand::Pause
                    | PlayerCommand::Seek(_)
                    | PlayerCommand::Stop => {}
                }
            }
        });
        Self::new(tx, snapshot)
    }
}
