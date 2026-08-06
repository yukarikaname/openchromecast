//! Media namespace: `urn:x-cast:com.google.cast.media`.
//!
//! Handles `LOAD`, `PLAY`, `PAUSE`, `SEEK`, `STOP`, `GET_STATUS` and publishes
//! live `MEDIA_STATUS` updates while a session is active.

use crate::player::{PlayerSnapshot, PlayerState};
use crate::server::{MessageSink, NS_MEDIA, Shared, payload_json, send_json};
use crate::state::MediaInfo;
use anyhow::{Context, Result};
use serde_json::{Value, json};
use std::time::Duration;
use tracing::{info, warn};

pub async fn handle(
    msg: &crate::proto::cast_channel::CastMessage,
    tx: &MessageSink,
    shared: &Shared,
) -> Result<()> {
    let payload = payload_json(msg)?;
    let kind = payload
        .get("type")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let rid = payload.get("requestId").cloned().unwrap_or(json!(0));
    let media_session_id = payload
        .get("mediaSessionId")
        .and_then(|v| v.as_u64())
        .unwrap_or(0) as u32;

    match kind.as_str() {
        "LOAD" => handle_load(msg, tx, shared, &payload, &rid).await,
        "PLAY" => {
            shared.player.play().await?;
            respond_media_status(tx, msg, shared, &rid, media_session_id).await
        }
        "PAUSE" => {
            shared.player.pause().await?;
            respond_media_status(tx, msg, shared, &rid, media_session_id).await
        }
        "SEEK" => {
            let t = payload
                .get("currentTime")
                .and_then(|v| v.as_f64())
                .unwrap_or(0.0) as f32;
            shared.player.seek(t).await?;
            respond_media_status(tx, msg, shared, &rid, media_session_id).await
        }
        "STOP" => {
            shared.player.stop().await?;
            respond_media_status_idle(tx, msg, shared, &rid).await
        }
        "GET_STATUS" => respond_media_status(tx, msg, shared, &rid, media_session_id).await,
        other => {
            warn!("unhandled media message type: {other}");
            Ok(())
        }
    }
}

async fn handle_load(
    msg: &crate::proto::cast_channel::CastMessage,
    tx: &MessageSink,
    shared: &Shared,
    payload: &Value,
    rid: &Value,
) -> Result<()> {
    let media = payload
        .get("media")
        .context("LOAD without a media object")?;
    let content_id = media
        .get("contentId")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let content_type = media
        .get("contentType")
        .and_then(|v| v.as_str())
        .unwrap_or("video/mp4")
        .to_string();
    let stream_type = media
        .get("streamType")
        .and_then(|v| v.as_str())
        .unwrap_or("BUFFERED")
        .to_string();
    let autoplay = payload
        .get("autoplay")
        .and_then(|v| v.as_bool())
        .unwrap_or(true);
    let current_time = payload
        .get("currentTime")
        .and_then(|v| v.as_f64())
        .unwrap_or(0.0) as f32;

    // Remember the media on the active session (echoed back in MEDIA_STATUS).
    let session_id = {
        let mut st = shared.state.lock().await;
        st.session.as_mut().map(|s| {
            s.media = Some(MediaInfo {
                content_id: content_id.clone(),
                content_type: content_type.clone(),
                stream_type: stream_type.clone(),
            });
            s.id.clone()
        })
    };
    let session_id = match session_id {
        Some(id) => id,
        None => {
            warn!("LOAD received but no session is running; ignoring");
            return Ok(());
        }
    };

    info!("LOAD contentId={content_id} type={content_type} autoplay={autoplay} t={current_time}");
    shared
        .player
        .load(&content_id, current_time, autoplay)
        .await?;

    respond_media_status(tx, msg, shared, rid, 0).await?;

    // Publish live MEDIA_STATUS updates while this session lives.
    spawn_status_poller(tx.clone(), shared.clone(), session_id);
    Ok(())
}

/// Build the current `MEDIA_STATUS.status` object (if a session is active).
async fn media_status(shared: &Shared) -> Option<Value> {
    let st = shared.state.lock().await;
    let session = st.session.as_ref()?;
    let snap = shared.player.snapshot().await;
    let state_str = player_state_cast(snap.state);
    let media = session.media.as_ref().map(|m| {
        json!({
            "contentId": m.content_id,
            "contentType": m.content_type,
            "streamType": m.stream_type,
            "duration": snap.duration,
        })
    });
    let mut status = json!({
        "mediaSessionId": session.media_session_id,
        "playbackRate": 1,
        "playerState": state_str,
        "currentTime": snap.position,
        "supportedMediaCommands": 15,
        "volume": { "level": st.volume, "muted": st.muted },
        "media": media,
    });
    if snap.state == PlayerState::Ended {
        status["idleReason"] = json!("FINISHED");
    }
    Some(status)
}

async fn respond_media_status(
    tx: &MessageSink,
    msg: &crate::proto::cast_channel::CastMessage,
    shared: &Shared,
    rid: &Value,
    _media_session_id: u32,
) -> Result<()> {
    if let Some(status) = media_status(shared).await {
        send_json(
            tx,
            "receiver-0",
            &msg.source_id,
            NS_MEDIA,
            &json!({ "requestId": rid, "status": [status], "type": "MEDIA_STATUS" }),
        )
        .await?;
    }
    Ok(())
}

/// Respond to media STOP with an IDLE status.
async fn respond_media_status_idle(
    tx: &MessageSink,
    msg: &crate::proto::cast_channel::CastMessage,
    shared: &Shared,
    rid: &Value,
) -> Result<()> {
    let st = shared.state.lock().await;
    let mid = st.session.as_ref().map(|s| s.media_session_id);
    let media = st.session.as_ref().and_then(|s| s.media.as_ref()).map(|m| {
        json!({
            "contentId": m.content_id,
            "contentType": m.content_type,
            "streamType": m.stream_type,
        })
    });
    let status = json!({
        "mediaSessionId": mid,
        "playerState": "IDLE",
        "idleReason": "CANCELLED",
        "supportedMediaCommands": 15,
        "volume": { "level": st.volume, "muted": st.muted },
        "media": media,
    });
    send_json(
        tx,
        "receiver-0",
        &msg.source_id,
        NS_MEDIA,
        &json!({ "requestId": rid, "status": [status], "type": "MEDIA_STATUS" }),
    )
    .await
}

/// Poll the player and push unsolicited MEDIA_STATUS updates.
fn spawn_status_poller(tx: MessageSink, shared: Shared, session_id: String) {
    tokio::spawn(async move {
        let mut last = PlayerSnapshot::default();
        loop {
            tokio::time::sleep(Duration::from_millis(1000)).await;

            let alive = shared
                .state
                .lock()
                .await
                .session
                .as_ref()
                .map(|s| s.id == session_id)
                .unwrap_or(false);
            if !alive {
                break;
            }

            let snap = shared.player.snapshot().await;
            let changed = snap.state != last.state
                || (snap.position - last.position).abs() > 0.5
                || snap.duration != last.duration;
            if !changed {
                continue;
            }

            if let Some(status) = media_status(&shared).await {
                let payload = json!({ "status": [status], "type": "MEDIA_STATUS" });
                if send_json(&tx, "receiver-0", &session_id, NS_MEDIA, &payload)
                    .await
                    .is_err()
                {
                    break;
                }
            }
            last = snap;
        }
    });
}

/// Map our player state onto the Cast `playerState` string.
fn player_state_cast(state: PlayerState) -> &'static str {
    match state {
        PlayerState::Idle | PlayerState::Ended => "IDLE",
        PlayerState::Loading | PlayerState::Buffering => "BUFFERING",
        PlayerState::Playing => "PLAYING",
        PlayerState::Paused => "PAUSED",
    }
}
