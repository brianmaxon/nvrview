//! Detection events, as docs/protocol.md "Detection events" describes them:
//! a client subscribes with command 192 (answered with an empty status 200),
//! and the mock then pushes command 33 with that request's message ID: a
//! full snapshot of every channel slot at once, and again after every
//! change.
//!
//! Tests change the state with [`crate::MockNvr::set_alarm`]; the command
//! line tool can cycle a scripted detection through the channels
//! ([`crate::Config::detection_cycle`]). Each channel's capabilities
//! (`aitype` in the 199 reply) come from [`crate::Config::aitypes`].

use crate::{Writer, lock};
use bc_protocol::crypto::SessionKey;
use bc_protocol::framer::encode_message_with_status;
use bc_protocol::header::Class;
use bc_protocol::xml::XML_DECLARATION;
use std::io::{self, Write};
use std::sync::Mutex;

/// One channel slot's alarm state.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SlotState {
    /// `status`, such as `MD`; empty for `none`.
    pub status: String,
    /// `AItype`, such as `people`; empty for `none`.
    pub ai_type: String,
}

struct Subscriber {
    conn: u64,
    writer: Writer,
    key: SessionKey,
    message_id: u32,
}

/// The alarm state of every slot and the sessions subscribed to it.
pub(crate) struct Board {
    slots: Mutex<Vec<SlotState>>,
    subscribers: Mutex<Vec<Subscriber>>,
}

fn none_if_empty(s: &str) -> &str {
    if s.is_empty() { "none" } else { s }
}

impl Board {
    pub(crate) fn new(channels: u8) -> Board {
        Board {
            slots: Mutex::new(vec![SlotState::default(); usize::from(channels)]),
            subscribers: Mutex::new(Vec::new()),
        }
    }

    fn snapshot_xml(&self) -> String {
        let slots = lock(&self.slots);
        let mut s = String::from("<AlarmEventList version=\"1.1\">\n");
        for (ch, st) in slots.iter().enumerate() {
            s.push_str(&format!(
                "<AlarmEvent version=\"1.1\">\n<channelId>{ch}</channelId>\n<status>{}</status>\n\
                 <AItype>{}</AItype>\n<recording>0</recording>\n<timeStamp>0</timeStamp>\n</AlarmEvent>\n",
                none_if_empty(&st.status),
                none_if_empty(&st.ai_type),
            ));
        }
        s.push_str("</AlarmEventList>\n");
        s
    }

    fn push_to(sub: &Subscriber, inner: &str) -> io::Result<()> {
        let mut body = format!("{XML_DECLARATION}\n<body>\n{inner}</body>\n").into_bytes();
        sub.key.encrypt(&mut body);
        let bytes = encode_message_with_status(33, sub.message_id, 200, Class::Device, &[], &body)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;
        lock(&sub.writer).write_all(&bytes)
    }

    /// A session subscribed (192): it gets the current snapshot at once.
    pub(crate) fn subscribe(&self, conn: u64, writer: Writer, key: SessionKey, message_id: u32) {
        let sub = Subscriber {
            conn,
            writer,
            key,
            message_id,
        };
        let _ = Self::push_to(&sub, &self.snapshot_xml());
        let mut subs = lock(&self.subscribers);
        subs.retain(|s| s.conn != conn);
        subs.push(sub);
    }

    /// The session ended.
    pub(crate) fn unsubscribe(&self, conn: u64) {
        lock(&self.subscribers).retain(|s| s.conn != conn);
    }

    /// Sets one slot; pushes a snapshot to every subscriber if it changed.
    /// Returns false for a channel the mock does not have.
    pub(crate) fn set(&self, channel: u8, state: SlotState) -> bool {
        {
            let mut slots = lock(&self.slots);
            let Some(slot) = slots.get_mut(usize::from(channel)) else {
                return false;
            };
            if *slot == state {
                return true;
            }
            *slot = state;
        }
        let xml = self.snapshot_xml();
        // A subscriber whose connection failed is dropped; its session ends
        // on its own.
        lock(&self.subscribers).retain(|s| Self::push_to(s, &xml).is_ok());
        true
    }

    /// Sessions currently subscribed.
    pub(crate) fn subscribers(&self) -> usize {
        lock(&self.subscribers).len()
    }
}
