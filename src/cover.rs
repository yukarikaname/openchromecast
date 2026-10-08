//! Fetching the artwork a sender offers, because mpv cannot.
//!
//! A Cast `LOAD` may carry `metadata.images`, and the URL points back at the sender: a phone on the same
//! network, serving plain HTTP. mpv can only be *given* a file, so the picture is fetched now and handed
//! over as a path. This is deliberately not a dependency - it is one GET of a small image from a device
//! next door, over `http`, and there is nothing here for a TLS stack or a connection pool to do.

use std::path::PathBuf;

use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// Download `url` into the temporary directory and return where it landed.
///
/// `None` for anything that is not a plain `http` URL, for a network that will not answer, and for a
/// response that is not a 200 - all of which are a track with no cover rather than a failed cast.
pub async fn fetch(url: &str) -> Option<PathBuf> {
    let rest = url.strip_prefix("http://")?;
    let (authority, path) = match rest.find("/") {
        Some(at) => (&rest[..at], &rest[at..]),
        None => (rest, "/"),
    };
    let authority = if authority.contains(":") {
        authority.to_string()
    } else {
        format!("{authority}:80")
    };

    let mut stream = tokio::net::TcpStream::connect(&authority).await.ok()?;
    let request =
        format!("GET {path} HTTP/1.1\r\nHost: {authority}\r\nConnection: close\r\n\r\n");
    stream.write_all(request.as_bytes()).await.ok()?;
    let mut raw = Vec::new();
    stream.read_to_end(&mut raw).await.ok()?;

    let picture = body_of(&raw)?;
    let out = std::env::temp_dir().join("openchromecast-art");
    std::fs::write(&out, picture).ok()?;
    Some(out)
}

/// The payload of a response: past the headers, and de-chunked if it arrived that way.
fn body_of(raw: &[u8]) -> Option<Vec<u8>> {
    let split = raw.windows(4).position(|w| w == b"\r\n\r\n")? + 4;
    let (head, body) = raw.split_at(split);
    let head = String::from_utf8_lossy(head).to_ascii_lowercase();
    if !head.starts_with("http/1.1 200") && !head.starts_with("http/1.0 200") {
        return None;
    }
    if !head.contains("transfer-encoding: chunked") {
        return Some(body.to_vec());
    }
    let mut out = Vec::new();
    let mut rest = body;
    loop {
        let line_end = rest.windows(2).position(|w| w == b"\r\n")?;
        let size = usize::from_str_radix(std::str::from_utf8(&rest[..line_end]).ok()?.trim(), 16).ok()?;
        rest = rest.get(line_end + 2..)?;
        if size == 0 {
            break;
        }
        out.extend_from_slice(rest.get(..size)?);
        rest = rest.get(size + 2..)?;
    }
    Some(out)
}
