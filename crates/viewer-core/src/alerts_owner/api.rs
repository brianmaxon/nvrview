//! The alert service's calls on [`crate::Core`] (docs/FFI.md, "Smart
//! alerts").

use super::store::{LogLines, SNAPSHOT_MAX_BYTES};
use super::{
    Acquisition, AlertError, AlertFrames, ExternalTriggers, InputContext, InputFactory, InputSlot,
    Intake, MAX_PLUGINS, OutputContext, OutputFactory, OutputSlot, Purpose, Secret, camera_json,
    lock, parse_detections,
};
use crate::CoreError;
use alerts::engine::{Engine, Now};
use alerts::model::{CameraId, Kind, Severity, Source};
use alerts::settings::{AlertSettings, OutputKind};
use alerts::trigger::{ExternalBody, Trigger, TriggerState, from_external};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tracing::info;

/// Most entries `alerts_get` answers with.
const MAX_QUERY_LIMIT: usize = 1000;

pub(super) fn core_err(e: CoreError) -> AlertError {
    match e {
        CoreError::Closed => AlertError::Closed,
        _ => AlertError::InvalidArgument,
    }
}

pub(super) fn parse_object(text: &str) -> Result<Value, AlertError> {
    let v: Value = serde_json::from_str(text).map_err(|_| AlertError::InvalidArgument)?;
    if v.is_object() {
        Ok(v)
    } else {
        Err(AlertError::InvalidArgument)
    }
}

/// A camera object `{"device","channel","lens"}` (lens optional).
pub(super) fn parse_camera(v: &Value) -> Result<CameraId, AlertError> {
    let bad = AlertError::InvalidArgument;
    let device = u32::try_from(v["device"].as_u64().ok_or(bad)?).map_err(|_| bad)?;
    let channel = u16::try_from(v["channel"].as_u64().ok_or(bad)?).map_err(|_| bad)?;
    let lens = match v.get("lens") {
        None | Some(Value::Null) => 0,
        Some(l) => u8::try_from(l.as_u64().ok_or(bad)?).map_err(|_| bad)?,
    };
    Ok(CameraId::new(device, channel, lens))
}

fn ms_field(
    v: &Value,
    key: &str,
    default: Duration,
    lo: u64,
    hi: u64,
) -> Result<Duration, AlertError> {
    match v.get(key) {
        None | Some(Value::Null) => Ok(default),
        Some(x) => {
            let n = x.as_u64().ok_or(AlertError::InvalidArgument)?;
            Ok(Duration::from_millis(n.clamp(lo, hi)))
        }
    }
}

fn parse_acquisition(v: &Value) -> Result<Acquisition, AlertError> {
    let d = Acquisition::default();
    if v.is_null() {
        return Ok(d);
    }
    if !v.is_object() {
        return Err(AlertError::InvalidArgument);
    }
    let count = |key: &str, default: u64, lo: u64, hi: u64| -> Result<u64, AlertError> {
        match v.get(key) {
            None | Some(Value::Null) => Ok(default),
            Some(x) => Ok(x.as_u64().ok_or(AlertError::InvalidArgument)?.clamp(lo, hi)),
        }
    };
    let flag = |key: &str, default: bool| -> Result<bool, AlertError> {
        match v.get(key) {
            None | Some(Value::Null) => Ok(default),
            Some(x) => x.as_bool().ok_or(AlertError::InvalidArgument),
        }
    };
    Ok(Acquisition {
        answer_timeout: ms_field(v, "answer_timeout_ms", d.answer_timeout, 500, 60_000)?,
        first_picture_timeout: ms_field(
            v,
            "first_picture_timeout_ms",
            d.first_picture_timeout,
            250,
            30_000,
        )?,
        second_picture: ms_field(v, "second_picture_ms", d.second_picture, 100, 10_000)?,
        picture_interval: ms_field(v, "picture_interval_ms", d.picture_interval, 250, 10_000)?,
        camera_pictures: u8::try_from(count(
            "camera_pictures",
            u64::from(d.camera_pictures),
            1,
            8,
        )?)
        .unwrap_or(2),
        sensor_pictures: u8::try_from(count(
            "sensor_pictures",
            u64::from(d.sensor_pictures),
            1,
            8,
        )?)
        .unwrap_or(4),
        max_wakes: usize::try_from(count("max_wakes", d.max_wakes as u64, 1, 32)?).unwrap_or(8),
        cover_fallback: flag("cover_fallback", d.cover_fallback)?,
        recheck: flag("recheck", d.recheck)?,
    })
}

/// The `secrets` object: credential text by the id of the input or output
/// it belongs to. Values must be strings.
fn parse_secrets(settings: &Value) -> Result<HashMap<String, Secret>, AlertError> {
    let mut out = HashMap::new();
    match settings.get("secrets") {
        None | Some(Value::Null) => {}
        Some(Value::Object(m)) => {
            for (k, v) in m {
                let text = v.as_str().ok_or(AlertError::InvalidArgument)?;
                if out.len() >= MAX_PLUGINS * 2 {
                    return Err(AlertError::InvalidArgument);
                }
                out.insert(k.clone(), Secret::new(text));
            }
        }
        Some(_) => return Err(AlertError::InvalidArgument),
    }
    Ok(out)
}

/// The credential for an `inputs[]` or `outputs[]` entry: keyed by its
/// `id`, else by its `kind`.
fn secret_for(secrets: &HashMap<String, Secret>, entry: &Value) -> Option<Secret> {
    let key = entry
        .get("id")
        .and_then(Value::as_str)
        .or_else(|| entry.get("kind").and_then(Value::as_str))?;
    secrets.get(key).cloned()
}

fn severity_of(text: &str) -> Option<Severity> {
    serde_json::from_value(json!(text)).ok()
}

impl crate::Core {
    /// Registers an input module by the `kind` its settings entries name
    /// (`inputs[]` of [`crate::Core::alerts_configure`]). Replaces an
    /// earlier registration of the same kind.
    pub fn alerts_register_input(&self, kind: &str, factory: InputFactory) {
        lock(&self.alerts.inner.factories)
            .inputs
            .insert(kind.to_owned(), factory);
    }

    /// Registers an output module by the `kind` its settings entries name
    /// (`outputs[]`; one of the engine's output kinds, such as
    /// `home_assistant` or `ntfy`). Replaces an earlier registration.
    pub fn alerts_register_output(&self, kind: &str, factory: OutputFactory) {
        lock(&self.alerts.inner.factories)
            .outputs
            .insert(kind.to_owned(), factory);
    }

    /// Where an input module sends external triggers.
    pub fn alerts_external_triggers(&self) -> ExternalTriggers {
        ExternalTriggers {
            tx: self.alerts.inner.intake.clone(),
            clock: self.alerts.inner.clock.clone(),
        }
    }

    /// Sets the smart-alert settings (docs/NOTIFICATIONS.md, "Settings",
    /// plus `enabled`, `utc_offset_min`, `acquisition`, and `inputs`; see
    /// docs/FFI.md, "Smart alerts"). The first call starts the engine; until
    /// then triggers are ignored. Values out of range are clamped and
    /// reported, never refused: the result arrives as an
    /// `alerts_configured` event. Inputs and outputs whose settings did not
    /// change keep running.
    ///
    /// # Errors
    /// [`AlertError::InvalidArgument`] for JSON that is not a settings
    /// object (or a member of the wrong type), [`AlertError::Closed`].
    pub fn alerts_configure(&self, json: &str) -> Result<(), AlertError> {
        self.check_api().map_err(core_err)?;
        let inner = &self.alerts.inner;
        let v = parse_object(json)?;
        let enabled = match v.get("enabled") {
            None | Some(Value::Null) => true,
            Some(b) => b.as_bool().ok_or(AlertError::InvalidArgument)?,
        };
        let offset_min = match v.get("utc_offset_min") {
            None | Some(Value::Null) => 0,
            Some(o) => {
                let o = o.as_i64().ok_or(AlertError::InvalidArgument)?;
                i32::try_from(o.clamp(-14 * 60, 14 * 60)).unwrap_or(0)
            }
        };
        let acq = parse_acquisition(v.get("acquisition").unwrap_or(&Value::Null))?;
        let evidence_min = match v.pointer("/evidence/retention_min") {
            None | Some(Value::Null) => None,
            Some(m) => Some(super::evidence::clamp_retention_min(
                m.as_i64().ok_or(AlertError::InvalidArgument)?,
            )),
        };
        let night_hold_h = match v.pointer("/evidence/night_hold_h") {
            None | Some(Value::Null) => None,
            Some(h) => Some(
                u32::try_from(
                    h.as_i64()
                        .ok_or(AlertError::InvalidArgument)?
                        .clamp(0, i64::from(super::evidence::MAX_NIGHT_HOLD_H)),
                )
                .unwrap_or(super::evidence::DEFAULT_NIGHT_HOLD_H),
            ),
        };
        let mut issues: Vec<String> = Vec::new();

        // Outputs the engine knows; others are reported and left out.
        let mut doc = v.clone();
        // Credentials are for the modules only: not part of the settings
        // the engine keeps, and never echoed.
        if let Some(o) = doc.as_object_mut() {
            o.remove("secrets");
        }
        let secrets = parse_secrets(&v)?;
        let mut outputs_raw: Vec<Value> = Vec::new();
        if let Some(list) = v.get("outputs") {
            let list = list.as_array().ok_or(AlertError::InvalidArgument)?;
            for (i, o) in list.iter().enumerate() {
                let kind_ok = o
                    .get("kind")
                    .and_then(|k| serde_json::from_value::<OutputKind>(k.clone()).ok())
                    .is_some();
                if kind_ok && outputs_raw.len() < MAX_PLUGINS {
                    outputs_raw.push(o.clone());
                } else {
                    issues.push(format!("output {i}: unknown kind or too many outputs"));
                }
            }
            doc["outputs"] = Value::Array(outputs_raw.clone());
        }
        let settings: AlertSettings =
            serde_json::from_value(doc).map_err(|_| AlertError::InvalidArgument)?;
        let inputs_raw: Vec<Value> = match v.get("inputs") {
            None | Some(Value::Null) => Vec::new(),
            Some(l) => l.as_array().ok_or(AlertError::InvalidArgument)?.clone(),
        };

        // Modules are built and stopped outside the state lock.
        let (factories_in, factories_out): (
            HashMap<String, InputFactory>,
            HashMap<String, OutputFactory>,
        ) = {
            let f = lock(&inner.factories);
            (f.inputs.clone(), f.outputs.clone())
        };
        let (mut old_outputs, mut old_inputs) = {
            let mut st = lock(&inner.state);
            (
                std::mem::take(&mut st.outputs),
                std::mem::take(&mut st.inputs),
            )
        };
        let mut outputs = Vec::new();
        let mut output_report = Vec::new();
        for (i, raw) in outputs_raw.iter().enumerate() {
            let kind_name = raw["kind"].as_str().unwrap_or_default().to_owned();
            let kind: OutputKind =
                serde_json::from_value(raw["kind"].clone()).unwrap_or(OutputKind::Desktop);
            let index = u32::try_from(i).unwrap_or(u32::MAX);
            let secret = secret_for(&secrets, raw);
            if let Some(p) = old_outputs
                .iter()
                .position(|o| o.raw == *raw && o.secret == secret)
            {
                let mut slot = old_outputs.remove(p);
                slot.index = index;
                outputs.push(slot);
                output_report.push(json!({"index": i, "kind": kind_name, "ok": true}));
                continue;
            }
            let Some(f) = factories_out.get(&kind_name) else {
                // Desktop notifications are the app's own (from `alert`
                // events); other kinds need a module.
                if kind != OutputKind::Desktop {
                    output_report.push(json!({"index": i, "kind": kind_name, "ok": false, "error": "no module for this kind"}));
                }
                continue;
            };
            let ctx = OutputContext {
                runtime: inner.runtime.clone(),
                secret: secret.clone(),
            };
            match f(&ctx, raw) {
                Ok(output) => {
                    outputs.push(OutputSlot {
                        index,
                        kind,
                        raw: raw.clone(),
                        secret,
                        output,
                    });
                    output_report.push(json!({"index": i, "kind": kind_name, "ok": true}));
                }
                Err(e) => {
                    output_report
                        .push(json!({"index": i, "kind": kind_name, "ok": false, "error": e}));
                }
            }
        }
        let mut inputs = Vec::new();
        let mut input_report = Vec::new();
        for (i, raw) in inputs_raw.iter().enumerate().take(MAX_PLUGINS) {
            let kind_name = raw["kind"].as_str().unwrap_or_default().to_owned();
            let secret = secret_for(&secrets, raw);
            if let Some(p) = old_inputs
                .iter()
                .position(|o| o.raw == *raw && o.secret == secret)
            {
                inputs.push(old_inputs.remove(p));
                input_report.push(json!({"index": i, "kind": kind_name, "ok": true}));
                continue;
            }
            let Some(f) = factories_in.get(&kind_name) else {
                input_report.push(json!({"index": i, "kind": kind_name, "ok": false, "error": "no module for this kind"}));
                continue;
            };
            let status_events: Arc<dyn Fn(Value) + Send + Sync> = {
                let me = std::sync::Arc::downgrade(inner);
                let kind = kind_name.clone();
                let index = i;
                Arc::new(move |status| {
                    if let Some(inner) = me.upgrade() {
                        inner.send(json!({"type": "alerts_input_status", "index": index,
                                          "kind": kind, "status": status}));
                    }
                })
            };
            let ctx = InputContext {
                runtime: inner.runtime.clone(),
                triggers: self.alerts_external_triggers(),
                secret: secret.clone(),
                status_events,
            };
            match f(&ctx, raw) {
                Ok(input) => {
                    inputs.push(InputSlot {
                        raw: raw.clone(),
                        secret,
                        input,
                    });
                    input_report.push(json!({"index": i, "kind": kind_name, "ok": true}));
                }
                Err(e) => {
                    input_report
                        .push(json!({"index": i, "kind": kind_name, "ok": false, "error": e}));
                }
            }
        }
        for o in old_outputs {
            o.output.shutdown();
        }
        for mut i in old_inputs {
            i.input.stop();
        }

        let mut st = lock(&inner.state);
        if self.closing.load(std::sync::atomic::Ordering::SeqCst) {
            drop(st);
            for o in outputs {
                o.output.shutdown();
            }
            for mut i in inputs {
                i.input.stop();
            }
            return Err(AlertError::Closed);
        }
        let engine_issues = match st.engine.as_mut() {
            Some(e) => e.set_settings(settings),
            None => {
                let (mut e, issues) = Engine::new(settings);
                e.set_next_alert_id(st.next_alert_id);
                st.engine = Some(e);
                issues
            }
        };
        issues.extend(engine_issues.iter().map(ToString::to_string));
        let night_plan = st.engine.as_ref().map(|e| super::evidence::NightPlan {
            location: e.settings().location,
            night: e.settings().night,
            offset_min,
        });
        st.enabled = enabled;
        st.offset_min = offset_min;
        st.acq = acq;
        {
            let now_ms = inner.clock.wall_now_us().div_euclid(1000);
            st.evidence.set_retention_min(
                evidence_min.unwrap_or(super::evidence::DEFAULT_RETENTION_MIN),
                now_ms,
            );
            st.evidence.set_night(
                night_hold_h.unwrap_or(super::evidence::DEFAULT_NIGHT_HOLD_H),
                night_plan,
                now_ms,
            );
        }
        st.outputs = outputs;
        st.inputs = inputs;
        if !enabled {
            // Off: nothing is checked and nothing stays woken.
            let ids: Vec<u64> = st.checks.keys().copied().collect();
            for id in ids {
                st.checks.remove(&id);
                inner.forget_check(&mut st, id);
            }
            let cams: Vec<CameraId> = st.wakes.keys().copied().collect();
            for c in cams {
                inner.stop_wake(&mut st, c);
            }
            let keys: Vec<String> = st.requests.keys().cloned().collect();
            for k in keys {
                inner.drop_request(&mut st, &k, "canceled");
            }
            st.waiting.clear();
        }
        drop(st);
        inner.send(json!({
            "type": "alerts_configured",
            "enabled": enabled,
            "issues": issues,
            "outputs": output_report,
            "inputs": input_report,
        }));
        info!(enabled, "smart alerts configured");
        inner.wake.notify_one();
        Ok(())
    }

    /// Queues a trigger from the app (a source of its own, or a test) and
    /// returns its trigger ID: `{"source":"external","origin":"<key>",
    /// "kind","state"?,"at_ms"?}` or `{"source":"device"|"local_audio"|
    /// "local_motion"|"nvr_input","camera":{...},"kind","state"?,"at_ms"?,
    /// "catch_up"?}`. The decision arrives as events.
    ///
    /// # Errors
    /// [`AlertError::InvalidArgument`] for a malformed trigger,
    /// [`AlertError::NotConfigured`] while the service is off,
    /// [`AlertError::Limit`] when the intake queue is full,
    /// [`AlertError::Closed`].
    pub fn alerts_trigger_put(&self, json: &str) -> Result<u64, AlertError> {
        self.check_api().map_err(core_err)?;
        let inner = &self.alerts.inner;
        let v = parse_object(json)?;
        {
            let st = lock(&inner.state);
            if !st.enabled || st.engine.is_none() {
                return Err(AlertError::NotConfigured);
            }
        }
        let now_ms = inner.clock.wall_now_us().div_euclid(1000);
        let source: Source =
            serde_json::from_value(v["source"].clone()).map_err(|_| AlertError::InvalidArgument)?;
        let kind = v["kind"].as_str().ok_or(AlertError::InvalidArgument)?;
        let state: Option<TriggerState> = match v.get("state") {
            None | Some(Value::Null) => None,
            Some(s) => {
                Some(serde_json::from_value(s.clone()).map_err(|_| AlertError::InvalidArgument)?)
            }
        };
        let at_ms = match v.get("at_ms") {
            None | Some(Value::Null) => None,
            Some(t) => Some(t.as_i64().ok_or(AlertError::InvalidArgument)?),
        };
        let test = v["test"].as_bool().unwrap_or(false);
        let to_ha = test && v["send_to_home_assistant"].as_bool().unwrap_or(false);
        let only_camera = match v.get("only_camera") {
            None | Some(Value::Null) => None,
            Some(c) if test => Some(parse_camera(c)?),
            Some(_) => return Err(AlertError::InvalidArgument),
        };
        let id = inner.new_trigger_id();
        let mut trigger = if source == Source::External {
            let origin = v["origin"].as_str().ok_or(AlertError::InvalidArgument)?;
            let body = ExternalBody {
                kind: kind.to_owned(),
                state: state.unwrap_or(TriggerState::Pulse),
                at_ms,
            };
            from_external(id, origin, &body, now_ms).map_err(|_| AlertError::InvalidArgument)?
        } else {
            let camera = parse_camera(&v["camera"])?;
            let kind = Kind::parse(kind).ok_or(AlertError::InvalidArgument)?;
            Trigger {
                id,
                source,
                origin: None,
                camera: Some(camera),
                kind,
                state: state.unwrap_or(TriggerState::Start),
                at_ms: at_ms.map_or(now_ms, |t| t.min(now_ms)),
                received_ms: now_ms,
                catch_up: v["catch_up"].as_bool().unwrap_or(false),
                only_camera: None,
                test: false,
            }
        };
        trigger.only_camera = only_camera;
        trigger.test = test;
        if test {
            let mut st = lock(&inner.state);
            st.tests.insert(id, to_ha);
            while st.tests.len() > super::MAX_TESTS {
                st.tests.pop_first();
            }
        }
        inner
            .intake
            .try_send(Intake::Full(trigger))
            .map_err(|e| match e {
                tokio::sync::mpsc::error::TrySendError::Full(_) => AlertError::Limit,
                tokio::sync::mpsc::error::TrySendError::Closed(_) => AlertError::Closed,
            })?;
        Ok(id)
    }

    /// The frames of a verification request: Annex-B access units from a
    /// keyframe (parameter sets included) to the picture to check. Never
    /// waits on a device.
    ///
    /// # Errors
    /// [`AlertError::UnknownId`] for a request answered, expired, or never
    /// sent; [`AlertError::Closed`].
    pub fn alert_frames_get(&self, request: &str) -> Result<AlertFrames, AlertError> {
        self.check_api().map_err(core_err)?;
        let st = lock(&self.alerts.inner.state);
        st.requests
            .get(request)
            .map(|r| r.bundle.frames())
            .ok_or(AlertError::UnknownId)
    }

    /// The detector's boxes for a verification request:
    /// `{"detections":[{"class":"person","score":0.8,"box":{"x","y","w","h"}}]}`
    /// in normalized coordinates of the whole picture (boxes found in crops
    /// mapped back). `class` may also be a detector label (`car`, `dog`);
    /// labels without an alert class are ignored. An empty list means
    /// nothing was found; a picture that could not be decoded is simply
    /// not answered (it then counts as not checked).
    ///
    /// # Errors
    /// [`AlertError::InvalidArgument`] for malformed boxes,
    /// [`AlertError::UnknownId`] for a request answered, expired, or never
    /// sent; [`AlertError::Closed`].
    pub fn alert_detections_put(&self, request: &str, json: &str) -> Result<(), AlertError> {
        self.check_api().map_err(core_err)?;
        let detections = parse_detections(json)?;
        self.alerts.inner.answer(request, detections)
    }

    /// Stores the app's picture (usually a JPEG with the boxes drawn) for an
    /// alert, replacing an earlier one; outputs that want pictures get it.
    ///
    /// # Errors
    /// [`AlertError::Limit`] above 1 MiB, [`AlertError::UnknownId`] for an
    /// alert ID never given, [`AlertError::InvalidArgument`] for empty data,
    /// [`AlertError::Internal`] when it cannot be written,
    /// [`AlertError::Closed`].
    pub fn alert_snapshot_put(&self, alert_id: u64, data: &[u8]) -> Result<(), AlertError> {
        self.check_api().map_err(core_err)?;
        if data.is_empty() {
            return Err(AlertError::InvalidArgument);
        }
        if data.len() > SNAPSHOT_MAX_BYTES {
            return Err(AlertError::Limit);
        }
        let inner = &self.alerts.inner;
        let mut st = lock(&inner.state);
        if alert_id == 0 || alert_id >= st.next_alert_id {
            return Err(AlertError::UnknownId);
        }
        let now_ms = inner.clock.wall_now_us().div_euclid(1000);
        if !st.snapshots.put(alert_id, data, now_ms) {
            return Err(AlertError::Internal);
        }
        drop(st);
        inner.wake.notify_one();
        Ok(())
    }

    /// The stored picture of an alert.
    ///
    /// # Errors
    /// [`AlertError::UnknownId`] when there is none (never put, or dropped
    /// by the cache's bounds), [`AlertError::Closed`].
    pub fn alert_snapshot_get(&self, alert_id: u64) -> Result<Vec<u8>, AlertError> {
        self.check_api().map_err(core_err)?;
        lock(&self.alerts.inner.state)
            .snapshots
            .get(alert_id)
            .ok_or(AlertError::UnknownId)
    }

    /// Feedback on an alert: `{"alert_id":4,"vote":"up"|"down","reason":
    /// "nothing_there"|"wrong_type"|"not_interesting"|"duplicate"}` (a
    /// reason only with "down"). Stored locally and shown by `alerts_get`;
    /// learning from it is a later slice.
    ///
    /// # Errors
    /// [`AlertError::InvalidArgument`], [`AlertError::UnknownId`] for an
    /// alert ID never given, [`AlertError::Closed`].
    pub fn alert_feedback(&self, json: &str) -> Result<(), AlertError> {
        self.check_api().map_err(core_err)?;
        let v = parse_object(json)?;
        let bad = AlertError::InvalidArgument;
        let alert_id = v["alert_id"].as_u64().ok_or(bad)?;
        let vote = v["vote"].as_str().ok_or(bad)?;
        if vote != "up" && vote != "down" {
            return Err(bad);
        }
        let reason = match v.get("reason") {
            None | Some(Value::Null) => None,
            Some(r) => {
                let r = r.as_str().ok_or(bad)?;
                let known = [
                    "nothing_there",
                    "wrong_type",
                    "not_interesting",
                    "duplicate",
                ];
                if vote != "down" || !known.contains(&r) {
                    return Err(bad);
                }
                Some(r)
            }
        };
        let inner = &self.alerts.inner;
        let mut st = lock(&inner.state);
        if alert_id == 0 || alert_id >= st.next_alert_id {
            return Err(AlertError::UnknownId);
        }
        let at_ms = inner.clock.wall_now_us().div_euclid(1000);
        let mut entry = json!({"alert_id": alert_id, "vote": vote, "at_ms": at_ms});
        if let Some(r) = reason {
            entry["reason"] = json!(r);
        }
        st.feedback.put(alert_id, entry);
        Ok(())
    }

    /// Mutes a camera for `seconds` (at most 24 h; 0 ends a mute). A mute
    /// is the only thing that stops always-alert rules.
    ///
    /// # Errors
    /// [`AlertError::InvalidArgument`] for a bad camera,
    /// [`AlertError::NotConfigured`] before the first configure,
    /// [`AlertError::Closed`].
    pub fn alert_mute(&self, camera_json: &str, seconds: u32) -> Result<(), AlertError> {
        self.check_api().map_err(core_err)?;
        let camera = parse_camera(&parse_object(camera_json)?)?;
        let inner = &self.alerts.inner;
        let mut st = lock(&inner.state);
        let now = Now::new(inner.clock.wall_now_us().div_euclid(1000), st.offset_min);
        let Some(engine) = st.engine.as_mut() else {
            return Err(AlertError::NotConfigured);
        };
        if seconds == 0 {
            engine.unmute(camera);
            st.mutes.remove(&camera);
        } else {
            engine.mute(camera, now.ms + i64::from(seconds) * 1000, now);
            match engine.muted(camera, now.ms) {
                Some(until) => {
                    st.mutes.insert(camera, until);
                }
                None => {
                    st.mutes.remove(&camera);
                }
            }
        }
        Ok(())
    }

    /// Today's sunrise and sunset and the night that starts this evening,
    /// for the settings screen. `request` is `{"lat":..,"lon":..,"at_ms":..,
    /// "utc_offset_min":..,"after_sunset_min":..,"before_sunrise_min":..}`
    /// (the offsets and margins optional, 0); the date is the local date of
    /// `at_ms` at the offset. Answers `{"date":"YYYY-MM-DD","kind":
    /// "normal"|"polar_day"|"polar_night","sunrise_ms":..,"sunset_ms":..,
    /// "night_start_ms":..,"night_end_ms":..}`; members that do not exist
    /// that day are null. Needs no state; never waits.
    ///
    /// # Errors
    /// [`AlertError::InvalidArgument`] for malformed JSON, a missing or
    /// non-numeric `lat`, `lon`, or `at_ms`, or a location out of range,
    /// [`AlertError::Closed`].
    pub fn alerts_sun_times(&self, request: &str) -> Result<String, AlertError> {
        self.check_api().map_err(core_err)?;
        let q = parse_object(request)?;
        let bad = AlertError::InvalidArgument;
        let lat = q.get("lat").and_then(Value::as_f64).ok_or(bad)?;
        let lon = q.get("lon").and_then(Value::as_f64).ok_or(bad)?;
        let at_ms = q.get("at_ms").and_then(Value::as_i64).ok_or(bad)?;
        if !(-90.0..=90.0).contains(&lat) || !(-180.0..=180.0).contains(&lon) {
            return Err(bad);
        }
        let int = |k: &str, max: i64| -> Result<i64, AlertError> {
            match q.get(k) {
                None | Some(Value::Null) => Ok(0),
                Some(x) => x.as_i64().map(|n| n.clamp(-max, max)).ok_or(bad),
            }
        };
        let offset = i32::try_from(int("utc_offset_min", 14 * 60)?).unwrap_or(0);
        let after = u16::try_from(int("after_sunset_min", 1440)?.max(0)).unwrap_or(0);
        let before = u16::try_from(int("before_sunrise_min", 1440)?.max(0)).unwrap_or(0);
        let date = alerts::time::local(at_ms, offset).date;
        let t = alerts::sun::sun_times(date, lat, lon, after, before);
        let (kind, rise, set) = match t.day {
            alerts::sun::SunDay::Normal { rise_ms, set_ms } => {
                ("normal", Some(rise_ms), Some(set_ms))
            }
            alerts::sun::SunDay::PolarDay => ("polar_day", None, None),
            alerts::sun::SunDay::PolarNight => ("polar_night", None, None),
        };
        Ok(json!({
            "date": format!("{:04}-{:02}-{:02}", date.year, date.month, date.day),
            "kind": kind,
            "sunrise_ms": rise,
            "sunset_ms": set,
            "night_start_ms": t.night_start_ms,
            "night_end_ms": t.night_end_ms,
        })
        .to_string())
    }

    /// The event log, newest first, for the Events view: `query` is
    /// `{"limit":100,"before_ms":..,"since_ms":..,"until_ms":..,
    /// "camera":{...},"device":n,"decision":["alert","quiet","suppress"],
    /// "min_severity":"normal","source":["device","external",...],
    /// "class":["person"],"text":"..","alert_id":n,"trigger_id":n}` (all
    /// optional). Answers `{"entries":[...],"more":bool,"status":{...}}`;
    /// each entry is a logged decision with its trace, `deliveries`,
    /// `snapshot` (whether a picture is stored), and `feedback`.
    ///
    /// # Errors
    /// [`AlertError::InvalidArgument`] for a malformed query,
    /// [`AlertError::Closed`].
    pub fn alerts_get(&self, query: &str) -> Result<String, AlertError> {
        self.check_api().map_err(core_err)?;
        let q = if query.trim().is_empty() {
            json!({})
        } else {
            parse_object(query)?
        };
        let bad = AlertError::InvalidArgument;
        let limit = match q.get("limit") {
            None | Some(Value::Null) => 100,
            Some(l) => usize::try_from(l.as_u64().ok_or(bad)?).unwrap_or(MAX_QUERY_LIMIT),
        }
        .min(MAX_QUERY_LIMIT);
        let num = |k: &str| -> Result<Option<i64>, AlertError> {
            match q.get(k) {
                None | Some(Value::Null) => Ok(None),
                Some(x) => x.as_i64().map(Some).ok_or(bad),
            }
        };
        let (before, since, until) = (num("before_ms")?, num("since_ms")?, num("until_ms")?);
        let alert_id = num("alert_id")?;
        let trigger_id = num("trigger_id")?;
        let device = num("device")?;
        let camera = match q.get("camera") {
            None | Some(Value::Null) => None,
            Some(c) => Some(camera_json(parse_camera(c)?)),
        };
        let strings = |k: &str| -> Result<Option<Vec<String>>, AlertError> {
            match q.get(k) {
                None | Some(Value::Null) => Ok(None),
                Some(Value::Array(a)) => a
                    .iter()
                    .map(|s| s.as_str().map(str::to_owned).ok_or(bad))
                    .collect::<Result<Vec<_>, _>>()
                    .map(Some),
                Some(_) => Err(bad),
            }
        };
        let decisions = strings("decision")?;
        let sources = strings("source")?;
        let classes = strings("class")?;
        let min_severity = match q.get("min_severity") {
            None | Some(Value::Null) => None,
            Some(s) => Some(severity_of(s.as_str().ok_or(bad)?).ok_or(bad)?),
        };
        let text = q["text"].as_str().map(str::to_lowercase);
        let sensor = q["sensor"].as_str().map(str::to_owned);
        let test_only = q["test"].as_bool();

        let inner = &self.alerts.inner;
        let (reader, status) = {
            let st = lock(&inner.state);
            (st.log.reader(), self.alerts_status(&st))
        };
        let lines: LogLines = reader.lines_newest_first();
        let mut deliveries: HashMap<u64, Vec<Value>> = HashMap::new();
        let mut entries = Vec::new();
        let mut more = false;
        for l in lines {
            let Ok(mut v) = serde_json::from_str::<Value>(&l) else {
                continue;
            };
            if let Some(d) = v.get("delivery") {
                if let Some(t) = d["trigger_id"].as_u64() {
                    let list = deliveries.entry(t).or_default();
                    if list.len() < 16 {
                        list.push(d["step"].clone());
                    }
                }
                continue;
            }
            let d = &v["decision"];
            if d.is_null() {
                continue;
            }
            let decided = d["decided_ms"].as_i64().unwrap_or(0);
            let keep = before.is_none_or(|b| decided < b)
                && since.is_none_or(|s| decided >= s)
                && until.is_none_or(|u| decided <= u)
                && alert_id.is_none_or(|a| d["alert_id"].as_i64() == Some(a))
                && trigger_id.is_none_or(|t| d["trigger_id"].as_i64() == Some(t))
                && device.is_none_or(|n| d["camera"]["device"].as_i64() == Some(n))
                && camera.as_ref().is_none_or(|c| {
                    let lens = d["camera"]["lens"].as_u64().unwrap_or(0);
                    d["camera"]["device"] == c["device"]
                        && d["camera"]["channel"] == c["channel"]
                        && json!(lens) == c["lens"]
                })
                && decisions
                    .as_ref()
                    .is_none_or(|l| l.iter().any(|x| d["decision"].as_str() == Some(x)))
                && sources
                    .as_ref()
                    .is_none_or(|l| l.iter().any(|x| v["source"].as_str() == Some(x)))
                && classes
                    .as_ref()
                    .is_none_or(|l| l.iter().any(|x| d["class"].as_str() == Some(x)))
                && sensor
                    .as_ref()
                    .is_none_or(|k| v["sensor"].as_str() == Some(k))
                && test_only.is_none_or(|t| v["test"].as_bool().unwrap_or(false) == t)
                && min_severity.is_none_or(|m| {
                    d["severity"]
                        .as_str()
                        .and_then(severity_of)
                        .is_some_and(|s| s >= m)
                })
                && text.as_ref().is_none_or(|t| {
                    d["explanation"]
                        .as_str()
                        .is_some_and(|e| e.to_lowercase().contains(t))
                });
            if !keep {
                continue;
            }
            if entries.len() >= limit {
                more = true;
                break;
            }
            let trig = d["trigger_id"].as_u64().unwrap_or(0);
            let alert = d["alert_id"].as_u64();
            let mut steps = deliveries.remove(&trig).unwrap_or_default();
            steps.reverse();
            v["deliveries"] = Value::Array(steps);
            let (snapshot, feedback) = match alert {
                Some(a) => {
                    let st = lock(&inner.state);
                    (st.snapshots.has(a), st.feedback.get(a).cloned())
                }
                None => (false, None),
            };
            v["snapshot"] = json!(snapshot);
            if let Some(f) = feedback {
                v["feedback"] = f;
            }
            entries.push(v);
        }
        Ok(json!({"entries": entries, "more": more, "status": status}).to_string())
    }

    fn alerts_status(&self, st: &super::State) -> Value {
        let e = st.engine.as_ref().map(Engine::status);
        let now_ms = self.alerts.inner.clock.wall_now_us().div_euclid(1000);
        let muted: Vec<Value> = st
            .mutes
            .iter()
            .filter(|(_, u)| **u > now_ms)
            .map(|(c, u)| json!({"camera": camera_json(*c), "until_ms": u}))
            .collect();
        json!({
            "configured": st.engine.is_some(),
            "enabled": st.enabled,
            "open_checks": e.as_ref().map_or(0, |s| s.open_checks),
            "episodes": e.as_ref().map_or(0, |s| s.episodes),
            "muted": muted,
            "woken": st.wakes.len(),
            "waiting": st.waiting.len(),
            "requests": st.requests.len(),
            "rechecks": st.requests.values().filter(|r| r.purpose == Purpose::Recheck).count(),
            "outputs": st.outputs.len(),
            "inputs": st.inputs.len(),
            "evidence": {
                "sets": st.evidence.counts().0,
                "pinned": st.evidence.counts().1,
                "bytes": st.evidence.counts().2,
                "retention_min": st.evidence.retention_min(),
                "night_hold_h": st.evidence.night_hold_h(),
            },
            "input_status": st.inputs.iter().enumerate().map(|(i, s)| json!({
                "index": i,
                "id": s.raw["id"],
                "kind": s.raw["kind"],
                "status": s.input.status(),
            })).collect::<Vec<_>>(),
        })
    }

    /// Cameras woken for checks right now (for tests and diagnostics).
    pub fn alerts_woken(&self) -> Vec<(u32, u16, u8)> {
        lock(&self.alerts.inner.state)
            .wakes
            .keys()
            .map(|c| (c.device, c.channel, c.lens))
            .collect()
    }
}
