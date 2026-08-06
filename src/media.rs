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

/// Commands this receiver advertises. PAUSE|SEEK|STREAM_VOLUME|STREAM_MUTE
/// (15) plus QUEUE_NEXT (64) and QUEUE_PREV (128) so the sender enables the
/// next/previous controls. Bit values match the Android Cast SDK /
/// pychromecast constants.
const SUPPORTED_MEDIA_COMMANDS: i64 = 15 | 64 | 128;

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
            // VLC sends media STOP+LOAD bursts when a track starts/advances.
            // Stopping mpv immediately aborts the just-issued loadfile opening
            // ("Opening failed or was aborted") and kills audio. Debounce:
            // only actually stop if no LOAD arrives shortly after.
            let load_gen = {
                let st = shared.state.lock().await;
                st.media_load_generation
            };
            let shared2 = shared.clone();
            tokio::spawn(async move {
                tokio::time::sleep(Duration::from_millis(800)).await;
                let gen_now = {
                    let st = shared2.state.lock().await;
                    st.media_load_generation
                };
                if gen_now == load_gen {
                    let _ = shared2.player.stop().await;
                }
            });
            respond_media_status_idle(tx, msg, shared, &rid).await
        }
        "GET_STATUS" => respond_media_status(tx, msg, shared, &rid, media_session_id).await,
        "QUEUE_LOAD" => handle_queue_load(msg, tx, shared, &payload, &rid).await,
        "QUEUE_INSERT" => handle_queue_insert(msg, tx, shared, &payload, &rid).await,
        "QUEUE_NEXT" => {
            let moved = {
                let mut st = shared.state.lock().await;
                match st.session.as_mut() {
                    Some(s) if !s.queue.is_empty() && s.queue_index + 1 < s.queue.len() => {
                        s.queue_index += 1;
                        true
                    }
                    _ => false,
                }
            };
            info!("QUEUE_NEXT received (moved={moved})");
            if moved {
                play_queue_item(shared, true).await?;
            }
            respond_media_status(tx, msg, shared, &rid, media_session_id).await
        }
        "QUEUE_PREV" => {
            let moved = {
                let mut st = shared.state.lock().await;
                match st.session.as_mut() {
                    Some(s) if !s.queue.is_empty() && s.queue_index > 0 => {
                        s.queue_index -= 1;
                        true
                    }
                    _ => false,
                }
            };
            info!("QUEUE_PREV received (moved={moved})");
            if moved {
                play_queue_item(shared, true).await?;
            }
            respond_media_status(tx, msg, shared, &rid, media_session_id).await
        }
        "QUEUE_UPDATE" => {
            // Next/previous are sent as QUEUE_UPDATE with a `jump` offset by
            // the Android Cast SDK / pychromecast (jump=1 next, jump=-1 prev).
            // Repeat/shuffle settings are acknowledged and otherwise ignored.
            let jump = payload.get("jump").and_then(|v| v.as_i64()).unwrap_or(0);
            let moved = {
                let mut st = shared.state.lock().await;
                match st.session.as_mut() {
                    Some(s) if !s.queue.is_empty() => {
                        let target = s.queue_index as i64 + jump;
                        if target >= 0 && (target as usize) < s.queue.len() {
                            s.queue_index = target as usize;
                            true
                        } else {
                            false
                        }
                    }
                    _ => false,
                }
            };
            info!("QUEUE_UPDATE jump={jump} (moved={moved})");
            if moved {
                play_queue_item(shared, true).await?;
            }
            respond_media_status(tx, msg, shared, &rid, media_session_id).await
        }
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
    // A re-cast of the SAME track is not a new queue entry (no duplicates).
    let session_id;
    let skip_load;
    {
        let mut st = shared.state.lock().await;
        // Bump the load counter: any LOAD following a media STOP cancels the
        // debounced stop (VLC's STOP+LOAD burst must not halt playback).
        st.media_load_generation = st.media_load_generation.wrapping_add(1);
        // Absorb VLC's STOP+LOAD burst: ignore repeated LOADs of the SAME
        // contentId that arrive within ~1.5s of the last one. The burst sends
        // STOP+LOAD every ~30-60ms and, once the track is playing, each LOAD
        // re-issues `loadfile replace`, killing the stream before audio is
        // heard (mpv log: loadfile -> Opening done -> audio ready -> 26ms
        // later another loadfile -> EOF -> "Stream ends prematurely at 0").
        let now = std::time::Instant::now();
        let burst_dup = st.last_load.as_ref().is_some_and(|(cid, at)| {
            cid == &content_id && now.duration_since(*at) < Duration::from_millis(1500)
        });
        if !burst_dup {
            st.last_load = Some((content_id.clone(), now));
        }
        let Some(s) = st.session.as_mut() else {
            warn!("LOAD received but no session is running; ignoring");
            return Ok(());
        };
        session_id = s.id.clone();
        let already_this = s
            .media
            .as_ref()
            .is_some_and(|m| m.content_id == content_id);
        // Also dedupe while the same track is still starting up (loading/
        // buffering) so the tail of the burst does not interrupt the opening.
        let starting = matches!(
            shared.player.snapshot().await.state,
            PlayerState::Loading | PlayerState::Buffering
        );
        if already_this {
            // Re-cast of the current track: keep it the active item without
            // appending a duplicate to the queue.
            if let Some(pos) = s.queue.iter().position(|m| m.content_id == content_id) {
                s.queue_index = pos;
            }
            skip_load = starting || burst_dup;
        } else {
            let item = MediaInfo {
                content_id: content_id.clone(),
                content_type: content_type.clone(),
                stream_type: stream_type.clone(),
            };
            s.media = Some(item.clone());
            // Build a playlist from consecutive casts: the first LOAD starts
            // the queue, later LOADs append so next/previous controls become
            // enabled and PREVIOUS can navigate back through cast tracks.
            // (A true full playlist still comes from QUEUE_LOAD/QUEUE_INSERT.)
            if s.queue.is_empty() {
                s.queue = vec![item];
                s.queue_index = 0;
            } else {
                s.queue.push(item);
                s.queue_index = s.queue.len() - 1;
            }
            skip_load = burst_dup;
        }
    }

    info!("LOAD contentId={content_id} type={content_type} autoplay={autoplay} t={current_time} skip_load={skip_load}");
    if !skip_load {
        shared
            .player
            .load(&content_id, current_time, autoplay)
            .await?;
    }

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
        "supportedMediaCommands": SUPPORTED_MEDIA_COMMANDS,
        "volume": { "level": st.volume, "muted": st.muted },
        "media": media,
    });
    if !session.queue.is_empty() {
        let items: Vec<Value> = session
            .queue
            .iter()
            .enumerate()
            .map(|(i, m)| {
                json!({
                    "itemId": i,
                    "media": {
                        "contentId": m.content_id,
                        "contentType": m.content_type,
                        "streamType": m.stream_type,
                    },
                })
            })
            .collect();
        status["items"] = json!(items);
        status["queueData"] = json!({
            "currentItemId": session.queue_index,
            "repeatMode": "REPEAT_OFF",
            "shuffle": false,
        });
    }
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
        "supportedMediaCommands": SUPPORTED_MEDIA_COMMANDS,
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

/// Build a `MediaInfo` from a Cast `media` object (if it has a contentId).
fn media_info_from(media: &Value) -> Option<MediaInfo> {
    let content_id = media.get("contentId")?.as_str()?.to_string();
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
    Some(MediaInfo {
        content_id,
        content_type,
        stream_type,
    })
}

/// Play the item at the session's current queue index.
async fn play_queue_item(shared: &Shared, autoplay: bool) -> Result<()> {
    let item = {
        let mut st = shared.state.lock().await;
        let session = match st.session.as_mut() {
            Some(s) => s,
            None => return Ok(()),
        };
        if session.queue.is_empty() {
            return Ok(());
        }
        let idx = session.queue_index.min(session.queue.len() - 1);
        let item = session.queue[idx].clone();
        session.media = Some(item.clone());
        item
    };
    info!("queue: now playing {} ({})", item.content_id, item.content_type);
    shared.player.load(&item.content_id, 0.0, autoplay).await?;
    Ok(())
}

/// `QUEUE_LOAD { items, startIndex }`: replace the whole queue and play.
async fn handle_queue_load(
    msg: &crate::proto::cast_channel::CastMessage,
    tx: &MessageSink,
    shared: &Shared,
    payload: &Value,
    rid: &Value,
) -> Result<()> {
    let items: Vec<MediaInfo> = payload
        .get("items")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|it| it.get("media").and_then(media_info_from))
                .collect()
        })
        .unwrap_or_default();
    let start = payload
        .get("startIndex")
        .and_then(|v| v.as_u64())
        .unwrap_or(0) as usize;
    {
        let mut st = shared.state.lock().await;
        if let Some(s) = st.session.as_mut() {
            s.queue = items;
            s.queue_index = start.min(s.queue.len().saturating_sub(1));
        }
    }
    play_queue_item(shared, true).await?;
    respond_media_status(tx, msg, shared, rid, 0).await
}

/// `QUEUE_INSERT { items }`: append to the queue; start playing if empty.
async fn handle_queue_insert(
    msg: &crate::proto::cast_channel::CastMessage,
    tx: &MessageSink,
    shared: &Shared,
    payload: &Value,
    rid: &Value,
) -> Result<()> {
    let items: Vec<MediaInfo> = payload
        .get("items")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|it| it.get("media").and_then(media_info_from))
                .collect()
        })
        .unwrap_or_default();
    let started_playing = {
        let mut st = shared.state.lock().await;
        match st.session.as_mut() {
            Some(s) => {
                let was_empty = s.queue.is_empty();
                s.queue.extend(items);
                if was_empty && !s.queue.is_empty() {
                    s.queue_index = 0;
                    true
                } else {
                    false
                }
            }
            None => false,
        }
    };
    if started_playing {
        play_queue_item(shared, true).await?;
    }
    respond_media_status(tx, msg, shared, rid, 0).await
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
