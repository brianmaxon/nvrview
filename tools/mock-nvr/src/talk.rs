//! A doorbell's speaker: talk-back as observed from the official client
//! (docs/protocol.md, "Talk-back (motion)").
//!
//! Channels listed in [`crate::Config::talk_channels`] have `ipcAudioTalk` 1
//! in the capabilities reply (199), answer the talk ability query (10) with
//! the observed `TalkAbility`, accept a talk request (201) with the observed
//! configuration, take audio (202, one `01wb` record each, every one
//! answered with an empty status-200 202 with message ID 0), and close on
//! 11. Every talk is logged with each record's payload and arrival time, so
//! tests can decode what arrived and check its pacing.
//!
//! What a real device answers to 10 or 201 on a channel without talk-back,
//! to a second 201, or to 202 without a talk is not known; the mock answers
//! status 400 to each (its own choice).

use bc_protocol::media::{Demuxer, Record};
use bc_protocol::talk::{self, DecodedRecord, RECORD_PAYLOAD_LEN, TalkAudioError};
use tokio::time::Instant;

/// The ability reply body (inside `<body>`), as observed.
pub(crate) const ABILITY: &str = "<TalkAbility version=\"1.1\">\n<duplexList>\n<duplex>FDX</duplex>\n\
    </duplexList>\n<audioStreamModeList>\n<audioStreamMode>followVideoStream</audioStreamMode>\n\
    <audioStreamMode>mixAudioStream</audioStreamMode>\n</audioStreamModeList>\n<audioConfigList>\n\
    <audioConfig>\n<priority>0</priority>\n<audioType>adpcm</audioType>\n\
    <sampleRate>16000</sampleRate>\n<samplePrecision>16</samplePrecision>\n\
    <lengthPerEncoder>1024</lengthPerEncoder>\n<soundTrack>mono</soundTrack>\n</audioConfig>\n\
    </audioConfigList>\n</TalkAbility>\n";

/// How a talk ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TalkEnd {
    /// Still open.
    Open,
    /// Closed by command 11.
    Closed,
    /// The connection ended with the talk open (no 11).
    Disconnected,
}

/// One received audio record.
#[derive(Clone, Debug)]
pub struct TalkRecord {
    /// When the message was read.
    pub at: Instant,
    /// The record's stream index (1 for `01wb`).
    pub stream_index: u8,
    /// The record's payload (prefix, ADPCM state, samples).
    pub payload: Vec<u8>,
}

impl TalkRecord {
    /// Decodes the payload.
    pub fn decode(&self) -> Result<DecodedRecord, TalkAudioError> {
        talk::decode_record(&self.payload)
    }
}

/// One talk, from its 201 to its end.
#[derive(Clone, Debug)]
pub struct TalkLog {
    /// The channel.
    pub channel: u8,
    /// The connection it ran on (one per login).
    pub connection: u64,
    /// When the 201 was accepted.
    pub opened: Instant,
    /// When it ended, if it has.
    pub ended: Option<Instant>,
    /// How it ended.
    pub end: TalkEnd,
    /// The audio records received, in order.
    pub records: Vec<TalkRecord>,
}

impl TalkLog {
    /// Every record decoded and joined: the samples that arrived. Fails on
    /// the first record that is not a well-formed talk record (wrong
    /// length or prefix, or undecodable).
    pub fn samples(&self) -> Result<Vec<i16>, String> {
        let mut out = Vec::new();
        for (i, r) in self.records.iter().enumerate() {
            if r.payload.len() != RECORD_PAYLOAD_LEN {
                return Err(format!("record {i}: {} bytes", r.payload.len()));
            }
            let d = r.decode().map_err(|e| format!("record {i}: {e}"))?;
            if d.prefix != talk::RECORD_PREFIX {
                return Err(format!("record {i}: prefix {:02x?}", d.prefix));
            }
            out.extend(d.samples);
        }
        Ok(out)
    }

    /// Whether every record starts where the previous one ended, the first
    /// from predictor 0, step index 0.
    pub fn continuous(&self) -> bool {
        let mut prev = talk::AdpcmState::default();
        for r in &self.records {
            match r.decode() {
                Ok(d) if d.start == prev => prev = d.end,
                _ => return false,
            }
        }
        true
    }
}

/// The audio records in one 202 payload.
pub(crate) fn records(payload: &[u8], at: Instant) -> Option<Vec<TalkRecord>> {
    let mut d = Demuxer::new();
    d.push(payload);
    let mut out = Vec::new();
    loop {
        match d.next_record() {
            Ok(Some(Record::Audio { stream_index, data })) => out.push(TalkRecord {
                at,
                stream_index,
                payload: data,
            }),
            Ok(Some(_)) | Err(_) => return None,
            Ok(None) => break,
        }
    }
    (d.buffered() == 0 && !out.is_empty()).then_some(out)
}
