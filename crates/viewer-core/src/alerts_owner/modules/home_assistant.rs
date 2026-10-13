//! The `home_assistant` output: decisions to Home Assistant's REST API
//! through `crates/ha-output`, run on its worker thread behind a bounded
//! queue so a slow server never blocks the alert service.
//!
//! The `outputs[]` entry carries `kind`, `enabled`, `endpoint` (the base
//! URL), `min_severity`, and `pictures` like every output, plus the
//! `ha-output` settings (`mode`, `rules`, `pulse_gap_ms`, `cooldown_ms`,
//! `message_template`, `max_attempts`, `backoff_ms`, `timeout_ms`). The
//! long-lived access token is the credential the app passes in `secrets`
//! under the entry's `id` (else `home_assistant`).

use crate::alerts_owner::{
    AlertNotice, AlertOutput, DeliveryDone, NoticeState, OutputContext, OutputFactory,
};
use alerts::model::{CameraId, Severity};
use ha_output::{Alert, Clock, Config, DecisionKind, HaOutput, Http, OutputResult, Token, Worker};
use serde_json::Value;
use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};
use tracing::info;

/// Alerts waiting for Home Assistant at once; more are dropped and reported
/// as failed deliveries.
const QUEUE: usize = 16;

type Pending = Arc<Mutex<HashMap<(u64, u32), DeliveryDone>>>;

/// The factory the core registers for the `home_assistant` kind. With
/// `None` it uses the real network and clock; tests pass their own.
pub fn home_assistant_factory(
    http: Option<Arc<dyn Http>>,
    clock: Option<Arc<dyn Clock>>,
) -> OutputFactory {
    Arc::new(move |ctx, raw| {
        build(ctx, raw, http.clone(), clock.clone()).map(|o| o as Arc<dyn AlertOutput>)
    })
}

struct HomeAssistant {
    worker: Mutex<Option<Worker>>,
    pending: Pending,
}

fn config_of(raw: &Value) -> Result<Config, String> {
    let mut obj = raw
        .as_object()
        .cloned()
        .ok_or_else(|| "Home Assistant settings must be an object".to_owned())?;
    let endpoint = obj
        .get("endpoint")
        .and_then(Value::as_str)
        .ok_or_else(|| "Home Assistant needs an endpoint (its base address)".to_owned())?
        .to_owned();
    for k in [
        "kind",
        "id",
        "enabled",
        "endpoint",
        "min_severity",
        "pictures",
    ] {
        obj.remove(k);
    }
    obj.insert("base_url".to_owned(), Value::String(endpoint));
    serde_json::from_value(Value::Object(obj))
        .map_err(|_| "Home Assistant settings are malformed".to_owned())
}

fn build(
    ctx: &OutputContext,
    raw: &Value,
    http: Option<Arc<dyn Http>>,
    clock: Option<Arc<dyn Clock>>,
) -> Result<Arc<HomeAssistant>, String> {
    let config = config_of(raw)?;
    let secret = ctx
        .secret
        .as_ref()
        .ok_or_else(|| "Home Assistant has no access token (add it in the keychain)".to_owned())?;
    let token = Token::new(secret.expose());
    let output = match (http, clock) {
        (Some(h), Some(c)) => HaOutput::with_parts(config, token, h, c),
        _ => HaOutput::new(config, token),
    }
    .map_err(|e| format!("Home Assistant settings: {e}"))?;
    let pending: Pending = Arc::new(Mutex::new(HashMap::new()));
    let p = pending.clone();
    let worker = Worker::spawn(Arc::new(output), QUEUE, move |alert, result| {
        let done = p
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(&(alert.alert_id, alert.rev));
        if let Some(done) = done {
            done.finish_with(result.ok, Some(summary(&result)));
        }
    });
    info!("Home Assistant output started");
    Ok(Arc::new(HomeAssistant {
        worker: Mutex::new(Some(worker)),
        pending,
    }))
}

/// The step's result line: the crate's summary, else its per-target ones.
fn summary(result: &OutputResult) -> String {
    if result.entities.is_empty() {
        return result.summary.clone();
    }
    result
        .entities
        .iter()
        .map(|e| e.summary.as_str())
        .collect::<Vec<_>>()
        .join("; ")
}

fn alert_of(n: &AlertNotice) -> Alert {
    Alert {
        alert_id: n.alert_id,
        rev: n.rev,
        camera: n.camera.unwrap_or(CameraId::new(0, 0, 0)),
        camera_label: n.camera_name.clone(),
        class: n.class,
        source: n.source.clone(),
        severity: n.severity.unwrap_or(Severity::Normal),
        decision: DecisionKind::Alert,
        at_ms: n.at_ms,
        utc_offset_min: n.utc_offset_min,
        explanation: n.explanation.clone(),
    }
}

impl AlertOutput for HomeAssistant {
    fn deliver(&self, notice: &AlertNotice, done: DeliveryDone) {
        if notice.state == NoticeState::Settled {
            done.finish_with(true, Some("Home Assistant: nothing to send".to_owned()));
            return;
        }
        let alert = alert_of(notice);
        let key = (alert.alert_id, alert.rev);
        self.pending
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(key, done);
        let submitted = match self
            .worker
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .as_ref()
        {
            Some(w) => w.submit(alert).map_err(|e| format!("{e:?}")),
            None => Err("closed".to_owned()),
        };
        if let Err(why) = submitted {
            let done = self
                .pending
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .remove(&key);
            if let Some(done) = done {
                done.finish_with(
                    false,
                    Some(format!("Home Assistant: alert dropped ({why})")),
                );
            }
        }
    }

    fn shutdown(&self) {
        let worker = self
            .worker
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
        if let Some(w) = worker {
            // Finishing the queue can take a pulse gap per alert: do it
            // off the caller's thread.
            let _ = std::thread::Builder::new()
                .name("ha-output-stop".to_owned())
                .spawn(move || w.shutdown());
        }
    }
}
