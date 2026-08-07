//! openchromecast — a Chromecast (Google Cast) receiver emulator.
//!
//! Pretends to be a Chromecast on the LAN so unmodified Cast senders
//! (Android YouTube, Google Home, Chrome, ...) cast media to this PC.

// On Windows, release builds use the GUI subsystem so no console window is
// shown — the app lives in the system tray only. Debug builds keep a console
// for development and CI, and `--no-tray` still works headless either way.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod config;
mod crypto;
mod mdns;
mod media;
mod player;
mod proto;
mod receiver;
mod server;
mod state;
mod tray;
mod youtube;

use anyhow::{Context, Result};
use std::sync::Arc;
use tokio::net::TcpListener;
use tokio::sync::{Mutex, Notify};
use tracing::{info, warn};
use tracing_subscriber::EnvFilter;

fn main() -> Result<()> {
    let cli = config::parse();
    init_tracing(&cli);
    if let Some(path) = &cli.dump_icon {
        // Packaging helper: render the app icon to a PNG and exit (no GUI).
        crate::tray::write_icon_png(path)?;
        return Ok(());
    }
    if let Some(path) = &cli.dump_icon_ico {
        // Packaging helper: render the app icon as a multi-size .ico and exit.
        crate::tray::write_icon_ico(path)?;
        return Ok(());
    }

    // Single-instance guard (after the packaging helpers so they always run).
    let _lock = match acquire_single_instance_lock() {
        Some(f) => f,
        None => return Ok(()),
    };

    if cli.no_tray {
        // Headless / server mode (CI, SSH, `--player none` testing).
        let rt = tokio::runtime::Runtime::new()?;
        rt.block_on(run_receiver(cli, Arc::new(Notify::new())))
    } else {
        tray::run(cli)
    }
}

/// Prevent multiple receiver instances from running at the same time.
///
/// An exclusive advisory lock is taken on a well-known file in the system
/// temp dir. A second instance cannot acquire it and exits. The OS releases
/// the lock automatically when the process dies, so a stale lock file left
/// behind by a crash does not block future launches.
fn acquire_single_instance_lock() -> Option<std::fs::File> {
    use fs4::fs_std::FileExt;

    let path = std::env::temp_dir().join("openchromecast.lock");
    let file = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(&path)
        .ok()?;
    match file.try_lock_exclusive() {
        // A second instance just exits quietly (no popup). The OS releases the
        // lock automatically when the process dies, so a stale lock file left
        // by a crash never blocks a later launch.
        Ok(()) => Some(file),
        Err(_) => None,
    }
}

fn init_tracing(cli: &config::Cli) {
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
}

/// The core receiver: identity, mDNS, player backend, TLS listener. Runs until
/// the listener ends, Ctrl-C, or the tray requests shutdown.
pub(crate) async fn run_receiver(cli: config::Cli, shutdown: Arc<Notify>) -> Result<()> {
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

    // macOS 15+ Local Network privacy: the permission prompt is triggered by a
    // unicast local-network operation, not by multicast mDNS alone. Probe the
    // local gateway so the prompt appears on first run; otherwise multicast is
    // silently blocked (EHOSTUNREACH) and the device is undiscoverable.
    #[cfg(target_os = "macos")]
    probe_local_network();

    // --- mDNS advertisement ---
    // Non-fatal: if local-network privacy (macOS 15+) blocks multicast mDNS,
    // still run the receiver (TCP listener) and tell the user how to fix
    // discovery instead of silently dying.
    let _mdns = match mdns::advertise(
        &cli.friendly_name,
        &cli.model,
        &device_id,
        cli.port,
        cli.capabilities,
    ) {
        Ok(d) => {
            info!("advertising '{}' ({})", cli.friendly_name, cli.model);
            Some(d)
        }
        Err(e) => {
            warn!(
                "mDNS advertisement failed: {e:#}. The device will NOT be \
                 discoverable. On macOS, open System Settings -> Privacy & \
                 Security -> Local Network and allow OpenChromecast, then \
                 restart the app."
            );
            None
        }
    };

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
        _ = shutdown.notified() => {
            info!("exit requested, shutting down");
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
    // 1) mpv bundled inside the app (self-contained: users install nothing).
    if let Some(p) = bundled_mpv_path() {
        return p;
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

/// Find a mpv shipped inside the app package (next to the executable, or in
/// the .app bundle's Resources on macOS). Returns `None` when not bundled.
fn bundled_mpv_path() -> Option<String> {
    let exe_dir = std::env::current_exe().ok()?.parent()?.to_path_buf();
    #[cfg(windows)]
    let candidates = [
        exe_dir.join("mpv").join("mpv.exe"),
        exe_dir.join("mpv.exe"),
    ];
    // Inside OpenChromecast.app: <exe_dir> = Contents/MacOS, so the bundled
    // player lives in Contents/Resources/mpv/.
    #[cfg(target_os = "macos")]
    let candidates = [
        exe_dir
            .join("..")
            .join("Resources")
            .join("mpv")
            .join("bin")
            .join("mpv"),
        exe_dir.join("..").join("Resources").join("mpv").join("mpv"),
        exe_dir.join("mpv"),
    ];
    #[cfg(not(any(windows, target_os = "macos")))]
    let candidates = [exe_dir.join("mpv"), exe_dir.join("mpv").join("mpv")];
    candidates
        .iter()
        .find(|p| p.exists())
        .map(|p| p.to_string_lossy().to_string())
}

/// Best-effort unicast probe of the local network, run on a background thread.
///
/// macOS 15+ only prompts for the Local Network permission on a *unicast*
/// local-network operation; pure multicast mDNS never triggers the prompt and
/// is silently blocked instead. Probing the gateway/broadcast makes the
/// permission dialog appear on first run so discovery (mDNS) can work after
/// the user clicks Allow.
#[cfg(target_os = "macos")]
fn probe_local_network() {
    use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpStream, UdpSocket};
    use std::time::Duration;

    let mut local: Option<Ipv4Addr> = None;
    for (_name, ip) in local_ip_address::list_afinet_netifas().unwrap_or_default() {
        if let IpAddr::V4(v4) = ip {
            if v4.is_private() {
                local = Some(v4);
                break;
            }
        }
    }
    let Some(local) = local else { return };
    let o = local.octets();
    // Gateway is usually x.x.x.1 on home networks.
    let gw = Ipv4Addr::new(o[0], o[1], o[2], 1);
    let bcast = Ipv4Addr::new(o[0], o[1], o[2], 255);
    let log_path = std::env::temp_dir().join("openchromecast-probe.log");

    std::thread::spawn(move || {
        use std::io::Write;
        let log = |msg: &str| {
            if let Ok(mut f) = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&log_path)
            {
                let _ = writeln!(f, "[{:?}] {msg}", std::time::Instant::now());
            }
        };
        log(&format!("local={local} gw={gw} bcast={bcast}"));
        // Retry a few times: TCC may only prompt after a couple of attempts.
        for attempt in 1..=5 {
            log(&format!("attempt {attempt}"));
            // 1) TCP connect to live local hosts — a *successful* unicast
            //    connection to a LAN device is what trips the TCC prompt.
            for port in [80u16, 443, 22, 8080, 8009] {
                let addr = SocketAddr::new(IpAddr::V4(gw), port);
                match TcpStream::connect_timeout(&addr, Duration::from_millis(500)) {
                    Ok(_) => log(&format!("tcp {gw}:{port} OK")),
                    Err(e) => log(&format!("tcp {gw}:{port} {e}")),
                }
            }
            // 2) UDP datagrams to the gateway and the broadcast address.
            if let Ok(sock) = UdpSocket::bind("0.0.0.0:0") {
                let _ = sock.set_broadcast(true);
                for port in [5353u16, 9, 123, 53, 8009] {
                    let _ = sock.send_to(b"openchromecast-local-network-probe", (gw, port));
                    let _ = sock.send_to(b"openchromecast-local-network-probe", (bcast, port));
                }
                log("udp sent");
            }
            std::thread::sleep(Duration::from_secs(3));
        }
    });
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
