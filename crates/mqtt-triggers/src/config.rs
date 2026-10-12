//! Settings for one MQTT trigger source. Everything here is plain data the
//! host can store; the password is deliberately not part of it (see
//! [`crate::Credentials`]).

use serde::{Deserialize, Serialize};

/// How a retained message (one the broker kept and replays on subscribe,
/// which says nothing about now) is treated.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RetainedPolicy {
    /// Drop it. A stale "on" must not fire at connect. The default.
    #[default]
    Ignore,
    /// Remember the state it reports without emitting, so the next change is
    /// judged against it (an "off" after a retained "on" is a stop).
    Seed,
    /// Treat it like any message.
    Accept,
}

/// What a source's messages mean.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    /// A binary sensor: off to on starts, on to off stops.
    #[default]
    Level,
    /// An event topic: every "on" is a single pulse, "off" is ignored.
    Pulse,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Mapping {
    /// Payloads (case-insensitive, trimmed) that mean "on".
    pub on_values: Vec<String>,
    /// Payloads that mean "off".
    pub off_values: Vec<String>,
    /// For JSON payloads: dot-separated keys and array indexes locating the
    /// value (`state`, `sensor.state`, `items.0.state`). Unset means the
    /// whole payload is the value.
    pub json_path: Option<String>,
    pub retained: RetainedPolicy,
    pub mode: Mode,
    /// Level mode: whether off to on counts as a start.
    pub start_on_on: bool,
    /// Level mode: whether on to off counts as a stop.
    pub stop_on_off: bool,
    /// A state change this soon after the previous trigger is dropped
    /// (a flapping sensor), as is any pulse. 0 disables.
    pub debounce_ms: u64,
}

impl Default for Mapping {
    fn default() -> Mapping {
        Mapping {
            // Home Assistant binary sensors publish ON and OFF; the others
            // are the plain forms other bridges use.
            on_values: vec!["ON".into(), "true".into(), "1".into()],
            off_values: vec!["OFF".into(), "false".into(), "0".into()],
            json_path: None,
            retained: RetainedPolicy::Ignore,
            mode: Mode::Level,
            start_on_on: true,
            stop_on_off: true,
            debounce_ms: 500,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TlsConfig {
    /// PEM text of the certificate authority that signed the broker's
    /// certificate. When unset, the built-in public web roots are used.
    pub ca_pem: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Broker {
    pub host: String,
    pub port: u16,
    /// `None` is plain TCP.
    pub tls: Option<TlsConfig>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Limits {
    /// Larger payloads are dropped unread.
    pub max_payload_bytes: usize,
    /// Messages accepted per minute, with a burst allowance; the rest are
    /// dropped and counted.
    pub rate_per_min: u32,
    pub rate_burst: u32,
}

impl Default for Limits {
    fn default() -> Limits {
        Limits {
            max_payload_bytes: 1024,
            rate_per_min: 10,
            rate_burst: 20,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Reconnect {
    pub base_ms: u64,
    pub max_ms: u64,
}

impl Default for Reconnect {
    fn default() -> Reconnect {
        Reconnect {
            base_ms: 1_000,
            max_ms: 60_000,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceConfig {
    /// The source key the host maps to cameras; carried on every trigger.
    pub id: String,
    /// The trigger kind the host files these under (for example `motion`
    /// or `contact`). Configured, never taken from a payload.
    pub kind: String,
    pub broker: Broker,
    /// The topic to subscribe to. Exact unless `allow_wildcards`.
    pub topic: String,
    #[serde(default)]
    pub allow_wildcards: bool,
    #[serde(default)]
    pub mapping: Mapping,
    #[serde(default = "default_client_id")]
    pub client_id: String,
    #[serde(default = "default_keepalive")]
    pub keepalive_secs: u16,
    #[serde(default = "default_true")]
    pub clean_session: bool,
    /// Allow a broker that is not on the local network. Off by default.
    #[serde(default)]
    pub allow_public: bool,
    #[serde(default)]
    pub limits: Limits,
    #[serde(default)]
    pub reconnect: Reconnect,
    #[serde(default = "default_connect_timeout")]
    pub connect_timeout_ms: u64,
}

fn default_client_id() -> String {
    "nvrview".into()
}
fn default_keepalive() -> u16 {
    30
}
fn default_true() -> bool {
    true
}
fn default_connect_timeout() -> u64 {
    5_000
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ConfigError {
    Id,
    Kind,
    Host,
    Topic,
    Wildcard,
    ClientId,
    Keepalive,
    Payload,
    Values,
    JsonPath,
    Backoff,
    /// A literal IP address that is not on the local network.
    PublicAddress,
}

impl std::fmt::Display for ConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            ConfigError::Id => "source id is empty or too long",
            ConfigError::Kind => "trigger kind is empty or too long",
            ConfigError::Host => "broker host is empty or invalid",
            ConfigError::Topic => "topic is empty, too long, or not a valid filter",
            ConfigError::Wildcard => "topic has wildcards but wildcards are not allowed",
            ConfigError::ClientId => "client id is empty or too long",
            ConfigError::Keepalive => "keepalive must be at least 5 seconds",
            ConfigError::Payload => "payload limit is out of range",
            ConfigError::Values => "on and off values must be non-empty and distinct",
            ConfigError::JsonPath => "JSON path is empty or too long",
            ConfigError::Backoff => "reconnect delays are invalid",
            ConfigError::PublicAddress => {
                "broker address is not on the local network (allow_public is off)"
            }
        })
    }
}

impl std::error::Error for ConfigError {}

fn valid_filter(t: &str, allow_wildcards: bool) -> Result<(), ConfigError> {
    if t.is_empty() || t.len() > 256 || t.contains('\0') {
        return Err(ConfigError::Topic);
    }
    let levels: Vec<&str> = t.split('/').collect();
    let mut any_wild = false;
    for (i, l) in levels.iter().enumerate() {
        if l.contains('#') {
            if *l != "#" || i + 1 != levels.len() {
                return Err(ConfigError::Topic);
            }
            any_wild = true;
        }
        if l.contains('+') {
            if *l != "+" {
                return Err(ConfigError::Topic);
            }
            any_wild = true;
        }
    }
    if any_wild && !allow_wildcards {
        return Err(ConfigError::Wildcard);
    }
    Ok(())
}

impl SourceConfig {
    /// A config with the defaults for everything but the identity, broker,
    /// and topic.
    pub fn new(id: &str, kind: &str, host: &str, port: u16, topic: &str) -> SourceConfig {
        SourceConfig {
            id: id.into(),
            kind: kind.into(),
            broker: Broker {
                host: host.into(),
                port,
                tls: None,
            },
            topic: topic.into(),
            allow_wildcards: false,
            mapping: Mapping::default(),
            client_id: default_client_id(),
            keepalive_secs: default_keepalive(),
            clean_session: true,
            allow_public: false,
            limits: Limits::default(),
            reconnect: Reconnect::default(),
            connect_timeout_ms: default_connect_timeout(),
        }
    }

    pub fn validate(&self) -> Result<(), ConfigError> {
        if self.id.is_empty() || self.id.len() > 64 {
            return Err(ConfigError::Id);
        }
        if self.kind.is_empty() || self.kind.len() > 64 {
            return Err(ConfigError::Kind);
        }
        let h = &self.broker.host;
        if h.is_empty() || h.len() > 253 || h.chars().any(|c| c.is_whitespace() || c == '/') {
            return Err(ConfigError::Host);
        }
        valid_filter(&self.topic, self.allow_wildcards)?;
        if self.client_id.is_empty() || self.client_id.len() > 64 {
            return Err(ConfigError::ClientId);
        }
        if self.keepalive_secs < 5 {
            return Err(ConfigError::Keepalive);
        }
        if self.limits.max_payload_bytes == 0 || self.limits.max_payload_bytes > 32 * 1024 {
            return Err(ConfigError::Payload);
        }
        let m = &self.mapping;
        let ci = |a: &str, b: &str| a.trim().eq_ignore_ascii_case(b.trim());
        if m.on_values.is_empty()
            || m.off_values.is_empty()
            || m.on_values
                .iter()
                .chain(&m.off_values)
                .any(|v| v.trim().is_empty())
            || m.on_values
                .iter()
                .any(|a| m.off_values.iter().any(|b| ci(a, b)))
        {
            return Err(ConfigError::Values);
        }
        if let Some(p) = &m.json_path
            && (p.is_empty() || p.len() > 128)
        {
            return Err(ConfigError::JsonPath);
        }
        if self.reconnect.base_ms == 0 || self.reconnect.max_ms < self.reconnect.base_ms {
            return Err(ConfigError::Backoff);
        }
        if let Ok(ip) = self.broker.host.parse::<std::net::IpAddr>()
            && !self.allow_public
            && !crate::net::is_local(ip)
        {
            return Err(ConfigError::PublicAddress);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ok() -> SourceConfig {
        SourceConfig::new("sensor-1", "motion", "192.0.2.10", 1883, "example/topic")
    }

    #[test]
    fn defaults_validate_but_documentation_range_is_public() {
        // 192.0.2.0/24 is documentation space, not private space.
        assert_eq!(ok().validate(), Err(ConfigError::PublicAddress));
        let mut c = ok();
        c.broker.host = "127.0.0.1".into();
        assert_eq!(c.validate(), Ok(()));
        c.broker.host = "192.0.2.10".into();
        c.allow_public = true;
        assert_eq!(c.validate(), Ok(()));
    }

    #[test]
    fn topics() {
        let mut c = ok();
        c.broker.host = "broker.example.net".into();
        c.topic = "a/+/c".into();
        assert_eq!(c.validate(), Err(ConfigError::Wildcard));
        c.allow_wildcards = true;
        assert_eq!(c.validate(), Ok(()));
        for bad in ["", "a/#/c", "a/b#", "a+/b"] {
            c.topic = bad.into();
            assert_eq!(c.validate(), Err(ConfigError::Topic), "{bad}");
        }
    }

    #[test]
    fn values_must_differ() {
        let mut c = ok();
        c.broker.host = "broker.example.net".into();
        c.mapping.on_values = vec!["x".into()];
        c.mapping.off_values = vec!["X".into()];
        assert_eq!(c.validate(), Err(ConfigError::Values));
    }

    #[test]
    fn json_round_trip_with_defaults() {
        let c: SourceConfig = serde_json::from_str(
            r#"{"id":"s","kind":"motion","broker":{"host":"broker.example.net","port":1883,"tls":null},"topic":"example/topic"}"#,
        )
        .unwrap();
        assert_eq!(c.mapping, Mapping::default());
        assert_eq!(c.validate(), Ok(()));
    }
}
