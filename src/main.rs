//! openchromecast — a Chromecast (Google Cast) receiver emulator.
//!
//! Pretends to be a Chromecast on the LAN so unmodified Cast senders
//! (Android YouTube, Google Home, Chrome, ...) cast media to this PC.

mod config;
mod crypto;
mod mdns;
mod media;
mod player;
mod proto;
mod receiver;
mod server;
mod state;
mod youtube;

use anyhow::{Context, Result};
use std::sync::Arc;
use tokio::net::TcpListener;
use tokio::sync::Mutex;
use tracing::{info, warn};
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> Result<()> {
    let cli = config::parse();

    let level = match cli.verbose {
        0 => "info",
        1 => "debug",
        _ => "trace",
    };
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(level)),
        )
        .init();

    let device_id = match &cli.device_id {
        Some(d) => d.clone(),
        // Full 128-bit UUID (32 hex chars): pychromecast explicitly ignores
        // devices whose `id` TXT is not a parseable UUID.
        None => uuid::Uuid::new_v4().simple().to_string(),
    };

    // --- Device identity (signing key + TLS certificate) ---
    let identity = crypto::Identity::load_or_generate(cli.cert.as_deref(), cli.key.as_deref())
        .context("failed to prepare device identity")?;
    info!(
        "device identity ready (auth cert {} bytes)",
        identity.auth_cert_der().len()
    );

    // --- mDNS advertisement ---
    let _mdns = mdns::advertise(
        &cli.friendly_name,
        &cli.model,
        &device_id,
        cli.port,
        cli.capabilities,
    )
    .context("failed to start mDNS advertisement")?;
    info!("advertising '{}' ({})", cli.friendly_name, cli.model);

    // --- Player backend ---
    let player = match cli.player.to_ascii_lowercase().as_str() {
        "none" => {
            info!("using null player (no media playback)");
            player::PlayerHandle::spawn_null()
        }
        "vlc" => {
            let path = resolve_vlc_path(&cli.vlc);
            info!("spawning VLC player ({path}), rc port {}", cli.vlc_port);
            match player::vlc::spawn(&path, cli.vlc_port).await {
                Ok(p) => p,
                Err(e) => {
                    warn!("VLC unavailable ({e:#}); falling back to null player");
                    player::PlayerHandle::spawn_null()
                }
            }
        }
        _ => {
            let mpv_path = resolve_mpv_path(&cli.mpv);
            let ipc = cli.ipc.clone().unwrap_or_else(default_ipc_path);
            info!("spawning mpv player ({mpv_path}), ipc {ipc}");
            match player::mpv::spawn(&mpv_path, &ipc).await {
                Ok(p) => p,
                Err(e) => {
                    warn!("mpv unavailable ({e:#}); falling back to null player");
                    player::PlayerHandle::spawn_null()
                }
            }
        }
    };

    // --- Shared state ---
    let shared = server::Shared {
        state: Arc::new(Mutex::new(state::AppState::default())),
        identity: Arc::new(identity),
        player,
    };

    // --- Cast V2 TLS listener ---
    let listener = TcpListener::bind(("0.0.0.0", cli.port))
        .await
        .with_context(|| format!("failed to bind port {}", cli.port))?;
    info!("listening for Cast V2 connections on port {}", cli.port);

    tokio::select! {
        r = server::run(listener, shared) => r?,
        _ = tokio::signal::ctrl_c() => {
            info!("received Ctrl-C, shutting down");
        }
    }

    Ok(())
}

/// Platform-appropriate default mpv IPC endpoint.
fn default_ipc_path() -> String {
    #[cfg(windows)]
    {
        format!(r"\\.\pipe\openchromecast-{}", std::process::id())
    }
    #[cfg(unix)]
    {
        format!("/tmp/openchromecast-{}.sock", std::process::id())
    }
}

/// Resolve the mpv executable path: honor an explicit path (anything other
/// than the bare `mpv` default), then auto-detect well-known install
/// locations (e.g. the winget package dir), then fall back to PATH.
fn resolve_mpv_path(explicit: &str) -> String {
    if !explicit.is_empty() && explicit != "mpv" {
        return explicit.to_string();
    }
    #[cfg(windows)]
    {
        if let Ok(root) = std::env::var("LOCALAPPDATA") {
            let packages = std::path::Path::new(&root).join(r"Microsoft\WinGet\Packages");
            if let Ok(entries) = std::fs::read_dir(&packages) {
                for entry in entries.flatten() {
                    let candidate = entry.path().join("mpv.exe");
                    if candidate.exists() {
                        return candidate.to_string_lossy().to_string();
                    }
                }
            }
        }
    }
    "mpv".to_string()
}

/// Resolve the VLC executable path (explicit flag, else well-known locations).
fn resolve_vlc_path(explicit: &str) -> String {
    if !explicit.is_empty() {
        return explicit.to_string();
    }
    #[cfg(windows)]
    {
        const CANDIDATES: [&str; 2] = [
            r"C:\Program Files\VideoLAN\VLC\vlc.exe",
            r"C:\Program Files (x86)\VideoLAN\VLC\vlc.exe",
        ];
        for p in CANDIDATES {
            if std::path::Path::new(p).exists() {
                return p.to_string();
            }
        }
    }
    "vlc".to_string()
}
