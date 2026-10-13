//! The per-source message log: the last messages a source received, with
//! what became of each, for a person debugging a sensor. Memory only: a
//! payload may be private, so nothing here is written to disk or to a log
//! line, and the log is gone when the source stops.

use serde::Serialize;
use std::collections::VecDeque;

/// Entries kept per source.
pub const MAX_ENTRIES: usize = 500;
/// Payload bytes kept per entry.
pub const MAX_PAYLOAD_PREVIEW: usize = 512;
/// Topic bytes kept per entry.
const MAX_TOPIC: usize = 256;

/// What became of a message. The `code` is stable (the app filters on it);
/// the `text` is the sentence shown.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct MessageOutcome {
    pub code: &'static str,
    pub text: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct MessageEntry {
    /// Increases by one per message for the life of the source.
    pub seq: u64,
    pub at_ms: u64,
    pub topic: String,
    /// Text when the payload is printable UTF-8, else `hex:` and the bytes.
    pub payload: String,
    /// The payload's full length in bytes.
    pub payload_len: usize,
    /// The preview stops at 512 bytes.
    pub truncated: bool,
    pub binary: bool,
    pub retained: bool,
    pub outcome: MessageOutcome,
}

/// One page, oldest first.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct MessagePage {
    pub entries: Vec<MessageEntry>,
    /// Pass as `after` to get what came since.
    pub next_seq: u64,
    /// Entries held now.
    pub total: usize,
    /// More entries after this page.
    pub more: bool,
}

#[derive(Default)]
pub(crate) struct MessageLog {
    entries: VecDeque<MessageEntry>,
    next_seq: u64,
}

fn cut(s: &str, max: usize) -> &str {
    if s.len() <= max {
        return s;
    }
    let mut end = max;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

/// A binary-safe preview: printable text as is, anything else as hex.
/// Returns the text, whether it was cut, and whether it is hex.
pub fn preview(payload: &[u8]) -> (String, bool, bool) {
    let truncated = payload.len() > MAX_PAYLOAD_PREVIEW;
    let part = &payload[..payload.len().min(MAX_PAYLOAD_PREVIEW)];
    let text = match std::str::from_utf8(part) {
        Ok(t) => Some(t),
        // A multi-byte character cut by the preview limit is still text.
        Err(e) if truncated && e.error_len().is_none() => {
            std::str::from_utf8(&part[..e.valid_up_to()]).ok()
        }
        Err(_) => None,
    };
    match text {
        Some(t)
            if !t
                .chars()
                .any(|c| c.is_control() && !matches!(c, '\n' | '\r' | '\t')) =>
        {
            (t.to_owned(), truncated, false)
        }
        _ => {
            let mut s = String::from("hex:");
            for b in part {
                s.push_str(&format!(" {b:02x}"));
            }
            (s, truncated, true)
        }
    }
}

impl MessageLog {
    pub(crate) fn push(
        &mut self,
        at_ms: u64,
        topic: &str,
        payload: &[u8],
        retained: bool,
        outcome: MessageOutcome,
    ) {
        let (payload_text, truncated, binary) = preview(payload);
        if self.entries.len() == MAX_ENTRIES {
            self.entries.pop_front();
        }
        self.entries.push_back(MessageEntry {
            seq: self.next_seq,
            at_ms,
            topic: cut(topic, MAX_TOPIC).to_owned(),
            payload: payload_text,
            payload_len: payload.len(),
            truncated,
            binary,
            retained,
            outcome,
        });
        self.next_seq += 1;
    }

    /// Entries with `seq >= after`, at most `limit`, oldest first.
    pub(crate) fn page(&self, after: u64, limit: usize) -> MessagePage {
        let matching = self.entries.iter().filter(|e| e.seq >= after);
        let entries: Vec<MessageEntry> = matching.clone().take(limit).cloned().collect();
        let more = matching.count() > entries.len();
        let first = self.entries.front().map_or(self.next_seq, |e| e.seq);
        let next_seq = entries.last().map_or(after.max(first), |e| e.seq + 1);
        MessagePage {
            entries,
            next_seq,
            total: self.entries.len(),
            more,
        }
    }

    pub(crate) fn clear(&mut self) {
        self.entries.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ok() -> MessageOutcome {
        MessageOutcome {
            code: "matched",
            text: "matched".into(),
        }
    }

    #[test]
    fn ring_keeps_the_last_500_and_pages_by_sequence() {
        let mut log = MessageLog::default();
        for i in 0..520u64 {
            log.push(i, "t", b"ON", false, ok());
        }
        let p = log.page(0, 1000);
        assert_eq!((p.total, p.entries.len()), (500, 500));
        assert_eq!(p.entries[0].seq, 20);
        assert_eq!(p.next_seq, 520);
        assert!(!p.more);
        let p = log.page(100, 10);
        assert_eq!(
            (p.entries.len(), p.entries[0].seq, p.next_seq),
            (10, 100, 110)
        );
        assert!(p.more);
        let p = log.page(520, 10);
        assert!(p.entries.is_empty());
        assert_eq!(p.next_seq, 520);
        log.clear();
        assert_eq!(log.page(0, 10).total, 0);
        log.push(0, "t", b"x", false, ok());
        assert_eq!(log.page(0, 10).entries[0].seq, 520);
    }

    #[test]
    fn previews_are_shortened_and_binary_safe() {
        let (t, cut, bin) = preview(b"{\"state\":\"ON\"}");
        assert_eq!((t.as_str(), cut, bin), ("{\"state\":\"ON\"}", false, false));
        let (t, cut, bin) = preview(&[0xff, 0x00, 0x1b]);
        assert_eq!((t.as_str(), cut, bin), ("hex: ff 00 1b", false, true));
        let big = vec![b'a'; 2000];
        let (t, cut, _) = preview(&big);
        assert_eq!((t.len(), cut), (512, true));
        // A multi-byte character split by the limit stays text.
        let mut s = "a".repeat(511);
        s.push('é');
        let (t, cut, bin) = preview(s.as_bytes());
        assert_eq!((t.len(), cut, bin), (511, true, false));
    }
}
