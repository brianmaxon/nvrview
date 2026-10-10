//! Talk-back: sound sent to a camera's or doorbell's speaker
//! (docs/protocol.md, "Talk-back (motion)").
//!
//! The sequence, as observed from the official client: read the channel's
//! talk ability (command 10, viewing tier), open talk with a `TalkConfig`
//! (201), send the audio as 202 messages each carrying one `01wb` record,
//! and close with 11. 201, 202 and 11 are camera motion control: the
//! session builds them only for a channel the user enabled
//! ([`crate::session::Session::set_motion_allowed`]).
//!
//! This module holds the pieces that do not depend on session state: which
//! channels can talk (the `ipcAudioTalk` field of 199), the ability reply,
//! the request texts, and the audio coding.
//!
//! **Audio.** IMA ADPCM, 16 kHz mono, 1024 samples per record. A record's
//! payload is a 4-byte prefix (`00 01 02 00`, the only value observed), the
//! encoder state before its first sample (i16 little-endian predictor, u8
//! step index, a zero byte), then 512 bytes holding two samples each, **high
//! nibble first**. The encoder runs on across records, so each record's
//! state is where the previous one ended; the first starts from predictor 0,
//! step index 0. All observed (client capture).
//!
//! The coding is the public IMA ADPCM algorithm (the IMA Digital Audio Focus
//! and Technical Working Group's recommendation), written from its
//! description.

use crate::xml::{XML_DECLARATION, XmlError};
use std::fmt;

/// Sample rate of talk-back audio.
pub const SAMPLE_RATE: u32 = 16_000;
/// Samples in one record (the ability's `lengthPerEncoder`).
pub const SAMPLES_PER_RECORD: usize = 1024;
/// Duration of one record: 1024 samples at 16 kHz.
pub const RECORD_DURATION_US: u64 = 64_000;
/// Bytes of 4-bit samples in one record.
pub const ADPCM_DATA_LEN: usize = SAMPLES_PER_RECORD / 2;
/// Bytes in front of the samples: the prefix and the encoder state.
pub const ADPCM_HEADER_LEN: usize = 8;
/// Length of one record's payload (both size fields of the `01wb` record).
pub const RECORD_PAYLOAD_LEN: usize = ADPCM_HEADER_LEN + ADPCM_DATA_LEN;
/// Stream index of talk-back records (the `01` of `01wb`).
pub const STREAM_INDEX: u8 = 1;
/// The 4-byte prefix of every observed record.
pub const RECORD_PREFIX: [u8; 4] = [0x00, 0x01, 0x02, 0x00];

const STEPS: [i32; 89] = [
    7, 8, 9, 10, 11, 12, 13, 14, 16, 17, 19, 21, 23, 25, 28, 31, 34, 37, 41, 45, 50, 55, 60, 66,
    73, 80, 88, 97, 107, 118, 130, 143, 157, 173, 190, 209, 230, 253, 279, 307, 337, 371, 408, 449,
    494, 544, 598, 658, 724, 796, 876, 963, 1060, 1166, 1282, 1411, 1552, 1707, 1878, 2066, 2272,
    2499, 2749, 3024, 3327, 3660, 4026, 4428, 4871, 5358, 5894, 6484, 7132, 7845, 8630, 9493,
    10442, 11487, 12635, 13899, 15289, 16818, 18500, 20350, 22385, 24623, 27086, 29794, 32767,
];

const INDEX_ADJUST: [i32; 8] = [-1, -1, -1, -1, 2, 4, 6, 8];

/// IMA ADPCM state: the predicted sample and the step index (0 to 88).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct AdpcmState {
    /// The last decoded sample.
    pub predictor: i16,
    /// Index into the step table.
    pub index: u8,
}

impl AdpcmState {
    /// Applies one 4-bit code, returning the new sample.
    fn step(&mut self, code: u8) -> i16 {
        let step = STEPS[usize::from(self.index)];
        let mut diff = step >> 3;
        if code & 1 != 0 {
            diff += step >> 2;
        }
        if code & 2 != 0 {
            diff += step >> 1;
        }
        if code & 4 != 0 {
            diff += step;
        }
        let p = i32::from(self.predictor);
        let p = if code & 8 != 0 { p - diff } else { p + diff };
        self.predictor = p.clamp(-32768, 32767) as i16;
        self.index =
            (i32::from(self.index) + INDEX_ADJUST[usize::from(code & 7)]).clamp(0, 88) as u8;
        self.predictor
    }

    /// Chooses the code for `sample` and applies it, so the encoder's state
    /// stays exactly the decoder's.
    fn encode(&mut self, sample: i16) -> u8 {
        let step = STEPS[usize::from(self.index)];
        let mut diff = i32::from(sample) - i32::from(self.predictor);
        let mut code = 0u8;
        if diff < 0 {
            code = 8;
            diff = -diff;
        }
        let mut s = step;
        for bit in [4u8, 2, 1] {
            if diff >= s {
                code |= bit;
                diff -= s;
            }
            s >>= 1;
        }
        self.step(code);
        code
    }
}

/// Encodes 16 kHz mono samples into talk-back record payloads, carrying its
/// state from one record to the next as the official client does.
#[derive(Clone, Debug, Default)]
pub struct AdpcmEncoder {
    state: AdpcmState,
}

impl AdpcmEncoder {
    /// An encoder starting from predictor 0, step index 0.
    pub fn new() -> AdpcmEncoder {
        AdpcmEncoder::default()
    }

    /// The state the next record will start from.
    pub fn state(&self) -> AdpcmState {
        self.state
    }

    /// Encodes exactly [`SAMPLES_PER_RECORD`] samples into one record
    /// payload ([`RECORD_PAYLOAD_LEN`] bytes).
    pub fn encode_record(&mut self, samples: &[i16; SAMPLES_PER_RECORD]) -> Vec<u8> {
        let mut out = Vec::with_capacity(RECORD_PAYLOAD_LEN);
        out.extend_from_slice(&RECORD_PREFIX);
        out.extend_from_slice(&self.state.predictor.to_le_bytes());
        out.push(self.state.index);
        out.push(0);
        for pair in samples.as_chunks::<2>().0 {
            let hi = self.state.encode(pair[0]);
            let lo = self.state.encode(pair[1]);
            out.push((hi << 4) | lo);
        }
        out
    }
}

/// Why a talk-back record payload cannot be decoded.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TalkAudioError {
    /// Shorter than its header.
    Truncated,
    /// The header's step index is outside 0 to 88.
    BadStepIndex(u8),
}

impl fmt::Display for TalkAudioError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TalkAudioError::Truncated => write!(f, "talk record shorter than its header"),
            TalkAudioError::BadStepIndex(i) => write!(f, "talk record step index {i} out of range"),
        }
    }
}

impl std::error::Error for TalkAudioError {}

/// One decoded talk-back record.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DecodedRecord {
    /// The 4-byte prefix as received.
    pub prefix: [u8; 4],
    /// The state in the record's header.
    pub start: AdpcmState,
    /// The state after its last sample (the next record's `start`).
    pub end: AdpcmState,
    /// The samples, two per data byte, high nibble first.
    pub samples: Vec<i16>,
}

/// Decodes one record payload (what follows the `01wb` record header).
pub fn decode_record(payload: &[u8]) -> Result<DecodedRecord, TalkAudioError> {
    if payload.len() < ADPCM_HEADER_LEN {
        return Err(TalkAudioError::Truncated);
    }
    let index = payload[6];
    if index > 88 {
        return Err(TalkAudioError::BadStepIndex(index));
    }
    let start = AdpcmState {
        predictor: i16::from_le_bytes([payload[4], payload[5]]),
        index,
    };
    let mut st = start;
    let data = &payload[ADPCM_HEADER_LEN..];
    let mut samples = Vec::with_capacity(data.len() * 2);
    for &b in data {
        samples.push(st.step(b >> 4));
        samples.push(st.step(b & 0x0F));
    }
    Ok(DecodedRecord {
        prefix: [payload[0], payload[1], payload[2], payload[3]],
        start,
        end: st,
        samples,
    })
}

/// The `duplex` of a talk request.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Duplex {
    /// `FDX` (the only value observed).
    Full,
    /// `HDX` (reported (client)).
    Half,
}

impl Duplex {
    /// The wire name.
    pub fn name(self) -> &'static str {
        match self {
            Duplex::Full => "FDX",
            Duplex::Half => "HDX",
        }
    }

    fn from_name(s: &str) -> Option<Duplex> {
        match s {
            "FDX" => Some(Duplex::Full),
            "HDX" => Some(Duplex::Half),
            _ => None,
        }
    }
}

/// The `audioStreamMode` of a talk request.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AudioStreamMode {
    /// `followVideoStream` (observed, and what the client sent).
    FollowVideoStream,
    /// `onlyAudioStream` (reported (client)).
    OnlyAudioStream,
    /// `mixAudioStream` (observed in abilities).
    MixAudioStream,
}

impl AudioStreamMode {
    /// The wire name.
    pub fn name(self) -> &'static str {
        match self {
            AudioStreamMode::FollowVideoStream => "followVideoStream",
            AudioStreamMode::OnlyAudioStream => "onlyAudioStream",
            AudioStreamMode::MixAudioStream => "mixAudioStream",
        }
    }

    fn from_name(s: &str) -> Option<AudioStreamMode> {
        match s {
            "followVideoStream" => Some(AudioStreamMode::FollowVideoStream),
            "onlyAudioStream" => Some(AudioStreamMode::OnlyAudioStream),
            "mixAudioStream" => Some(AudioStreamMode::MixAudioStream),
            _ => None,
        }
    }
}

/// One `audioConfig` of a talk ability.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AudioConfig {
    /// `audioType` (`adpcm` observed).
    pub audio_type: String,
    /// `sampleRate`.
    pub sample_rate: u32,
    /// `samplePrecision` (bits).
    pub sample_precision: u32,
    /// `lengthPerEncoder` (samples per record).
    pub length_per_encoder: u32,
    /// `soundTrack` (`mono` observed).
    pub sound_track: String,
}

impl AudioConfig {
    /// Whether this is the one configuration nvrview sends: `adpcm`, 16000,
    /// 16, 1024, `mono` (every ability observed offered exactly this).
    pub fn is_supported(&self) -> bool {
        self.audio_type == "adpcm"
            && self.sample_rate == SAMPLE_RATE
            && self.sample_precision == 16
            && self.length_per_encoder == SAMPLES_PER_RECORD as u32
            && self.sound_track == "mono"
    }
}

/// A channel's talk ability (the reply to command 10).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TalkAbility {
    /// `duplexList/duplex`, in order (unknown names left out).
    pub duplex: Vec<Duplex>,
    /// `audioStreamModeList/audioStreamMode`, in order (unknown names left
    /// out).
    pub stream_modes: Vec<AudioStreamMode>,
    /// `audioConfigList/audioConfig`, in order.
    pub configs: Vec<AudioConfig>,
}

/// What a talk request (201) asks for. The audio configuration is always
/// the supported one ([`AudioConfig::is_supported`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TalkConfig {
    /// The `duplex` to request.
    pub duplex: Duplex,
    /// The `audioStreamMode` to request.
    pub stream_mode: AudioStreamMode,
}

impl TalkAbility {
    /// The request the official client would send for this ability: its
    /// first `duplex` and first `audioStreamMode`, with the supported audio
    /// configuration. `None` when the ability offers no supported audio
    /// configuration or lists no known duplex or stream mode.
    pub fn choose(&self) -> Option<TalkConfig> {
        if !self.configs.iter().any(AudioConfig::is_supported) {
            return None;
        }
        Some(TalkConfig {
            duplex: *self.duplex.first()?,
            stream_mode: *self.stream_modes.first()?,
        })
    }
}

/// Text of every element named `tag`, in order (trimmed).
fn all_texts(xml: &str, tag: &str) -> Result<Vec<String>, XmlError> {
    use quick_xml::Reader;
    use quick_xml::events::Event;
    let mut reader = Reader::from_str(xml);
    let mut out = Vec::new();
    let mut current: Option<String> = None;
    loop {
        match reader.read_event().map_err(|_| XmlError::Malformed)? {
            Event::Start(e) if e.local_name().as_ref() == tag.as_bytes() => {
                current = Some(String::new());
            }
            Event::Text(t) => {
                if let Some(s) = current.as_mut() {
                    s.push_str(&t.xml10_content().map_err(|_| XmlError::Malformed)?);
                }
            }
            Event::End(e) if e.local_name().as_ref() == tag.as_bytes() => {
                out.extend(current.take().map(|s| s.trim().to_owned()));
            }
            Event::Eof => return Ok(out),
            _ => {}
        }
    }
}

/// Parses a `TalkAbility` reply (command 10). Unknown elements are ignored.
pub fn parse_talk_ability(xml: &str) -> Result<TalkAbility, XmlError> {
    if crate::xml::find_text(xml, "TalkAbility")?.is_none() {
        return Err(XmlError::Missing("TalkAbility"));
    }
    let configs = crate::xml::records(xml, "audioConfig")?
        .iter()
        .map(|f| {
            let num = |n: &str| {
                crate::xml::field(f, n)
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(0)
            };
            let text = |n: &str| crate::xml::field(f, n).unwrap_or("").to_owned();
            AudioConfig {
                audio_type: text("audioType"),
                sample_rate: num("sampleRate"),
                sample_precision: num("samplePrecision"),
                length_per_encoder: num("lengthPerEncoder"),
                sound_track: text("soundTrack"),
            }
        })
        .collect();
    Ok(TalkAbility {
        duplex: all_texts(xml, "duplex")?
            .iter()
            .filter_map(|s| Duplex::from_name(s))
            .collect(),
        stream_modes: all_texts(xml, "audioStreamMode")?
            .iter()
            .filter_map(|s| AudioStreamMode::from_name(s))
            .collect(),
        configs,
    })
}

/// The channels whose capabilities item (command 199) has `ipcAudioTalk`
/// 1: the channels that support talk-back (observed: the official client
/// asked exactly these for their talk ability). Only an item's own fields
/// count, not those of its lenses' `subItem`s.
pub fn parse_talk_channels(xml: &str) -> Result<Vec<u8>, XmlError> {
    use quick_xml::Reader;
    use quick_xml::events::Event;
    let mut reader = Reader::from_str(xml);
    let mut out = Vec::new();
    // Depth inside the open item (1 = its own leaves), its chnID and
    // ipcAudioTalk, and the open leaf.
    let mut depth = 0usize;
    let mut chn: Option<String> = None;
    let mut talk: Option<String> = None;
    let mut leaf: Option<(String, String)> = None;
    loop {
        match reader.read_event().map_err(|_| XmlError::Malformed)? {
            Event::Start(e) => {
                let name = e.local_name();
                if depth == 0 {
                    if name.as_ref() == b"item" {
                        depth = 1;
                        chn = None;
                        talk = None;
                    }
                } else {
                    depth += 1;
                    if depth == 2 {
                        leaf = Some((
                            String::from_utf8_lossy(name.as_ref()).into_owned(),
                            String::new(),
                        ));
                    } else {
                        leaf = None;
                    }
                }
            }
            Event::Text(t) => {
                if let Some((_, s)) = leaf.as_mut() {
                    s.push_str(&t.xml10_content().map_err(|_| XmlError::Malformed)?);
                }
            }
            Event::End(_) if depth > 0 => {
                if depth == 2
                    && let Some((name, text)) = leaf.take()
                {
                    match name.as_str() {
                        "chnID" if chn.is_none() => chn = Some(text.trim().to_owned()),
                        "ipcAudioTalk" if talk.is_none() => talk = Some(text.trim().to_owned()),
                        _ => {}
                    }
                }
                depth -= 1;
                if depth == 0
                    && talk.as_deref() == Some("1")
                    && let Some(c) = chn.as_deref().and_then(|c| c.parse::<u8>().ok())
                    && !out.contains(&c)
                {
                    out.push(c);
                }
            }
            Event::Eof => return Ok(out),
            _ => {}
        }
    }
}

/// Extension of a talk ability query (10), as the official client sends
/// it: the channel and `chnType` 0.
pub fn ability_extension(channel: u8) -> String {
    format!(
        "{XML_DECLARATION}\n<Extension version=\"1.1\">\n<channelId>{channel}</channelId>\n\
         <chnType>0</chnType>\n</Extension>\n"
    )
}

/// Extension of a talk open (201) or close (11): the channel only.
pub fn channel_extension(channel: u8) -> String {
    crate::xml::channel_extension(channel)
}

/// Extension of a talk audio message (202).
pub fn stream_extension(channel: u8) -> String {
    format!(
        "{XML_DECLARATION}\n<Extension version=\"1.1\">\n<binaryData>1</binaryData>\n\
         <channelId>{channel}</channelId>\n</Extension>\n"
    )
}

/// Payload of a talk open (201), built from a fixed template.
pub fn open_body(channel: u8, cfg: &TalkConfig) -> String {
    format!(
        "{XML_DECLARATION}\n<body>\n<TalkConfig version=\"1.1\">\n<channelId>{channel}</channelId>\n\
         <duplex>{}</duplex>\n<audioStreamMode>{}</audioStreamMode>\n<audioConfig>\n\
         <audioType>adpcm</audioType>\n<sampleRate>{SAMPLE_RATE}</sampleRate>\n\
         <samplePrecision>16</samplePrecision>\n<lengthPerEncoder>{SAMPLES_PER_RECORD}</lengthPerEncoder>\n\
         <soundTrack>mono</soundTrack>\n</audioConfig>\n</TalkConfig>\n</body>\n",
        cfg.duplex.name(),
        cfg.stream_mode.name()
    )
}

/// Parses a talk open (201) body as a device would: the channel and the
/// request. For device simulators and tests.
pub fn parse_open_body(xml: &str) -> Result<(u8, TalkConfig, AudioConfig), XmlError> {
    let channel = crate::xml::find_text(xml, "channelId")?
        .ok_or(XmlError::Missing("channelId"))?
        .parse()
        .map_err(|_| XmlError::BadNumber("channelId"))?;
    let duplex = crate::xml::find_text(xml, "duplex")?
        .and_then(|s| Duplex::from_name(&s))
        .ok_or(XmlError::Missing("duplex"))?;
    let stream_mode = crate::xml::find_text(xml, "audioStreamMode")?
        .and_then(|s| AudioStreamMode::from_name(&s))
        .ok_or(XmlError::Missing("audioStreamMode"))?;
    let ability = parse_talk_ability(&format!(
        "<TalkAbility>{}</TalkAbility>",
        xml.split_once("<audioConfig>")
            .and_then(|(_, rest)| rest.split_once("</audioConfig>"))
            .map(|(inner, _)| format!("<audioConfig>{inner}</audioConfig>"))
            .unwrap_or_default()
    ))?;
    let audio = ability
        .configs
        .into_iter()
        .next()
        .ok_or(XmlError::Missing("audioConfig"))?;
    Ok((
        channel,
        TalkConfig {
            duplex,
            stream_mode,
        },
        audio,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    const ABILITY: &str = "<?xml version=\"1.0\" encoding=\"UTF-8\" ?>\n<body>\n\
        <TalkAbility version=\"1.1\">\n<duplexList>\n<duplex>FDX</duplex>\n</duplexList>\n\
        <audioStreamModeList>\n<audioStreamMode>followVideoStream</audioStreamMode>\n\
        <audioStreamMode>mixAudioStream</audioStreamMode>\n</audioStreamModeList>\n\
        <audioConfigList>\n<audioConfig>\n<priority>0</priority>\n<audioType>adpcm</audioType>\n\
        <sampleRate>16000</sampleRate>\n<samplePrecision>16</samplePrecision>\n\
        <lengthPerEncoder>1024</lengthPerEncoder>\n<soundTrack>mono</soundTrack>\n\
        </audioConfig>\n</audioConfigList>\n</TalkAbility>\n</body>\n";

    fn tone(n: usize, freq: f64, amp: f64) -> Vec<i16> {
        (0..n)
            .map(|i| (amp * (2.0 * std::f64::consts::PI * freq * i as f64 / 16_000.0).sin()) as i16)
            .collect()
    }

    fn snr_db(a: &[i16], b: &[i16], skip: usize) -> f64 {
        let (mut sig, mut err) = (0.0f64, 0.0f64);
        for (x, y) in a.iter().zip(b).skip(skip) {
            sig += f64::from(*x).powi(2);
            err += (f64::from(*x) - f64::from(*y)).powi(2);
        }
        10.0 * (sig / err.max(1e-9)).log10()
    }

    #[test]
    fn records_have_the_observed_layout_and_round_trip() {
        let src = tone(SAMPLES_PER_RECORD * 8, 440.0, 8000.0);
        let mut enc = AdpcmEncoder::new();
        let mut out = Vec::new();
        let mut prev_end = AdpcmState::default();
        for (i, block) in src.as_chunks::<SAMPLES_PER_RECORD>().0.iter().enumerate() {
            let payload = enc.encode_record(block);
            assert_eq!(payload.len(), RECORD_PAYLOAD_LEN);
            assert_eq!(&payload[..4], &RECORD_PREFIX);
            assert_eq!(payload[7], 0);
            if i == 0 {
                assert_eq!(&payload[4..7], &[0, 0, 0]);
            }
            let d = decode_record(&payload).unwrap();
            // Continuity: each record starts where the previous one ended.
            assert_eq!(d.start, prev_end);
            assert_eq!(d.end, enc.state());
            prev_end = d.end;
            out.extend(d.samples);
        }
        assert_eq!(out.len(), src.len());
        let snr = snr_db(&src, &out, 200);
        assert!(snr > 20.0, "SNR {snr:.1} dB");
    }

    #[test]
    fn high_nibble_is_the_first_sample() {
        // A positive step then a negative one: 0x7 raises, 0xF lowers.
        let mut payload = vec![0, 1, 2, 0, 0, 0, 0, 0, 0x7F];
        payload.resize(RECORD_PAYLOAD_LEN, 0x00);
        let d = decode_record(&payload).unwrap();
        assert!(d.samples[0] > 0, "{:?}", &d.samples[..2]);
        assert!(d.samples[1] < d.samples[0]);
    }

    #[test]
    fn silence_and_full_scale_stay_in_range() {
        let mut enc = AdpcmEncoder::new();
        let silent = enc.encode_record(&[0; SAMPLES_PER_RECORD]);
        let d = decode_record(&silent).unwrap();
        assert!(d.samples.iter().all(|s| s.abs() < 16));
        let mut square = [0i16; SAMPLES_PER_RECORD];
        for (i, s) in square.iter_mut().enumerate() {
            *s = if (i / 8) % 2 == 0 { i16::MAX } else { i16::MIN };
        }
        for _ in 0..4 {
            let d = decode_record(&enc.encode_record(&square)).unwrap();
            assert!(d.end.index <= 88);
        }
    }

    #[test]
    fn bad_records_are_refused() {
        assert_eq!(decode_record(&[0; 7]), Err(TalkAudioError::Truncated));
        assert_eq!(
            decode_record(&[0, 1, 2, 0, 0, 0, 89, 0]),
            Err(TalkAudioError::BadStepIndex(89))
        );
    }

    #[test]
    fn ability_and_choice() {
        let a = parse_talk_ability(ABILITY).unwrap();
        assert_eq!(a.duplex, vec![Duplex::Full]);
        assert_eq!(
            a.stream_modes,
            vec![
                AudioStreamMode::FollowVideoStream,
                AudioStreamMode::MixAudioStream
            ]
        );
        assert_eq!(a.configs.len(), 1);
        assert!(a.configs[0].is_supported());
        assert_eq!(
            a.choose(),
            Some(TalkConfig {
                duplex: Duplex::Full,
                stream_mode: AudioStreamMode::FollowVideoStream
            })
        );
        let g711 = ABILITY.replace("adpcm", "g711");
        assert_eq!(parse_talk_ability(&g711).unwrap().choose(), None);
        let rate = ABILITY.replace("16000", "8000");
        assert_eq!(parse_talk_ability(&rate).unwrap().choose(), None);
        assert!(parse_talk_ability("<body><Other/></body>").is_err());
    }

    #[test]
    fn open_body_matches_the_observed_request() {
        let cfg = TalkConfig {
            duplex: Duplex::Full,
            stream_mode: AudioStreamMode::FollowVideoStream,
        };
        let body = open_body(5, &cfg);
        assert_eq!(
            body,
            "<?xml version=\"1.0\" encoding=\"UTF-8\" ?>\n<body>\n<TalkConfig version=\"1.1\">\n\
             <channelId>5</channelId>\n<duplex>FDX</duplex>\n\
             <audioStreamMode>followVideoStream</audioStreamMode>\n<audioConfig>\n\
             <audioType>adpcm</audioType>\n<sampleRate>16000</sampleRate>\n\
             <samplePrecision>16</samplePrecision>\n<lengthPerEncoder>1024</lengthPerEncoder>\n\
             <soundTrack>mono</soundTrack>\n</audioConfig>\n</TalkConfig>\n</body>\n"
        );
        let (ch, back, audio) = parse_open_body(&body).unwrap();
        assert_eq!((ch, back), (5, cfg));
        assert!(audio.is_supported());
        // Observed sizes: the 201/11 extension of a one-digit channel is
        // 104 bytes, the 202 extension 131.
        assert_eq!(channel_extension(8).len(), 104);
        assert_eq!(stream_extension(8).len(), 131);
        assert!(ability_extension(3).contains("<chnType>0</chnType>"));
    }

    #[test]
    fn talk_channels_from_support() {
        let xml = "<body><Support version=\"1.1\"><channelNum>4</channelNum>\
            <item><chnID>0</chnID><ipcAudioTalk>0</ipcAudioTalk></item>\
            <item><chnID>1</chnID><aitype>0</aitype><ipcAudioTalk>1</ipcAudioTalk></item>\
            <item><chnID>2</chnID><subItem><chnID>0</chnID><ipcAudioTalk>1</ipcAudioTalk></subItem>\
            <ipcAudioTalk>0</ipcAudioTalk></item>\
            <item><chnID>3</chnID></item></Support></body>";
        assert_eq!(parse_talk_channels(xml).unwrap(), vec![1]);
        assert_eq!(parse_talk_channels("<body/>").unwrap(), Vec::<u8>::new());
    }
}
