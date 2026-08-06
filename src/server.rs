//! TLS transport + Cast V2 message framing + namespace dispatch.
//!
//! Cast V2 runs over TLS on port 8009. Each message is a `CastMessage`
//! (protobuf) prefixed with a 4-byte big-endian length. Before anything else,
//! the sender completes a device-auth handshake (`...tp.deviceauth`).

use crate::crypto::Identity;
use crate::media;
use crate::player::PlayerHandle;
use crate::proto::cast_channel::{
    AuthResponse, CastMessage, DeviceAuthMessage, HashAlgorithm, PayloadType, SignatureAlgorithm,
};
use crate::receiver;
use crate::state::AppState;
use crate::youtube;
use anyhow::{Context, Result, bail};
use prost::Message as _;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{Mutex, mpsc};
use tokio_rustls::TlsAcceptor;
use tracing::{debug, info, warn};

// Standard Cast V2 namespaces.
pub const NS_CONNECTION: &str = "urn:x-cast:com.google.cast.tp.connection";
pub const NS_HEARTBEAT: &str = "urn:x-cast:com.google.cast.tp.heartbeat";
pub const NS_DEVICE_AUTH: &str = "urn:x-cast:com.google.cast.tp.deviceauth";
pub const NS_RECEIVER: &str = "urn:x-cast:com.google.cast.receiver";
pub const NS_MEDIA: &str = "urn:x-cast:com.google.cast.media";
pub const NS_YOUTUBE: &str = "urn:x-cast:com.google.youtube.mdx";

/// A sink for framed messages destined for one client connection.
pub type MessageSink = mpsc::Sender<Vec<u8>>;

/// Everything shared between connections and tasks.
#[derive(Clone)]
pub struct Shared {
    pub state: Arc<Mutex<AppState>>,
    pub identity: Arc<Identity>,
    pub player: PlayerHandle,
}

/// Accept loop: spawn one task per incoming connection.
pub async fn run(listener: TcpListener, shared: Shared) -> Result<()> {
    loop {
        let (tcp, peer) = listener.accept().await?;
        let shared = shared.clone();
        tokio::spawn(async move {
            if let Err(err) = handle_connection(tcp, peer, shared).await {
                warn!("connection {peer} ended: {err:#}");
            }
        });
    }
}

/// Handle one Cast connection: TLS, device auth, then the message loop.
async fn handle_connection(tcp: TcpStream, peer: SocketAddr, shared: Shared) -> Result<()> {
    info!("incoming Cast connection from {peer}");
    let acceptor = tls_acceptor(&shared.identity)?;
    let stream = match acceptor.accept(tcp).await {
        Ok(s) => s,
        Err(e) => {
            // Probing connections (VLC, Google Home, Chrome, ...) frequently
            // open a socket and abort before the handshake completes. That is
            // normal discovery noise, so log it at debug, not as a warning.
            debug!("TLS handshake with {peer} failed: {e}");
            return Ok(());
        }
    };
    info!("TLS established with {peer}");

    let (mut read_half, write_half) = tokio::io::split(stream);
    let (tx, mut rx) = mpsc::channel::<Vec<u8>>(64);

    // Writer task: owns the TLS write half, drains the outbound queue.
    let mut write_half = write_half;
    let writer = tokio::spawn(async move {
        while let Some(frame) = rx.recv().await {
            if write_half.write_all(&frame).await.is_err() {
                break;
            }
        }
    });

    // Read + dispatch loop. Track whether this connection owns the active
    // session's transport channel.
    let mut transport: Option<String> = None;
    loop {
        let msg = match read_message(&mut read_half).await {
            Ok(m) => m,
            Err(err) => {
                debug!("read from {peer} failed: {err:#}");
                break;
            }
        };
        if msg.namespace == NS_CONNECTION
            && let Ok(payload) = payload_json(&msg)
            && payload.get("type").and_then(|v| v.as_str()) == Some("CONNECT")
        {
            transport = Some(msg.destination_id.clone());
        }
        if let Err(err) = dispatch(&msg, &tx, &shared).await {
            warn!("dispatch error on {peer}: {err:#}");
        }
    }

    drop(tx);
    let _ = writer.await;

    // The sender that owned the active session left (e.g. the user switched
    // the cast target back to the phone). Stop playback so it does not keep
    // running silently on this device.
    let owned_session = {
        let st = shared.state.lock().await;
        match (&st.session, &transport) {
            (Some(s), Some(t)) => s.id == *t,
            _ => false,
        }
    };
    if owned_session {
        info!("session owner disconnected; stopping playback");
        let _ = shared.player.stop().await;
        let mut st = shared.state.lock().await;
        st.session = None;
    }

    info!("connection {peer} closed");
    Ok(())
}

fn tls_acceptor(identity: &Identity) -> Result<TlsAcceptor> {
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let config = rustls::ServerConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()?
        .with_no_client_auth()
        .with_single_cert(
            vec![rustls::pki_types::CertificateDer::from(
                identity.tls_cert_der().to_vec(),
            )],
            identity.tls_private_key_der()?,
        )?;
    Ok(TlsAcceptor::from(Arc::new(config)))
}

/// Read one length-prefixed `CastMessage`.
async fn read_message<S: AsyncRead + Unpin>(stream: &mut S) -> Result<CastMessage> {
    let mut len_buf = [0u8; 4];
    stream.read_exact(&mut len_buf).await?;
    let len = u32::from_be_bytes(len_buf) as usize;
    if len > 1 << 20 {
        bail!("frame too large: {len} bytes");
    }
    let mut buf = vec![0u8; len];
    stream.read_exact(&mut buf).await?;
    CastMessage::decode(&buf[..]).context("failed to decode CastMessage")
}

/// Serialize a message and enqueue it on the outbound sink.
pub async fn send_message(tx: &MessageSink, msg: &CastMessage) -> Result<()> {
    let bytes = msg.encode_to_vec();
    let mut framed = Vec::with_capacity(bytes.len() + 4);
    framed.extend_from_slice(&(bytes.len() as u32).to_be_bytes());
    framed.extend_from_slice(&bytes);
    tx.send(framed).await?;
    Ok(())
}

/// Build and enqueue a JSON `CastMessage`.
pub async fn send_json(
    tx: &MessageSink,
    source: &str,
    destination: &str,
    namespace: &str,
    payload: &serde_json::Value,
) -> Result<()> {
    let payload_utf8 = serde_json::to_string(payload)?;
    let msg = CastMessage {
        protocol_version: 0,
        source_id: source.to_string(),
        destination_id: destination.to_string(),
        namespace: namespace.to_string(),
        payload_type: PayloadType::String as i32,
        payload_utf8: Some(payload_utf8),
        payload_binary: None,
        ..Default::default()
    };
    send_message(tx, &msg).await
}

/// Parse the UTF-8 JSON payload of a message.
pub fn payload_json(msg: &CastMessage) -> Result<serde_json::Value> {
    let s = msg
        .payload_utf8
        .as_deref()
        .context("message has no UTF-8 payload")?;
    Ok(serde_json::from_str(s)?)
}

/// Route a message by namespace.
async fn dispatch(msg: &CastMessage, tx: &MessageSink, shared: &Shared) -> Result<()> {
    let kind = msg
        .payload_utf8
        .as_deref()
        .and_then(|s| serde_json::from_str::<serde_json::Value>(s).ok())
        .and_then(|v| v.get("type").and_then(|t| t.as_str()).map(str::to_string))
        .unwrap_or_default();
    debug!(
        "ns={} src={} dst={} type={}",
        msg.namespace, msg.source_id, msg.destination_id, kind
    );
    match msg.namespace.as_str() {
        NS_DEVICE_AUTH => handle_device_auth(msg, tx, shared).await,
        NS_CONNECTION => handle_connection_ns(msg).await,
        NS_HEARTBEAT => handle_heartbeat(msg, tx).await,
        NS_RECEIVER => receiver::handle(msg, tx, shared).await,
        NS_MEDIA => media::handle(msg, tx, shared).await,
        NS_YOUTUBE => youtube::handle(msg, tx, shared).await,
        other => {
            warn!("unhandled namespace: {other}");
            Ok(())
        }
    }
}

/// Device-auth handshake: sign the sender's nonce and present our certificate.
///
/// Ground truth from Chromium's `cast_auth_util.cc`: the sender expects the
/// `AuthResponse` to echo `sender_nonce` (field 5), carry a certificate chain
/// (`client_auth_certificate` + `intermediate_certificate`) that paths to a
/// trusted Cast root CA, and sign `sender_nonce || peer_cert_der` (the TLS
/// cert) with RSASSA-PKCS#1 v1.5 + SHA-256. The incoming challenge is parsed
/// *tolerantly* (only the nonce is needed), so field-number drift between Cast
/// SDK versions is harmless.
async fn handle_device_auth(msg: &CastMessage, tx: &MessageSink, shared: &Shared) -> Result<()> {
    let payload = msg
        .payload_binary
        .as_deref()
        .context("device auth message has no binary payload")?;

    // DeviceAuthMessage.challenge is a length-delimited sub-message at field 1.
    if let Some(challenge) = extract_length_delimited(payload, 1) {
        let nonce = extract_nonce(challenge).unwrap_or_default();

        // The signature covers (sender_nonce || TLS peer cert).
        let mut sig_input = nonce.clone();
        sig_input.extend_from_slice(shared.identity.tls_cert_der());
        let signature = shared.identity.sign(&sig_input);

        let response = DeviceAuthMessage {
            response: Some(AuthResponse {
                signature,
                client_auth_certificate: shared.identity.auth_cert_der().to_vec(),
                intermediate_certificate: shared
                    .identity
                    .intermediate_der()
                    .map(|c| vec![c.to_vec()])
                    .unwrap_or_default(),
                signature_algorithm: Some(SignatureAlgorithm::RsassaPkcs1v15 as i32),
                sender_nonce: Some(nonce.clone()),
                hash_algorithm: Some(HashAlgorithm::Sha256 as i32),
                crl: None,
            }),
            ..Default::default()
        };

        let reply = CastMessage {
            protocol_version: 0,
            source_id: msg.destination_id.clone(),
            destination_id: msg.source_id.clone(),
            namespace: NS_DEVICE_AUTH.to_string(),
            payload_type: PayloadType::Binary as i32,
            payload_utf8: None,
            payload_binary: Some(response.encode_to_vec()),
            ..Default::default()
        };
        send_message(tx, &reply).await?;
        info!(
            "completed device auth (nonce {}B, sig input {}B)",
            nonce.len(),
            sig_input.len()
        );
    }
    Ok(())
}

/// Minimal, tolerant protobuf walker: return the bytes of the first
/// length-delimited field with number `target` at this level.
/// Unknown fields of any wire type are skipped, so schema drift is harmless.
fn extract_length_delimited(data: &[u8], target: u32) -> Option<&[u8]> {
    length_delimited_fields(data)
        .into_iter()
        .find(|(field, _)| *field == target)
        .map(|(_, bytes)| bytes)
}

/// Pick the sender nonce out of an `AuthChallenge`: the first length-delimited
/// field that is random binary (non-UTF-8). Falls back to the first field.
fn extract_nonce(challenge: &[u8]) -> Option<Vec<u8>> {
    let fields = length_delimited_fields(challenge);
    fields
        .iter()
        .find(|(_, b)| b.len() >= 8 && std::str::from_utf8(b).is_err())
        .or_else(|| fields.first())
        .map(|(_, b)| b.to_vec())
}

/// Walk one protobuf message level and return every (field_number, bytes)
/// pair for length-delimited fields, skipping all other wire types.
fn length_delimited_fields(data: &[u8]) -> Vec<(u32, &[u8])> {
    let mut out = Vec::new();
    let mut pos = 0usize;
    while pos < data.len() {
        let Some((tag, next)) = read_varint(data, pos) else {
            break;
        };
        pos = next;
        let field = (tag >> 3) as u32;
        let wire = (tag & 0x07) as u8;
        match wire {
            0 => {
                let Some((_, next)) = read_varint(data, pos) else {
                    break;
                };
                pos = next;
            }
            1 => {
                let Some(p) = pos.checked_add(8) else { break };
                pos = p;
            }
            2 => {
                let Some((len, next)) = read_varint(data, pos) else {
                    break;
                };
                pos = next;
                let Some(end) = pos.checked_add(len as usize) else {
                    break;
                };
                if end <= data.len() {
                    out.push((field, &data[pos..end]));
                }
                pos = end;
            }
            5 => {
                let Some(p) = pos.checked_add(4) else { break };
                pos = p;
            }
            _ => break,
        }
    }
    out
}

/// Decode a base-128 varint at `pos`.
fn read_varint(data: &[u8], pos: usize) -> Option<(u64, usize)> {
    let mut value: u64 = 0;
    let mut shift = 0;
    let mut p = pos;
    loop {
        let b = *data.get(p)?;
        value |= ((b & 0x7f) as u64) << shift;
        p += 1;
        if b & 0x80 == 0 {
            break;
        }
        shift += 7;
        if shift >= 64 {
            return None;
        }
    }
    Some((value, p))
}

/// Connection-namespace bookkeeping (CONNECT / CLOSE).
async fn handle_connection_ns(msg: &CastMessage) -> Result<()> {
    let payload = payload_json(msg)?;
    let kind = payload.get("type").and_then(|v| v.as_str()).unwrap_or("");
    match kind {
        "CONNECT" => info!("CONNECT {} -> {}", msg.source_id, msg.destination_id),
        "CLOSE" => info!("CLOSE {} -> {}", msg.source_id, msg.destination_id),
        other => warn!("unhandled connection message type: {other}"),
    }
    Ok(())
}

/// Heartbeat namespace: answer PING with PONG.
async fn handle_heartbeat(msg: &CastMessage, tx: &MessageSink) -> Result<()> {
    let payload = payload_json(msg)?;
    let kind = payload.get("type").and_then(|v| v.as_str()).unwrap_or("");
    if kind == "PING" {
        let pong = serde_json::json!({ "type": "PONG" });
        send_json(tx, &msg.destination_id, &msg.source_id, NS_HEARTBEAT, &pong).await?;
    }
    Ok(())
}
