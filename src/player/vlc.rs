//! VLC-backed player, driven over VLC's `rc` (remote control) TCP interface.
//!
//! VLC exposes a simple line-based control protocol when started with
//! `--intf rc --rc-host 127.0.0.1:<port>`. We parse the `status change:` /
//! `( ... )` lines it emits to keep the Cast `MEDIA_STATUS` snapshot fresh,
//! and poll `get_time` / `get_length` / `status` every second so the playback
//! position advances even though VLC does not push time updates.
//!
//! Observed rc output (VLC 3.0.23):
//! ```text
//! status change: ( play state: 3 )        # state is an integer
//! status change: ( new input: <url> )
//! status change: ( audio volume: 256 )    # 0..256, 256 = 100%
//! status: returned 0 (no error)
//! ```
//!
//! rc command map:
//!   Cast LOAD  -> `clear` + `add <url>` (+ `pause` if !autoplay, `seek` if t>0)
//!   PLAY       -> `play`        PAUSE -> `pause`
//!   SEEK       -> `seek <t>`    STOP  -> `stop`
//!   SET_VOLUME -> `volume <0-100>`

use crate::player::{PlayerCommand, PlayerHandle, PlayerSnapshot, PlayerState};
use anyhow::{bail, Context, Result};
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;
use tokio::process::Command;
use tokio::sync::{mpsc, Mutex};
use tokio::time::MissedTickBehavior;
use tracing::{info, warn};

/// VLC input-state integers observed via rc (`play state: N`).
/// 0=stopped/idle, 1=opening, 2=buffering, 3=playing, 4=paused, 5=ended/error.
fn map_state(n: u32) -> PlayerState {
    match n {
        1 => PlayerState::Loading,
        2 => PlayerState::Buffering,
        3 => PlayerState::Playing,
        4 => PlayerState::Paused,
        5 => PlayerState::Ended,
        _ => PlayerState::Idle,
    }
}

/// Spawn VLC and return a handle to the player actor.
pub async fn spawn(vlc_bin: &str, rc_port: u16) -> Result<PlayerHandle> {
    let mut cmd = Command::new(vlc_bin);
    cmd.arg("--intf")
        .arg("rc")
        .arg("--rc-host")
        .arg(format!("127.0.0.1:{rc_port}"))
        .arg("--no-video-title-show")
        .arg("--quiet");
    #[cfg(windows)]
    {
        // CREATE_NO_WINDOW: don't show VLC's console window.
        cmd.creation_flags(0x0800_0000);
    }
    let mut child = cmd
        .spawn()
        .with_context(|| format!("failed to spawn VLC ({vlc_bin}); use --player none to disable playback"))?;

    let (read_half, write_half) = connect_rc(rc_port)
        .await
        .context("failed to connect to VLC rc interface")?;

    let snapshot = Arc::new(Mutex::new(PlayerSnapshot::default()));

    // Reader task: parse `status change:` / `( ... )` lines into the snapshot.
    {
        let snapshot = snapshot.clone();
        tokio::spawn(async move {
            let mut reader = BufReader::new(read_half);
            let mut line = String::new();
            loop {
                line.clear();
                let n = match reader.read_line(&mut line).await {
                    Ok(n) => n,
                    Err(e) => {
                        warn!("vlc rc read error: {e}");
                        break;
                    }
                };
                if n == 0 {
                    break;
                }
                tracing::trace!("vlc rc << {line:?}");
                handle_rc_line(&line, &snapshot).await;
            }
            info!("vlc rc connection closed");
        });
    }

    // Actor task: drive commands and poll status every second.
    let (tx, mut rx) = mpsc::channel::<PlayerCommand>(32);
    let mut conn = RcConn {
        write: write_half,
    };
    tokio::spawn(async move {
        let mut poll = tokio::time::interval(Duration::from_secs(1));
        poll.set_missed_tick_behavior(MissedTickBehavior::Skip);
        loop {
            tokio::select! {
                cmd = rx.recv() => {
                    let Some(cmd) = cmd else { break };
                    if let Err(e) = run_command(&mut conn, cmd).await {
                        warn!("vlc command failed: {e:#}");
                    }
                }
                _ = poll.tick() => {
                    for c in ["get_time", "get_length", "status"] {
                        if let Err(e) = conn.send(c).await {
                            warn!("vlc status poll failed: {e:#}");
                            break;
                        }
                    }
                }
            }
        }
        let _ = child.kill().await;
    });

    info!("vlc player running (bin={vlc_bin}, rc=127.0.0.1:{rc_port})");
    Ok(PlayerHandle::new(tx, snapshot))
}

/// Retry-connect to VLC's rc TCP port.
async fn connect_rc(
    port: u16,
) -> Result<(
    Box<dyn AsyncRead + Unpin + Send>,
    Box<dyn AsyncWrite + Unpin + Send>,
)> {
    let addr = format!("127.0.0.1:{port}");
    let mut last_err = None;
    for _ in 0..50 {
        match TcpStream::connect(&addr).await {
            Ok(s) => {
                let (r, w) = tokio::io::split(s);
                return Ok((Box::new(r), Box::new(w)));
            }
            Err(e) => {
                last_err = Some(e);
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        }
    }
    bail!("VLC rc interface not reachable at {addr}: {last_err:?}")
}

/// Thin wrapper around the rc write half.
struct RcConn {
    write: Box<dyn AsyncWrite + Unpin + Send>,
}

impl RcConn {
    async fn send(&mut self, cmd: &str) -> Result<()> {
        let mut line = cmd.to_string();
        line.push('\n');
        self.write.write_all(line.as_bytes()).await?;
        self.write.flush().await?;
        Ok(())
    }
}

/// Translate a Cast playback command into VLC rc commands.
async fn run_command(conn: &mut RcConn, cmd: PlayerCommand) -> Result<()> {
    match cmd {
        PlayerCommand::Load {
            url,
            position,
            autoplay,
        } => {
            conn.send("clear").await?;
            conn.send(&format!("add {url}")).await?;
            if !autoplay {
                conn.send("pause").await?;
            }
            if position > 0.0 {
                conn.send(&format!("seek {position}")).await?;
            }
            if autoplay {
                conn.send("play").await?;
            }
        }
        PlayerCommand::Play => conn.send("play").await?,
        PlayerCommand::Pause => conn.send("pause").await?,
        PlayerCommand::Seek(t) => conn.send(&format!("seek {t}")).await?,
        PlayerCommand::Stop => conn.send("stop").await?,
        PlayerCommand::SetVolume(level) => {
            let pct = (level.clamp(0.0, 1.0) * 100.0).round() as u32;
            conn.send(&format!("volume {pct}")).await?;
        }
        PlayerCommand::SetMute(m) => {
            // rc has no direct mute; use volume 0 / restore.
            if m {
                conn.send("volume 0").await?;
            } else {
                conn.send("volume 100").await?;
            }
        }
        PlayerCommand::GetSnapshot(reply) => {
            let _ = reply.send(PlayerSnapshot::default());
        }
    }
    Ok(())
}

/// Parse a line of rc output and fold `key: value` pairs into the snapshot.
async fn handle_rc_line(line: &str, snapshot: &Arc<Mutex<PlayerSnapshot>>) {
    let line = line.trim();
    let mut s = snapshot.lock().await;
    // Tolerantly look for `key: value` pairs anywhere in the line.
    for key in ["state", "time", "length", "volume", "mute"] {
        let marker = format!("{key}:");
        let Some(idx) = line.find(&marker) else { continue };
        let value = line[idx + marker.len()..]
            .split_whitespace()
            .next()
            .unwrap_or("")
            .trim_end_matches([')', ':']);
        tracing::trace!("vlc rc parsed {key}={value}");
        match key {
            "state" => {
                if let Ok(n) = value.parse::<u32>() {
                    s.state = map_state(n);
                }
            }
            "time" => {
                if let Ok(f) = value.parse::<f32>()
                    && f >= 0.0
                {
                    s.position = f;
                }
            }
            "length" => {
                if let Ok(f) = value.parse::<f32>() {
                    s.duration = f;
                }
            }
            "volume" => {
                // VLC reports internal volume on a 0..256 scale (256 = 100%).
                if let Ok(n) = value.parse::<f32>()
                    && n > 0.0
                {
                    s.volume = (n / 256.0).clamp(0.0, 1.0);
                }
            }
            "mute" => s.muted = matches!(value, "true" | "1"),
            _ => {}
        }
    }
}
