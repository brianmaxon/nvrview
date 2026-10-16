//! What the alert filters learned (docs/FFI.md, "Smart alerts", minor 17):
//! insight layers per camera for the app to draw, a per-camera reset, and
//! the static-scene memory's file in the alerts data folder.
//!
//! The memory is saved as `scene-memory.bin` (a small versioned binary
//! file, at most a few MB) at most every [`SAVE_EVERY`] while it changes,
//! and when the service stops; a missing, damaged, or older file starts an
//! empty memory. Everything here is local data: nothing is sent to a camera.

use super::api::{core_err, parse_camera, parse_object};
use super::{AlertError, State, camera_json, class_name, lock};
use alerts::filters::SceneMemory;
use alerts::insight::insights;
use alerts::rules::context;
use serde_json::{Value, json};
use std::fs;
use std::path::Path;
use std::time::Duration;
use tokio::time::Instant;
use tracing::warn;

const SCENE_FILE: &str = "scene-memory.bin";
/// The longest a changed memory waits to be saved.
pub(super) const SAVE_EVERY: Duration = Duration::from_secs(300);

/// The saved memory, if there is a usable one.
pub(super) fn load_scene(dir: Option<&Path>) -> Option<SceneMemory> {
    let bytes = fs::read(dir?.join(SCENE_FILE)).ok()?;
    let scene = SceneMemory::from_bytes(&bytes);
    if scene.is_none() {
        warn!("the saved scene memory could not be read; starting empty");
    }
    scene
}

/// Saves the memory when it changed and the last save is old enough (or
/// `force`).
pub(super) fn save_scene_if_due(st: &mut State, instant: Instant, force: bool) {
    let Some(e) = st.engine.as_mut() else {
        return;
    };
    if !e.scene_dirty()
        || (!force
            && st
                .scene_saved
                .is_some_and(|t| instant.duration_since(t) < SAVE_EVERY))
    {
        return;
    }
    st.scene_saved = Some(instant);
    let Some(dir) = st.dir.as_ref() else {
        e.scene_saved();
        return;
    };
    let tmp = dir.join(format!("{SCENE_FILE}.tmp"));
    let done = fs::create_dir_all(dir)
        .and_then(|()| fs::write(&tmp, e.scene().to_bytes()))
        .and_then(|()| fs::rename(&tmp, dir.join(SCENE_FILE)));
    match done {
        Ok(()) => e.scene_saved(),
        Err(err) => warn!("could not save the scene memory: {err}"),
    }
}

impl crate::Core {
    /// What the alert filters learned. `{}` answers `{"cameras":[{"camera",
    /// "classes":[...]}],"at_ms"}`: the cameras with a scene memory.
    /// `{"camera":{device,channel,lens}}` answers that camera's layers:
    /// `{"camera","at_ms","night","layers":[...],"picture":{"trigger_id",
    /// "frame","at_ms"}|null}`, where `picture` is the newest kept evidence
    /// frame of the camera to draw the layers on. Each layer has `id`,
    /// `title`, `description`, `provenance`, `units`, `range` and a `kind`:
    /// `grid` (`rows`, `cols`, `values` row by row from the top left,
    /// optional `class`, `threshold`, `updated_ms`), `boxes` (`boxes`:
    /// `{class, score, box}`, optional `at_ms`), or `scalar` (`items`:
    /// `{id, label, value, units}`).
    ///
    /// # Errors
    /// [`AlertError::InvalidArgument`], [`AlertError::NotConfigured`]
    /// before the first configure, [`AlertError::Closed`].
    pub fn alert_insights_get(&self, query: &str) -> Result<String, AlertError> {
        self.check_api().map_err(core_err)?;
        let q = if query.trim().is_empty() {
            json!({})
        } else {
            parse_object(query)?
        };
        let camera = match q.get("camera") {
            None | Some(Value::Null) => None,
            Some(c) => Some(parse_camera(c)?),
        };
        let inner = &self.alerts.inner;
        let now_ms = inner.clock.wall_now_us().div_euclid(1000);
        let mut st = lock(&inner.state);
        let offset = st.offset_min;
        let Some(e) = st.engine.as_ref() else {
            return Err(AlertError::NotConfigured);
        };
        let Some(cam) = camera else {
            let cameras: Vec<Value> = e
                .scene()
                .cameras()
                .into_iter()
                .map(|(c, classes)| {
                    json!({
                        "camera": camera_json(c),
                        "classes": classes.into_iter().map(class_name).collect::<Vec<_>>(),
                    })
                })
                .collect();
            return Ok(json!({"cameras": cameras, "at_ms": now_ms}).to_string());
        };
        let night = context(e.settings(), now_ms, offset).night;
        let mut v = serde_json::to_value(insights(e, cam, now_ms, night))
            .map_err(|_| AlertError::Internal)?;
        st.evidence.prune(now_ms);
        v["picture"] = st.evidence.latest_picture(&camera_json(cam)).map_or(
            Value::Null,
            |(id, frame, at)| json!({"trigger_id": id, "frame": frame, "at_ms": at}),
        );
        Ok(v.to_string())
    }

    /// Forgets what the filters learned about one camera
    /// (`{"camera":{device,channel,lens}}`): its scene memory and its last
    /// alerted boxes. A local data action; the file is saved at once.
    ///
    /// # Errors
    /// [`AlertError::InvalidArgument`], [`AlertError::NotConfigured`],
    /// [`AlertError::Closed`].
    pub fn alert_insights_reset(&self, query: &str) -> Result<(), AlertError> {
        self.check_api().map_err(core_err)?;
        let q = parse_object(query)?;
        let cam = parse_camera(q.get("camera").ok_or(AlertError::InvalidArgument)?)?;
        let inner = &self.alerts.inner;
        let mut st = lock(&inner.state);
        let Some(e) = st.engine.as_mut() else {
            return Err(AlertError::NotConfigured);
        };
        e.reset_scene(cam);
        save_scene_if_due(&mut st, Instant::now(), true);
        Ok(())
    }
}
