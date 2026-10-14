//! The calls for debugging and reviewing smart alerts (docs/FFI.md, "Smart
//! alerts", minor 14): a sensor's message log, a test alert through an
//! output, and the verification evidence store.

use super::api::{core_err, parse_camera, parse_object};
use super::evidence::{MAX_FRAME_BYTES, MAX_FRAMES};
use super::seams::{TargetsResult, TestResult};
use super::{AlertError, AlertNotice, NoticeState, lock};
use alerts::model::{Class, Severity};
use serde_json::{Value, json};
use std::sync::atomic::Ordering;

/// Entries one `alerts_messages` page may hold.
const MAX_MESSAGE_PAGE: usize = 500;

impl crate::Core {
    /// A trigger input's message log. `{"input":"<id>","after":0,
    /// "limit":100,"clear":false}`: the entries with a sequence number at
    /// or after `after` (oldest first), as the input makes them (for an
    /// MQTT sensor: `seq`, `at_ms`, `topic`, a shortened `payload`,
    /// `outcome`), plus `next_seq`, `total` and `more`. With `"clear":
    /// true` the log is emptied first. The log is in memory only: never
    /// written to disk, to events, or to the core's log.
    ///
    /// # Errors
    /// [`AlertError::InvalidArgument`], [`AlertError::UnknownId`] for no
    /// input with that `id` (or one that keeps no log),
    /// [`AlertError::Closed`].
    pub fn alerts_messages(&self, request: &str) -> Result<String, AlertError> {
        self.check_api().map_err(core_err)?;
        let v = parse_object(request)?;
        let bad = AlertError::InvalidArgument;
        let id = v["input"].as_str().ok_or(bad)?;
        let after = v.get("after").and_then(Value::as_u64).unwrap_or(0);
        let limit = match v.get("limit") {
            None | Some(Value::Null) => 100,
            Some(l) => usize::try_from(l.as_u64().ok_or(bad)?).unwrap_or(MAX_MESSAGE_PAGE),
        }
        .clamp(1, MAX_MESSAGE_PAGE);
        let st = lock(&self.alerts.inner.state);
        let slot = st
            .inputs
            .iter()
            .find(|s| s.raw["id"].as_str() == Some(id))
            .ok_or(AlertError::UnknownId)?;
        if v["clear"].as_bool().unwrap_or(false) {
            slot.input.clear_messages();
        }
        let page = slot
            .input
            .messages(after, limit)
            .ok_or(AlertError::UnknownId)?;
        Ok(page.to_string())
    }

    /// Sends one test alert through a configured output now. `{"output":
    /// <index in outputs[]>,"rule":<index>?,"camera":{...}?,"class":
    /// "person"?}`. It returns at once; the result arrives as an
    /// `alerts_output_test` event: `{"type","output","kind","ok",
    /// "summary","http_status"?,"at_ms"}`. Rules are tested whether or not
    /// they would match, and their cooldown does not apply.
    ///
    /// # Errors
    /// [`AlertError::InvalidArgument`], [`AlertError::NotConfigured`],
    /// [`AlertError::UnknownId`] for no such output,
    /// [`AlertError::Closed`].
    pub fn alerts_output_test(&self, request: &str) -> Result<(), AlertError> {
        self.check_api().map_err(core_err)?;
        let v = parse_object(request)?;
        let bad = AlertError::InvalidArgument;
        let index = u32::try_from(v["output"].as_u64().ok_or(bad)?).map_err(|_| bad)?;
        let rule = match v.get("rule") {
            None | Some(Value::Null) => None,
            Some(r) => Some(usize::try_from(r.as_u64().ok_or(bad)?).map_err(|_| bad)?),
        };
        let camera = match v.get("camera") {
            None | Some(Value::Null) => None,
            Some(c) => Some(parse_camera(c)?),
        };
        let class: Class = match v.get("class") {
            None | Some(Value::Null) => Class::Person,
            Some(c) => serde_json::from_value(c.clone()).map_err(|_| bad)?,
        };
        let inner = &self.alerts.inner;
        let now_ms = inner.clock.wall_now_us().div_euclid(1000);
        let (output, kind, notice) = {
            let st = lock(&inner.state);
            let Some(engine) = st.engine.as_ref() else {
                return Err(AlertError::NotConfigured);
            };
            let slot = st
                .outputs
                .iter()
                .find(|o| o.index == index)
                .ok_or(AlertError::UnknownId)?;
            let name = camera.map_or_else(
                || "Test camera".to_owned(),
                |c| engine.settings().camera(c).display_name(),
            );
            let notice = AlertNotice {
                state: NoticeState::New,
                alert_id: 0,
                rev: 1,
                camera,
                camera_name: name,
                source: None,
                utc_offset_min: st.offset_min,
                class,
                severity: Some(Severity::Normal),
                unverified: false,
                late: false,
                always: None,
                explanation: "test alert from nvrview".to_owned(),
                at_ms: now_ms,
                decision_json: String::new(),
                snapshot: None,
                test: true,
            };
            (slot.output.clone(), slot.raw["kind"].clone(), notice)
        };
        let me = std::sync::Arc::downgrade(inner);
        let clock = inner.clock.clone();
        output.send_test(
            &notice,
            rule,
            Box::new(move |r: TestResult| {
                if let Some(inner) = me.upgrade() {
                    let mut ev = json!({
                        "type": "alerts_output_test",
                        "output": index,
                        "kind": kind,
                        "ok": r.ok,
                        "summary": r.summary,
                        "at_ms": clock.wall_now_us().div_euclid(1000),
                    });
                    if let Some(s) = r.http_status {
                        ev["http_status"] = json!(s);
                    }
                    inner.send(ev);
                }
            }),
        );
        Ok(())
    }

    /// Asks a configured output which targets it offers, for a settings
    /// menu (Home Assistant: notify entities and legacy notify services,
    /// fetched with the stored token). `{"output":<index in outputs[]>}`.
    /// It returns at once; the result arrives as an `alerts_output_targets`
    /// event: `{"type","output","kind","ok","summary","targets":[{"name",
    /// "kind":"entity"|"service"}],"at_ms"}`. `summary` is the reason on
    /// failure. An output with no list reports `ok` false.
    ///
    /// # Errors
    /// [`AlertError::InvalidArgument`], [`AlertError::NotConfigured`],
    /// [`AlertError::UnknownId`] for no such output,
    /// [`AlertError::Closed`].
    pub fn alerts_output_targets(&self, request: &str) -> Result<(), AlertError> {
        self.check_api().map_err(core_err)?;
        let v = parse_object(request)?;
        let bad = AlertError::InvalidArgument;
        let index = u32::try_from(v["output"].as_u64().ok_or(bad)?).map_err(|_| bad)?;
        let inner = &self.alerts.inner;
        let (output, kind) = {
            let st = lock(&inner.state);
            if st.engine.is_none() {
                return Err(AlertError::NotConfigured);
            }
            let slot = st
                .outputs
                .iter()
                .find(|o| o.index == index)
                .ok_or(AlertError::UnknownId)?;
            (slot.output.clone(), slot.raw["kind"].clone())
        };
        let me = std::sync::Arc::downgrade(inner);
        let clock = inner.clock.clone();
        output.list_targets(Box::new(move |r: TargetsResult| {
            if let Some(inner) = me.upgrade() {
                let targets: Vec<Value> = r
                    .targets
                    .iter()
                    .map(|t| json!({"name": t.name, "kind": t.kind}))
                    .collect();
                inner.send(json!({
                    "type": "alerts_output_targets",
                    "output": index,
                    "kind": kind,
                    "ok": r.ok,
                    "summary": r.summary,
                    "targets": targets,
                    "at_ms": clock.wall_now_us().div_euclid(1000),
                }));
            }
        }));
        Ok(())
    }

    /// Stores evidence for a trigger's check. `json` is `{"trigger_id":N,
    /// "frame":k?,"at_ms":ms?,"doc":{...}?}` and `data` the JPEG for frame
    /// `k` (at most eight frames of 1 MiB per set). `doc` is merged into
    /// the set's document (the app puts the camera AI's claim, its own
    /// detector's boxes, anything else that ran, under `methods`); the
    /// core adds the decision when it is made, so put the evidence before
    /// answering the detections. Sets expire (see the evidence retention
    /// setting) and are capped by count and size.
    ///
    /// # Errors
    /// [`AlertError::InvalidArgument`] (no frame or doc, bad values),
    /// [`AlertError::UnknownId`] for a trigger never given,
    /// [`AlertError::Limit`] for a frame above 1 MiB or a ninth frame,
    /// [`AlertError::Internal`], [`AlertError::Closed`].
    pub fn alert_evidence_put(&self, json: &str, data: &[u8]) -> Result<(), AlertError> {
        self.check_api().map_err(core_err)?;
        let v = parse_object(json)?;
        let bad = AlertError::InvalidArgument;
        let trigger_id = v["trigger_id"].as_u64().ok_or(bad)?;
        let inner = &self.alerts.inner;
        if trigger_id == 0 || trigger_id >= inner.next_trigger.load(Ordering::Relaxed) {
            return Err(AlertError::UnknownId);
        }
        let frame = match v.get("frame") {
            None | Some(Value::Null) => None,
            Some(f) => Some(u32::try_from(f.as_u64().ok_or(bad)?).map_err(|_| bad)?),
        };
        let doc = v.get("doc").filter(|d| !d.is_null());
        if frame.is_none() && doc.is_none() {
            return Err(bad);
        }
        let now_ms = inner.clock.wall_now_us().div_euclid(1000);
        let at_ms = v.get("at_ms").and_then(Value::as_i64).unwrap_or(now_ms);
        let mut st = lock(&inner.state);
        if let Some(index) = frame {
            if data.is_empty() {
                return Err(bad);
            }
            if data.len() > MAX_FRAME_BYTES {
                return Err(AlertError::Limit);
            }
            if !st
                .evidence
                .put_frame(trigger_id, index, at_ms, data, now_ms)
            {
                return Err(if index as usize >= MAX_FRAMES {
                    AlertError::Limit
                } else {
                    AlertError::Internal
                });
            }
        }
        if let Some(d) = doc
            && !st.evidence.merge_doc(trigger_id, d, now_ms)
        {
            return Err(bad);
        }
        Ok(())
    }

    /// Evidence as JSON. `{"trigger_id":N}` answers one set (its document,
    /// `created_ms`, `expires_ms`, `pinned`, `bytes`, `frames`); `{}` or
    /// `{"since_ms":ms}` answers `{"sets":[...],"retention_min":n}`, newest
    /// first.
    ///
    /// # Errors
    /// [`AlertError::InvalidArgument`], [`AlertError::UnknownId`] for a
    /// set that is not there (never kept, or expired),
    /// [`AlertError::Closed`].
    pub fn alert_evidence_get(&self, query: &str) -> Result<String, AlertError> {
        self.check_api().map_err(core_err)?;
        let q = if query.trim().is_empty() {
            json!({})
        } else {
            parse_object(query)?
        };
        let inner = &self.alerts.inner;
        let now_ms = inner.clock.wall_now_us().div_euclid(1000);
        let mut st = lock(&inner.state);
        match q.get("trigger_id") {
            None | Some(Value::Null) => {
                let since = q.get("since_ms").and_then(Value::as_i64);
                let sets = st.evidence.list(since, now_ms);
                Ok(json!({"sets": sets, "retention_min": st.evidence.retention_min()}).to_string())
            }
            Some(id) => {
                let id = id.as_u64().ok_or(AlertError::InvalidArgument)?;
                st.evidence
                    .get(id, now_ms)
                    .map(|v| v.to_string())
                    .ok_or(AlertError::UnknownId)
            }
        }
    }

    /// One kept frame (JPEG).
    ///
    /// # Errors
    /// [`AlertError::UnknownId`] when the set or frame is not there,
    /// [`AlertError::Closed`].
    pub fn alert_evidence_frame_get(
        &self,
        trigger_id: u64,
        index: u32,
    ) -> Result<Vec<u8>, AlertError> {
        self.check_api().map_err(core_err)?;
        let inner = &self.alerts.inner;
        let now_ms = inner.clock.wall_now_us().div_euclid(1000);
        lock(&inner.state)
            .evidence
            .frame(trigger_id, index, now_ms)
            .ok_or(AlertError::UnknownId)
    }

    /// Pins a set ("Keep"): it leaves the short-lived store and keeps the
    /// per-alert snapshot limits (7 days, 256 sets, 64 MiB). When the
    /// decision was an alert without a picture, the set's first frame also
    /// becomes the alert's snapshot.
    ///
    /// # Errors
    /// [`AlertError::UnknownId`] for a set that is not there,
    /// [`AlertError::Internal`], [`AlertError::Closed`].
    pub fn alert_evidence_pin(&self, trigger_id: u64) -> Result<(), AlertError> {
        self.check_api().map_err(core_err)?;
        let inner = &self.alerts.inner;
        let now_ms = inner.clock.wall_now_us().div_euclid(1000);
        let mut st = lock(&inner.state);
        let Some(set) = st.evidence.get(trigger_id, now_ms) else {
            return Err(AlertError::UnknownId);
        };
        if !st.evidence.pin(trigger_id, now_ms) {
            return Err(AlertError::Internal);
        }
        if let Some(alert) = set["decision"]["alert_id"].as_u64()
            && !st.snapshots.has(alert)
            && let Some(first) = set["frames"][0]["index"].as_u64()
            && let Some(jpeg) = u32::try_from(first)
                .ok()
                .and_then(|i| st.evidence.frame(trigger_id, i, now_ms))
        {
            st.snapshots.put(alert, &jpeg, now_ms);
        }
        Ok(())
    }
}
