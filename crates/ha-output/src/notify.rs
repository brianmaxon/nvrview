//! Notify targets. Home Assistant 2024.5 and later expose notifiers as
//! entities (`notify.<name>`, sent with the `notify.send_message` action);
//! older setups and some integrations still register a legacy service
//! (`notify.<name>`, posted to `/api/services/notify/<name>`). The REST API
//! cannot tell the two apart by name, so the route is found by asking for
//! the entity's state: 200 is an entity, 404 is not.

use crate::output::{Attempt, ErrorKind, Failure, HaOutput, Step, describe};
use serde_json::{Value, json};
use std::collections::BTreeMap;

/// How a notify target is reached.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Route {
    /// `POST /api/services/notify/send_message` with `entity_id`.
    Entity,
    /// `POST /api/services/notify/<name>`.
    Legacy,
}

impl Route {
    fn summary(self) -> &'static str {
        match self {
            Route::Entity => "sent to the notify entity (200)",
            Route::Legacy => "sent with the legacy notify service (200)",
        }
    }
}

/// The kind of a listed target.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum TargetKind {
    Entity,
    Service,
}

/// One notify target Home Assistant offers: the name without the `notify.`
/// prefix, which is what a rule's `service` holds.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NotifyTarget {
    pub name: String,
    pub kind: TargetKind,
}

/// The name as stored: a pasted `notify.example_phone` becomes
/// `example_phone`.
pub(crate) fn bare_name(name: &str) -> &str {
    let name = name.trim();
    name.strip_prefix("notify.").unwrap_or(name)
}

/// Legacy services that are not targets of their own: the entity action
/// itself and the persistent-notification helper.
const NOT_TARGETS: [&str; 2] = ["send_message", "persistent_notification"];

/// Targets from the replies of `GET /api/states` (notify entities) and
/// `GET /api/services` (legacy services), entities first then services,
/// each sorted, one per name. Anything else in the replies is ignored.
pub(crate) fn parse_targets(states: &Value, services: &Value) -> Vec<NotifyTarget> {
    let mut by_name: BTreeMap<String, TargetKind> = BTreeMap::new();
    for s in states.as_array().into_iter().flatten() {
        let id = s.get("entity_id").and_then(Value::as_str).unwrap_or("");
        if let Some(name) = id.strip_prefix("notify.").filter(|n| usable(n)) {
            by_name.insert(name.to_owned(), TargetKind::Entity);
        }
    }
    for d in services.as_array().into_iter().flatten() {
        if d.get("domain").and_then(Value::as_str) != Some("notify") {
            continue;
        }
        let names = d.get("services").and_then(Value::as_object);
        for name in names.into_iter().flat_map(|m| m.keys()) {
            if usable(name) && !NOT_TARGETS.contains(&name.as_str()) {
                by_name.entry(name.clone()).or_insert(TargetKind::Service);
            }
        }
    }
    let mut out: Vec<NotifyTarget> = by_name
        .into_iter()
        .map(|(name, kind)| NotifyTarget { name, kind })
        .collect();
    out.sort_by(|a, b| a.kind.cmp(&b.kind).then_with(|| a.name.cmp(&b.name)));
    out
}

fn usable(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
}

impl HaOutput {
    /// The notify targets Home Assistant offers, fetched now with this
    /// output's token. The error text says what failed, in the same words as
    /// a delivery failure.
    pub fn list_notify_targets(&self) -> Result<Vec<NotifyTarget>, String> {
        let mut rec: Vec<Attempt> = Vec::new();
        let fail = |f: Failure| {
            format!(
                "Home Assistant: could not list targets: {}",
                describe(&f, Step::ReadState)
            )
        };
        let states = self.get_list("/api/states", &mut rec).map_err(fail)?;
        let services = self.get_list("/api/services", &mut rec).map_err(fail)?;
        Ok(parse_targets(&states, &services))
    }

    /// Sends one notification to `name`, resolving the route first when it
    /// is not known. Returns the success text for the summary.
    pub(crate) fn notify(
        &self,
        name: &str,
        title: Option<&str>,
        message: &str,
        rec: &mut Vec<Attempt>,
    ) -> Result<String, Failure> {
        let (route, fresh) = match self.route_of(name) {
            Some(r) => (r, false),
            None => (self.resolve(name, rec)?, true),
        };
        match self.send_notify(route, name, title, message, rec) {
            Ok(()) => Ok(route.summary().to_owned()),
            Err(f) if !fresh && matches!(f.status, Some(400 | 404)) => {
                // The cached route may be stale (the integration was
                // replaced, the entity renamed): ask again, once.
                self.forget_route(name);
                let again = self.resolve(name, rec)?;
                if again == route {
                    return Err(f);
                }
                self.send_notify(again, name, title, message, rec)?;
                Ok(again.summary().to_owned())
            }
            Err(f) => Err(f),
        }
    }

    /// Asks whether `notify.<name>` is an entity and remembers the answer.
    fn resolve(&self, name: &str, rec: &mut Vec<Attempt>) -> Result<Route, Failure> {
        let path = format!("/api/states/notify.{name}");
        let route = match self.get(Step::ReadState, &path, rec) {
            Ok(_) => Route::Entity,
            Err(f) if f.kind == ErrorKind::NotFound => Route::Legacy,
            Err(f) => return Err(f),
        };
        self.remember_route(name, route);
        Ok(route)
    }

    fn send_notify(
        &self,
        route: Route,
        name: &str,
        title: Option<&str>,
        message: &str,
        rec: &mut Vec<Attempt>,
    ) -> Result<(), Failure> {
        let mut body = json!({ "message": message });
        if let Some(t) = title {
            body["title"] = json!(t);
        }
        let path = match route {
            Route::Entity => {
                body["entity_id"] = json!(format!("notify.{name}"));
                "/api/services/notify/send_message".to_owned()
            }
            Route::Legacy => format!("/api/services/notify/{name}"),
        };
        self.post_list(Step::Service, &path, &body, rec).map(|_| ())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_pasted_full_name_loses_its_prefix() {
        assert_eq!(bare_name("notify.example_phone"), "example_phone");
        assert_eq!(bare_name(" example_phone "), "example_phone");
    }

    #[test]
    fn targets_merge_entities_and_services() {
        let states = json!([
            {"entity_id": "notify.example_phone", "state": "unknown"},
            {"entity_id": "light.example", "state": "on"},
            {"entity_id": "notify.Bad Name", "state": "unknown"}
        ]);
        let services = json!([
            {"domain": "light", "services": {"turn_on": {}}},
            {"domain": "notify", "services": {
                "send_message": {}, "persistent_notification": {},
                "example_phone": {}, "example_old": {}
            }}
        ]);
        let t = parse_targets(&states, &services);
        let names: Vec<_> = t.iter().map(|t| (t.name.as_str(), t.kind)).collect();
        assert_eq!(
            names,
            vec![
                ("example_phone", TargetKind::Entity),
                ("example_old", TargetKind::Service)
            ]
        );
    }
}
