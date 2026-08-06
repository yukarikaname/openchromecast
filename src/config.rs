//! Command-line configuration for the fake Chromecast.

use clap::Parser;
use std::path::PathBuf;

#[derive(Parser, Debug, Clone)]
#[command(
    name = "openchromecast",
    version,
    about = "Chromecast (Google Cast) receiver emulator — 'openchromecast'"
)]
pub struct Cli {
    /// Friendly name shown to Cast senders (mDNS `fn` record).
    #[arg(long, default_value = "OpenChromecast")]
    pub friendly_name: String,

    /// Model string advertised in mDNS (`md` record).
    #[arg(long, default_value = "Chromecast Ultra")]
    pub model: String,

    /// Device id advertised in mDNS (`id` record, full 128-bit UUID, 32 hex chars). Random if omitted.
    #[arg(long)]
    pub device_id: Option<String>,

    /// Port for the Cast V2 TLS service (default 8009).
    #[arg(long, default_value_t = 8009)]
    pub port: u16,

    /// Capabilities bitmask advertised in mDNS (`ca` record).
    #[arg(long, default_value_t = 4101)]
    pub capabilities: u32,

    /// PEM-encoded device certificate (advanced: real-device credentials).
    #[arg(long)]
    pub cert: Option<PathBuf>,

    /// PEM-encoded PKCS#8 device private key (advanced).
    #[arg(long)]
    pub key: Option<PathBuf>,

    /// Player backend: `mpv` (default), `vlc`, or `none` (protocol testing only).
    #[arg(long, default_value = "mpv")]
    pub player: String,

    /// mpv executable name or path.
    #[arg(long, default_value = "mpv")]
    pub mpv: String,

    /// mpv IPC endpoint path. Defaults to a platform-specific path.
    #[arg(long)]
    pub ipc: Option<String>,

    /// VLC executable path (used with --player vlc). Auto-detected if empty.
    #[arg(long, default_value = "")]
    pub vlc: String,

    /// Port for the VLC rc (remote control) interface.
    #[arg(long, default_value_t = 4212)]
    pub vlc_port: u16,

    /// Increase log verbosity (-v, -vv).
    #[arg(short, action = clap::ArgAction::Count)]
    pub verbose: u8,
}

pub fn parse() -> Cli {
    Cli::parse()
}
