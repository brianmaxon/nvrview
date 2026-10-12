//! Settings for the output: where Home Assistant is, and which alerts
//! drive which entities.

use crate::http::host_is_local;
use crate::output::DecisionKind;
use crate::template;
use alerts::{CameraId, Class, Severity};
use serde::{Deserialize, Serialize};
use std::fmt;

/// What a matching rule does.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Action {
    /// For helper entities that drive routines: read the state, turn the
    /// entity off first if it is already on (so the routine sees a real
    /// off-to-on change), turn it on, wait the pulse gap, turn it off.
    Pulse { entity_id: String },
    /// Turn the entity on and leave it.
    TurnOn { entity_id: String },
    /// `script.turn_on` with the alert facts as script variables.
    Script { entity_id: String },
    /// `notify.<service>` with a message built from the template.
    Notify {
        service: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        title: Option<String>,
    },
}

impl Action {
    /// The name shown in results: the entity id, or `notify.<service>`.
    pub fn target(&self) -> String {
        match self {
            Action::Pulse { entity_id }
            | Action::TurnOn { entity_id }
            | Action::Script { entity_id } => entity_id.clone(),
            Action::Notify { service, .. } => format!("notify.{service}"),
        }
    }
}

/// One mapping rule. Every non-empty list must contain the alert's value;
/// empty lists match anything.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rule {
    #[serde(default)]
    pub classes: Vec<Class>,
    /// Trigger source ids (the `sources[]` ids of the alert settings).
    #[serde(default)]
    pub sources: Vec<String>,
    #[serde(default)]
    pub cameras: Vec<CameraId>,
    /// The alert's severity must be at least this.
    #[serde(default)]
    pub min_severity: Option<Severity>,
    /// Decisions that count. Default: alerts only.
    #[serde(default = "default_decisions")]
    pub decisions: Vec<DecisionKind>,
    pub action: Action,
    /// Overrides [`Config::cooldown_ms`] for this rule's entity.
    #[serde(default)]
    pub cooldown_ms: Option<u64>,
}

fn default_decisions() -> Vec<DecisionKind> {
    vec![DecisionKind::Alert]
}

/// Which matching rules fire.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MatchMode {
    /// Only the first matching rule.
    #[default]
    FirstMatch,
    /// Every matching rule (one call per distinct target).
    AllMatches,
}

/// The output's settings (`outputs[]` entry of kind `home_assistant`).
/// The token is not here; the host passes it separately from the keychain.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Config {
    /// `http://host:8123` (local-network hosts only) or `https://...`.
    pub base_url: String,
    #[serde(default)]
    pub mode: MatchMode,
    #[serde(default)]
    pub rules: Vec<Rule>,
    /// Gap between turning a pulsed entity on and off again.
    #[serde(default = "d_gap")]
    pub pulse_gap_ms: u64,
    /// Per-entity quiet period after a delivery, so one event does not
    /// speak twice.
    #[serde(default = "d_cooldown")]
    pub cooldown_ms: u64,
    /// Message template for `notify` and the script `message` variable.
    /// Placeholders: `{camera}`, `{class}`, `{time}`, `{severity}`,
    /// `{explanation}`.
    #[serde(default = "d_template")]
    pub message_template: String,
    /// Tries per request (the first plus retries), 1 to 5.
    #[serde(default = "d_attempts")]
    pub max_attempts: u32,
    /// First retry delay; doubles each retry, capped at 8 s.
    #[serde(default = "d_backoff")]
    pub backoff_ms: u64,
    /// Connect and response timeout of each request.
    #[serde(default = "d_timeout")]
    pub timeout_ms: u64,
}

fn d_gap() -> u64 {
    2_000
}
fn d_cooldown() -> u64 {
    30_000
}
fn d_template() -> String {
    "{class} on {camera} at {time}: {explanation}".to_string()
}
fn d_attempts() -> u32 {
    3
}
fn d_backoff() -> u64 {
    500
}
fn d_timeout() -> u64 {
    5_000
}

impl Config {
    /// A config with the defaults and no rules.
    pub fn new(base_url: impl Into<String>) -> Config {
        Config {
            base_url: base_url.into(),
            mode: MatchMode::default(),
            rules: Vec::new(),
            pulse_gap_ms: d_gap(),
            cooldown_ms: d_cooldown(),
            message_template: d_template(),
            max_attempts: d_attempts(),
            backoff_ms: d_backoff(),
            timeout_ms: d_timeout(),
        }
    }

    /// Checks everything that would otherwise reach a request path or body:
    /// the URL, entity ids, service names, the template. Numbers are
    /// clamped by the output, not rejected.
    pub fn validate(&self) -> Result<(), ConfigError> {
        parse_base(&self.base_url)?;
        template::validate(&self.message_template).map_err(ConfigError::Template)?;
        for (i, rule) in self.rules.iter().enumerate() {
            let bad = |why: &str| ConfigError::Rule(i, why.to_string());
            match &rule.action {
                Action::Pulse { entity_id } | Action::TurnOn { entity_id } => {
                    let Some((domain, _)) = split_entity(entity_id) else {
                        return Err(bad("entity id must look like domain.object_id"));
                    };
                    if !matches!(domain, "input_boolean" | "switch") {
                        return Err(bad(
                            "pulse and turn_on entities are input_boolean or switch",
                        ));
                    }
                }
                Action::Script { entity_id } => match split_entity(entity_id) {
                    Some(("script", _)) => {}
                    _ => return Err(bad("script entity must look like script.name")),
                },
                Action::Notify { service, title } => {
                    if !is_ident(service) {
                        return Err(bad("notify service must be lowercase letters, digits, _"));
                    }
                    if let Some(t) = title {
                        template::validate(t).map_err(ConfigError::Template)?;
                    }
                }
            }
            if rule.decisions.is_empty() {
                return Err(bad("decisions must not be empty"));
            }
        }
        Ok(())
    }
}

/// Why a [`Config`] was refused.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ConfigError {
    /// The base URL is not usable; the text says why (never the URL).
    Url(&'static str),
    Template(&'static str),
    /// Rule index and reason.
    Rule(usize, String),
    /// HTTPS needs processor features this machine lacks.
    HttpsUnsupported,
}

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ConfigError::Url(w) => write!(f, "Home Assistant address: {w}"),
            ConfigError::Template(w) => write!(f, "message template: {w}"),
            ConfigError::Rule(i, w) => write!(f, "mapping rule {}: {w}", i + 1),
            ConfigError::HttpsUnsupported => {
                f.write_str("https needs a processor with AES-NI, AVX2, BMI2, and ADX")
            }
        }
    }
}

impl std::error::Error for ConfigError {}

pub(crate) struct Base {
    /// Scheme, authority, and optional path prefix; no trailing slash.
    pub root: String,
}

/// Parses and checks the base URL: http or https, a host, no credentials,
/// no query or fragment, and `http` only for local-network hosts.
pub(crate) fn parse_base(text: &str) -> Result<Base, ConfigError> {
    let err = ConfigError::Url;
    let (scheme, rest) = text.split_once("://").ok_or(err("not a URL"))?;
    let secure = match scheme.to_ascii_lowercase().as_str() {
        "https" => true,
        "http" => false,
        _ => return Err(err("only http:// and https:// addresses")),
    };
    if text.len() > 300 || text.chars().any(|c| c.is_control() || c.is_whitespace()) {
        return Err(err("contains spaces, control characters, or is too long"));
    }
    if rest.contains(['?', '#']) {
        return Err(err("no query or fragment"));
    }
    let split = rest.find('/').unwrap_or(rest.len());
    let (authority, path) = rest.split_at(split);
    if authority.contains('@') {
        return Err(err("no user name or password in the address"));
    }
    let host = if let Some(inner) = authority.strip_prefix('[') {
        inner.split(']').next().unwrap_or("")
    } else {
        authority.rsplit_once(':').map_or(authority, |(h, _)| h)
    };
    if host.is_empty() {
        return Err(err("no host"));
    }
    if !secure && !host_is_local(host) {
        return Err(err(
            "http:// is only for hosts on the local network; use https://",
        ));
    }
    if secure && !crate::http::https_supported() {
        return Err(ConfigError::HttpsUnsupported);
    }
    Ok(Base {
        root: format!("{scheme}://{authority}{}", path.trim_end_matches('/')),
    })
}

fn is_ident(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 64
        && s.bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
}

pub(crate) fn split_entity(id: &str) -> Option<(&str, &str)> {
    let (d, o) = id.split_once('.')?;
    (is_ident(d) && is_ident(o)).then_some((d, o))
}
