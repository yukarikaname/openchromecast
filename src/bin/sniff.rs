//! `cast-sniff` — a passive TCP proxy that dumps raw Cast V2 traffic.
//!
//! Point a Cast sender at the proxy port, have the proxy forward to a real
//! Chromecast (or this fake receiver), and capture the exact bytes in both
//! directions. Even though the payload is TLS-encrypted, the TLS handshake
//! reveals the real device's server certificate chain — exactly what you need
//! to impersonate a device (see `docs/reverse-engineering.md`).
//!
//! Usage:
//! ```text
//! cargo run --bin cast-sniff -- --listen 0.0.0.0:8009 --target 192.168.1.50:8009 --out ./capture
//! ```

use anyhow::Result;
use clap::Parser;
use std::fs::File;
use std::io::Write;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

static CONN: AtomicU64 = AtomicU64::new(0);

#[derive(Parser)]
#[command(
    name = "cast-sniff",
    about = "Passive TCP proxy that dumps Cast V2 traffic"
)]
struct Args {
    /// Address to listen on.
    #[arg(long, default_value = "0.0.0.0:8009")]
    listen: SocketAddr,

    /// Upstream Cast receiver to forward to.
    #[arg(long)]
    target: SocketAddr,

    /// Directory for captured byte streams.
    #[arg(long, default_value = "./capture")]
    out: PathBuf,
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    std::fs::create_dir_all(&args.out)?;
    let listener = TcpListener::bind(args.listen).await?;
    println!(
        "sniffing {} -> {} (logging to {})",
        args.listen,
        args.target,
        args.out.display()
    );

    loop {
        let (client, _) = listener.accept().await?;
        let target = match TcpStream::connect(args.target).await {
            Ok(t) => t,
            Err(e) => {
                eprintln!("connect to {} failed: {e}", args.target);
                continue;
            }
        };
        let id = CONN.fetch_add(1, Ordering::SeqCst);
        let out_dir = args.out.clone();
        tokio::spawn(async move {
            let _ = proxy(client, target, id, &out_dir).await;
        });
    }
}

async fn proxy(client: TcpStream, target: TcpStream, id: u64, out_dir: &Path) -> Result<()> {
    let (c_r, c_w) = tokio::io::split(client);
    let (t_r, t_w) = tokio::io::split(target);
    let a2b = tokio::spawn(pump(
        c_r,
        t_w,
        out_dir.join(format!("{id:04}_sender_to_receiver.bin")),
        "sender->receiver",
    ));
    let b2a = tokio::spawn(pump(
        t_r,
        c_w,
        out_dir.join(format!("{id:04}_receiver_to_sender.bin")),
        "receiver->sender",
    ));
    let _ = a2b.await;
    let _ = b2a.await;
    Ok(())
}

async fn pump<F, T>(mut from: F, mut to: T, path: PathBuf, label: &'static str) -> Result<()>
where
    F: AsyncRead + Unpin,
    T: AsyncWrite + Unpin,
{
    let mut file = File::create(&path)?;
    let mut buf = [0u8; 8192];
    loop {
        let n = match from.read(&mut buf).await {
            Ok(0) => break,
            Ok(n) => n,
            Err(_) => break,
        };
        file.write_all(&buf[..n])?;
        file.flush()?;
        if to.write_all(&buf[..n]).await.is_err() {
            break;
        }
    }
    println!("[{label}] captured to {}", path.display());
    Ok(())
}
