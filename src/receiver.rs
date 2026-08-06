//! Receiver control namespace: `urn:x-cast:com.google.cast.receiver`.
//!
//! Handles `GET_STATUS`, `GET_APP_AVAILABILITY`, `LAUNCH`, `STOP`, `SET_VOLUME`.

use crate::server::{
    MessageSink, NS_MEDIA, NS_RECEIVER, NS_YOUTUBE, Shared, payload_json, send_json,
};
use crate::state::{AppState, Session};
use anyhow::Result;
use serde_json::{Value, json};
use tracing::{info, warn};
use uuid::Uuid;

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

    match kind.as_str() {
        "GET_STATUS" => {
            let st = shared.state.lock().await;
            let status = receiver_status(&st);
            send_json(
                tx,
                "receiver-0",
                &msg.source_id,
                NS_RECEIVER,
                &json!({ "requestId": rid, "status": status, "type": "RECEIVER_STATUS" }),
            )
            .await?;
        }
        "GET_APP_AVAILABILITY" => {
            let app_ids = app_ids_from(&payload);
            let availability: Value = app_ids
                .iter()
                .map(|a| (a.as_str(), "APP_AVAILABLE"))
                .collect();
            info!("app availability requested: {app_ids:?}");
            send_json(
                tx,
                "receiver-0",
                &msg.source_id,
                NS_RECEIVER,
                &json!({ "requestId": rid, "availability": availability, "type": "RECEIVER_ACTION" }),
            )
            .await?;
        }
        "LAUNCH" => {
            let app_id = payload
                .get("appId")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let session = {
                let mut st = shared.state.lock().await;
                st.media_session_counter += 1;
                let session = Session::new(app_id.clone(), st.media_session_counter);
                st.session = Some(session.clone());
                session
            };
            info!("launched app '{app_id}' session {}", session.id);
            let st = shared.state.lock().await;
            let status = receiver_status(&st);
            send_json(
                tx,
                "receiver-0",
                &msg.source_id,
                NS_RECEIVER,
                &json!({ "requestId": rid, "status": status, "type": "RECEIVER_STATUS" }),
            )
            .await?;
        }
        "STOP" => {
            info!("stopping current app");
            let _ = shared.player.stop().await;
            let mut st = shared.state.lock().await;
            st.session = None;
            drop(st);
            let st = shared.state.lock().await;
            let status = receiver_status(&st);
            send_json(
                tx,
                "receiver-0",
                &msg.source_id,
                NS_RECEIVER,
                &json!({ "requestId": rid, "status": status, "type": "RECEIVER_STATUS" }),
            )
            .await?;
        }
        "SET_VOLUME" => {
            let vol = payload.get("volume");
            let mut st = shared.state.lock().await;
            if let Some(l) = vol.and_then(|v| v.get("level")).and_then(|v| v.as_f64()) {
                st.volume = l as f32;
            }
            if let Some(m) = vol.and_then(|v| v.get("muted")).and_then(|v| v.as_bool()) {
                st.muted = m;
            }
            let (level, muted) = (st.volume, st.muted);
            drop(st);

            let _ = shared.player.set_volume(level).await;
            let _ = shared.player.set_mute(muted).await;

            let st = shared.state.lock().await;
            let status = receiver_status(&st);
            send_json(
                tx,
                "receiver-0",
                &msg.source_id,
                NS_RECEIVER,
                &json!({ "requestId": rid, "status": status, "type": "RECEIVER_STATUS" }),
            )
            .await?;
        }
        other => warn!("unhandled receiver message type: {other}"),
    }
    Ok(())
}

/// `GET_APP_AVAILABILITY` may carry `appId` (string) or `appIds` (array).
fn app_ids_from(payload: &Value) -> Vec<String> {
    if let Some(s) = payload.get("appId").and_then(|v| v.as_str()) {
        vec![s.to_string()]
    } else if let Some(arr) = payload.get("appIds").and_then(|v| v.as_array()) {
        arr.iter()
            .filter_map(|v| v.as_str())
            .map(|s| s.to_string())
            .collect()
    } else {
        vec![]
    }
}

/// Build the `RECEIVER_STATUS.status` object from the shared state.
fn receiver_status(state: &AppState) -> Value {
    let apps = match &state.session {
        Some(s) => json!([{
            "appId": s.app_id,
            "displayName": s.display_name,
            "isIdleScreen": false,
            "namespaces": s.namespaces.iter().map(|n| json!({"name": n})).collect::<Vec<_>>(),
            "sessionId": s.id,
            "statusText": s.status_text,
            "transportId": s.id,
        }]),
        None => json!([]),
    };
    json!({
        "activeInput": "input1",
        "applications": apps,
        "standBy": "no",
        "userEq": { "high_shelf": 0.0, "low_shelf": 0.0 },
        "volume": {
            "controlType": "attenuation",
            "level": state.volume,
            "muted": state.muted,
            "stepInterval": 0.05
        }
    })
}

impl Session {
    fn new(app_id: String, media_session_id: u32) -> Self {
        let id = Uuid::new_v4().simple().to_string();
        let (display_name, namespaces) = namespaces_for(&app_id);
        Session {
            id,
            app_id,
            display_name,
            namespaces,
            status_text: "Ready to cast".to_string(),
            media_session_id,
            media: None,
            queue: Vec::new(),
            queue_index: 0,
        }
    }
}

/// Known receiver apps and the namespaces they announce.
fn namespaces_for(app_id: &str) -> (String, Vec<String>) {
    match app_id {
        "YouTube" | "youtube" => ("YouTube".into(), vec![NS_MEDIA.into(), NS_YOUTUBE.into()]),
        "CC1AD845" => ("Default Media Receiver".into(), vec![NS_MEDIA.into()]),
        _ => (app_id.to_string(), vec![NS_MEDIA.into(), NS_YOUTUBE.into()]),
    }
}
