//! The `mqtt` input: one subscribe-only MQTT client per `inputs[]` entry,
//! mapping each message to an [`ExternalTrigger`] for the entry's source.
//!
//! The entry is the `mqtt-triggers` source config with two differences:
//! `kind` is `"mqtt"` (the module), so the trigger kind it files messages
//! under is `trigger_kind` (default `sensor_motion`); and `username` is
//! part of the entry while the password is the credential the app passes
//! in `secrets` under the entry's `id`.

use crate::alerts_owner::{
    AlertInput, ExternalTrigger, ExternalTriggers, InputContext, InputFactory,
};
use alerts::trigger::TriggerState;
use mqtt_triggers::{Credentials, Handle, Options, SourceConfig, Status, Trigger};
use serde_json::{Value, json};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use tracing::{info, warn};

/// The factory the core registers for the `mqtt` kind.
pub fn mqtt_factory() -> InputFactory {
    Arc::new(|ctx, raw| build(ctx, raw).map(|i| Box::new(i) as Box<dyn AlertInput>))
}

struct MqttInput {
    handle: Option<Handle>,
    /// Triggers the alert service refused because its queue was full.
    queue_dropped: Arc<AtomicU64>,
}

fn source_config(raw: &Value) -> Result<(SourceConfig, Option<String>), String> {
    let mut obj = raw
        .as_object()
        .cloned()
        .ok_or_else(|| "MQTT input settings must be an object".to_owned())?;
    let username = match obj.remove("username") {
        None | Some(Value::Null) => None,
        Some(Value::String(s)) if !s.is_empty() => Some(s),
        Some(Value::String(_)) => None,
        Some(_) => return Err("MQTT username must be text".to_owned()),
    };
    let trigger_kind = match obj.remove("trigger_kind") {
        None | Some(Value::Null) => "sensor_motion".to_owned(),
        Some(Value::String(s)) => s,
        Some(_) => return Err("MQTT trigger_kind must be text".to_owned()),
    };
    obj.insert("kind".to_owned(), Value::String(trigger_kind));
    // Settings the entry carries for the app, not for the client.
    obj.remove("enabled");
    let config: SourceConfig = serde_json::from_value(Value::Object(obj))
        .map_err(|_| "MQTT input settings are incomplete or malformed".to_owned())?;
    Ok((config, username))
}

fn sink(triggers: ExternalTriggers, dropped: Arc<AtomicU64>) -> mqtt_triggers::Sink {
    Arc::new(move |t: Trigger| {
        let state = match t.state {
            mqtt_triggers::TriggerState::Start => TriggerState::Start,
            mqtt_triggers::TriggerState::Stop => TriggerState::Stop,
            mqtt_triggers::TriggerState::Pulse => TriggerState::Pulse,
        };
        // The trigger is stamped with the core's own clock when queued.
        let r = triggers.put(ExternalTrigger {
            source: t.source_id,
            kind: t.kind,
            state,
            at_ms: None,
        });
        if r.is_err() {
            dropped.fetch_add(1, Ordering::Relaxed);
        }
    })
}

fn build(ctx: &InputContext, raw: &Value) -> Result<MqttInput, String> {
    let (config, username) = source_config(raw)?;
    let credentials = username.map(|username| Credentials {
        username,
        password: ctx
            .secret
            .as_ref()
            .map(|s| s.expose().to_owned())
            .unwrap_or_default(),
    });
    let queue_dropped = Arc::new(AtomicU64::new(0));
    let id = config.id.clone();
    let status_events = ctx.status_events.clone();
    let options = Options {
        log: Some(Arc::new(move |line: &str| {
            warn!(source = %id, "mqtt input: {line}");
            status_events(json!({"log": line}));
        })),
        ..Options::default()
    };
    let handle = mqtt_triggers::start(
        config,
        credentials,
        sink(ctx.triggers.clone(), queue_dropped.clone()),
        options,
    )
    .map_err(|e| format!("MQTT input could not start: {e:?}"))?;
    info!("mqtt trigger input started");
    Ok(MqttInput {
        handle: Some(handle),
        queue_dropped,
    })
}

impl AlertInput for MqttInput {
    fn stop(&mut self) {
        let Some(handle) = self.handle.take() else {
            return;
        };
        // A client mid-connect cannot be interrupted: let it end itself
        // rather than wait for the connect timeout.
        match handle.stats().status {
            Status::Connected | Status::Backoff | Status::AuthFailed | Status::Refused => {
                handle.stop();
            }
            _ => drop(handle),
        }
    }

    fn status(&self) -> Option<Value> {
        let handle = self.handle.as_ref()?;
        let mut v = serde_json::to_value(handle.stats()).ok()?;
        v["queue_dropped"] = json!(self.queue_dropped.load(Ordering::Relaxed));
        Some(v)
    }
}

impl Drop for MqttInput {
    fn drop(&mut self) {
        self.stop();
    }
}
