//! The input and output modules the core ships with, registered by kind
//! when the core starts: the MQTT trigger input (`crates/mqtt-triggers`)
//! and the Home Assistant output (`crates/ha-output`). Each is built from
//! its settings entry plus the credential the app passed in memory with
//! the settings (docs/NOTIFICATIONS.md, "MQTT input" and "Home Assistant
//! output"); a credential is never stored, logged, or put in an event.

mod home_assistant;
mod mqtt;

pub use home_assistant::home_assistant_factory;
pub use mqtt::mqtt_factory;

use super::Factories;

/// The factories every core starts with.
pub(super) fn builtin() -> Factories {
    let mut f = Factories::default();
    f.inputs.insert("mqtt".to_owned(), mqtt_factory());
    f.outputs.insert(
        "home_assistant".to_owned(),
        home_assistant_factory(None, None),
    );
    f
}
