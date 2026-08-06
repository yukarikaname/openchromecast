//! mpv-backed player controlled over JSON IPC.
//!
//! mpv exposes a JSON-RPC line protocol on `--input-ipc-server`:
//! * Unix: a Unix domain socket.
//! * Windows: a named pipe (e.g. `\\.\pipe\openchromecast-<pid>`).
//!
//! We spawn mpv, connect to its IPC endpoint, and drive it with JSON commands.
//! Property-change events (`pause`, `time-pos`, `duration`, `idle-active`,
//! `eof-reached`) are observed so we can publish live `MEDIA_STATUS` updates.

use crate::player::{PlayerCommand, PlayerHandle, PlayerSnapshot, PlayerState};
use anyhow::{Context, Result, anyhow, bail};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::process::Command;
use tokio::sync::{Mutex, mpsc, oneshot};
use tracing::{info, warn};

#[cfg(unix)]
use tokio::net::UnixStream;
#[cfg(windows)]
use tokio::net::windows::named_pipe::ClientOptions;

/// Spawn mpv and return a handle to the player actor.
pub async fn spawn(bin: &str, ipc_path: &str) -> Result<PlayerHandle> {
    let mut cmd = Command::new(bin);
    cmd.arg("--no-terminal")
        .arg("--idle=yes")
        .arg("--keep-open=yes")
        .arg(format!("--input-ipc-server={ipc_path}"));
    #[cfg(windows)]
    {
        // CREATE_NO_WINDOW: don't show a console window.
        cmd.creation_flags(0x0800_0000);
    }
    let mut child = cmd.spawn().with_context(|| {
        format!("failed to spawn mpv ({bin}); use --player none to run without a player")
    })?;

    let (read_half, write_half) = connect_ipc(ipc_path)
        .await
        .context("failed to connect to mpv IPC (is mpv installed?)")?;

    let pending: Arc<Mutex<HashMap<u64, oneshot::Sender<Value>>>> =
        Arc::new(Mutex::new(HashMap::new()));
    let snapshot = Arc::new(Mutex::new(PlayerSnapshot::default()));
    let next_id = Arc::new(AtomicU64::new(0));

    // Reader task: resolves command responses and consumes property events.
    {
        let pending = pending.clone();
        let snapshot = snapshot.clone();
        tokio::spawn(async move {
            let mut reader = BufReader::new(read_half);
            let mut line = String::new();
            loop {
                line.clear();
                let n = match reader.read_line(&mut line).await {
                    Ok(n) => n,
                    Err(e) => {
                        warn!("mpv IPC read error: {e}");
                        break;
                    }
                };
                if n == 0 {
                    break;
                }
                let Ok(v) = serde_json::from_str::<Value>(&line) else {
                    continue;
                };
                if let Some(rid) = v.get("request_id").and_then(|x| x.as_u64()) {
                    if let Some(otx) = pending.lock().await.remove(&rid) {
                        let _ = otx.send(v.clone());
                    }
                } else if let Some(ev) = v.get("event").and_then(|x| x.as_str()) {
                    handle_event(ev, &v, &snapshot).await;
                }
            }
            info!("mpv IPC connection closed");
        });
    }

    // Writer (actor) task: owns the IPC socket write half.
    let (tx, mut rx) = mpsc::channel::<PlayerCommand>(32);
    let conn = MpvConn {
        write: write_half,
        pending: pending.clone(),
        next_id: next_id.clone(),
    };
    tokio::spawn(async move {
        let mut conn = conn;
        // Observe the properties that drive Cast MEDIA_STATUS updates.
        for (id, prop) in [
            (1u64, "pause"),
            (2, "time-pos"),
            (3, "duration"),
            (4, "idle-active"),
            (5, "eof-reached"),
        ] {
            if let Err(e) = conn
                .send_cmd(json!({"command": ["observe_property", id, prop]}))
                .await
            {
                warn!("observe_property {prop} failed: {e}");
            }
        }
        while let Some(cmd) = rx.recv().await {
            if let Err(e) = run_command(&mut conn, cmd).await {
                warn!("mpv command failed: {e:#}");
            }
        }
        // Command channel closed -> shut mpv down.
        let _ = child.kill().await;
    });

    info!("mpv player running (bin={bin}, ipc={ipc_path})");
    Ok(PlayerHandle::new(tx, snapshot))
}

/// Internal connect helper result: boxed read/write halves.
#[cfg(unix)]
async fn connect_ipc(
    path: &str,
) -> Result<(
    Box<dyn AsyncRead + Unpin + Send>,
    Box<dyn AsyncWrite + Unpin + Send>,
)> {
    let path = path.to_string();
    let stream = retry(|| UnixStream::connect(&path)).await?;
    let (r, w) = tokio::io::split(stream);
    Ok((Box::new(r), Box::new(w)))
}

#[cfg(windows)]
async fn connect_ipc(
    path: &str,
) -> Result<(
    Box<dyn AsyncRead + Unpin + Send>,
    Box<dyn AsyncWrite + Unpin + Send>,
)> {
    let path = path.to_string();
    let client = retry(|| ClientOptions::new().open(&path)).await?;
    let (r, w) = tokio::io::split(client);
    Ok((Box::new(r), Box::new(w)))
}

/// Retry a (synchronous) connect until mpv has created its IPC endpoint.
async fn retry<C, F>(mut f: F) -> Result<C>
where
    F: FnMut() -> std::io::Result<C>,
{
    for attempt in 0..100u32 {
        match f() {
            Ok(c) => return Ok(c),
            Err(_e) if attempt < 99 => {
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
            Err(e) => return Err(anyhow!("mpv IPC connect failed: {e}")),
        }
    }
    unreachable!()
}

/// The IPC write side plus pending-request bookkeeping.
struct MpvConn {
    write: Box<dyn AsyncWrite + Unpin + Send>,
    pending: Arc<Mutex<HashMap<u64, oneshot::Sender<Value>>>>,
    next_id: Arc<AtomicU64>,
}

impl MpvConn {
    /// Send a JSON-RPC command and wait for mpv's response.
    async fn send_cmd(&mut self, mut value: Value) -> Result<Value> {
        let rid = self.next_id.fetch_add(1, Ordering::SeqCst) + 1;
        value["request_id"] = json!(rid);

        let (otx, orx) = oneshot::channel();
        self.pending.lock().await.insert(rid, otx);

        let mut line = serde_json::to_string(&value)?;
        line.push('\n');
        self.write.write_all(line.as_bytes()).await?;
        self.write.flush().await?;

        let resp = tokio::time::timeout(Duration::from_secs(10), orx).await;
        match resp {
            Ok(Ok(v)) => Ok(v),
            Ok(Err(_)) => bail!("mpv response channel closed"),
            Err(_) => {
                self.pending.lock().await.remove(&rid);
                bail!("mpv command timed out")
            }
        }
    }
}

/// Translate a Cast playback command into mpv JSON-RPC calls.
async fn run_command(conn: &mut MpvConn, cmd: PlayerCommand) -> Result<()> {
    match cmd {
        PlayerCommand::Load {
            url,
            position,
            autoplay,
        } => {
            conn.send_cmd(json!({"command": ["loadfile", url, "replace"]}))
                .await?;
            if !autoplay {
                conn.send_cmd(json!({"command": ["set_property", "pause", true]}))
                    .await?;
            }
            if position > 0.0 {
                conn.send_cmd(json!({"command": ["seek", position, "absolute"]}))
                    .await?;
            }
        }
        PlayerCommand::Play => {
            conn.send_cmd(json!({"command": ["set_property", "pause", false]}))
                .await?;
        }
        PlayerCommand::Pause => {
            conn.send_cmd(json!({"command": ["set_property", "pause", true]}))
                .await?;
        }
        PlayerCommand::Seek(t) => {
            conn.send_cmd(json!({"command": ["seek", t, "absolute"]}))
                .await?;
        }
        PlayerCommand::Stop => {
            conn.send_cmd(json!({"command": ["stop"]})).await?;
        }
        PlayerCommand::SetVolume(level) => {
            let pct = (level.clamp(0.0, 1.0) * 100.0).round() as i64;
            conn.send_cmd(json!({"command": ["set_property", "volume", pct]}))
                .await?;
        }
        PlayerCommand::SetMute(m) => {
            conn.send_cmd(json!({"command": ["set_property", "mute", m]}))
                .await?;
        }
        PlayerCommand::GetSnapshot(reply) => {
            let _ = reply.send(PlayerSnapshot::default());
        }
    }
    Ok(())
}

/// Apply an mpv property event to the published snapshot.
async fn handle_event(ev: &str, v: &Value, snapshot: &Arc<Mutex<PlayerSnapshot>>) {
    match ev {
        "property-change" => {
            let name = v.get("name").and_then(|x| x.as_str()).unwrap_or("");
            let data = v.get("data");
            let mut s = snapshot.lock().await;
            match name {
                "pause" => {
                    if let Some(b) = data.and_then(|d| d.as_bool())
                        && matches!(
                            s.state,
                            PlayerState::Playing
                                | PlayerState::Paused
                                | PlayerState::Buffering
                                | PlayerState::Loading
                        )
                    {
                        s.state = if b {
                            PlayerState::Paused
                        } else {
                            PlayerState::Playing
                        };
                    }
                }
                "time-pos" => {
                    if let Some(f) = data.and_then(|d| d.as_f64()) {
                        s.position = f as f32;
                    }
                }
                "duration" => {
                    if let Some(f) = data.and_then(|d| d.as_f64()) {
                        s.duration = f as f32;
                    }
                }
                "idle-active" => {
                    if let Some(true) = data.and_then(|d| d.as_bool()) {
                        s.state = PlayerState::Idle;
                        s.position = 0.0;
                    }
                }
                "eof-reached" => {
                    if let Some(true) = data.and_then(|d| d.as_bool()) {
                        s.state = PlayerState::Ended;
                    }
                }
                _ => {}
            }
        }
        "start-file" => {
            snapshot.lock().await.state = PlayerState::Loading;
        }
        "file-loaded" | "playback-restart" => {
            let mut s = snapshot.lock().await;
            if s.state != PlayerState::Ended {
                s.state = PlayerState::Playing;
            }
        }
        _ => {}
    }
}
