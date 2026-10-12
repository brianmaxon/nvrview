//! A fake recording store: synthetic recording files and events across
//! channels and days, served the way docs/protocol.md describes a device
//! serving them (day records 142, file search 14–16, event search 272–274,
//! cover images 298, download by time range 143 with its stop 144).
//!
//! Every configured day is recorded continuously on every channel, in files
//! of [`ArchiveConfig::file_minutes`]. Events come from a deterministic
//! generator plus any explicit [`MockEvent`]s. A download is a synthetic
//! BCMEDIA stream cut at the keyframe at or before the requested start, the
//! way devices cut (a keyframe every [`ArchiveConfig::gop_secs`]).
//!
//! Faults can be injected into downloads ([`DownloadFault`]): stall (stop
//! sending, keep the connection) or disconnect, after a number of bytes.

use crate::audio::AudioFixture;
use crate::fixture::Fixture;
use bc_protocol::archive::{DateTime, Element};
use bc_protocol::media::{encode_audio, encode_info, encode_video};
use bc_protocol::stream::StreamTier;
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// An explicit event in the fake store.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MockEvent {
    /// Channel.
    pub channel: u8,
    /// Type as a device names it (`md`, `people`, `vehicle`, ...).
    pub kind: String,
    /// Start.
    pub start: DateTime,
    /// End.
    pub end: DateTime,
}

/// What an injected download fault does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FaultKind {
    /// Stop sending but keep the connection open (the client must notice
    /// the stall and stop the download itself).
    Stall,
    /// Close the connection.
    Disconnect,
}

/// A fault applied to the next matching download, once.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DownloadFault {
    /// Only downloads of this channel (any channel when `None`).
    pub channel: Option<u8>,
    /// Fault after this many payload bytes of media were sent.
    pub after_bytes: usize,
    /// What happens.
    pub kind: FaultKind,
    /// Matching downloads to let through untouched first.
    pub skip: u32,
}

/// One download the fake served, for test assertions.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DownloadLog {
    /// Channel.
    pub channel: u8,
    /// Requested start.
    pub start: DateTime,
    /// Requested end.
    pub end: DateTime,
    /// Stream requested.
    pub stream: Option<StreamTier>,
}

/// Settings of the fake recording store.
#[derive(Clone, Debug)]
pub struct ArchiveConfig {
    /// Days that have recordings (any time of day; only the date counts).
    pub days: Vec<DateTime>,
    /// Length of each recording file in minutes (files tile every day).
    pub file_minutes: u32,
    /// Generated events per channel and day (deterministic).
    pub events_per_day: u32,
    /// Types the generator cycles through.
    pub event_types: Vec<String>,
    /// Explicit events, in addition to generated ones.
    pub events: Vec<MockEvent>,
    /// Files per 15 reply; after the last page, 15 answers an empty list.
    pub file_page: usize,
    /// Events per 273 page (devices were seen sending up to 50).
    pub event_page: usize,
    /// Video frames per second of recorded video.
    pub fps: u32,
    /// Seconds between keyframes.
    pub gop_secs: u32,
    /// Channels whose plays use their own keyframe spacing in seconds
    /// instead of [`ArchiveConfig::gop_secs`] (cameras of one NVR differ).
    pub gop_by_channel: Vec<(u8, u32)>,
    /// Audio interleaved in downloads, one ADTS frame every 64 ms, with two
    /// records before the first keyframe as devices send (docs/protocol.md).
    pub audio: Option<Arc<AudioFixture>>,
    /// Throttle for download media in bytes per second (unpaced if `None`,
    /// as devices send downloads).
    pub download_rate: Option<u64>,
    /// Seconds of video a play (command 5) sends at once before pacing to
    /// real time (observed: 2 to 4.4 s within the first quarter second).
    pub play_burst_secs: u32,
    /// Plays a connection may run at once; one more is answered status 400
    /// (the mock's choice: how a device refuses a play is not known). `None`:
    /// no limit.
    pub max_plays: Option<usize>,
    /// How long a file search (14) takes to answer (the mock's choice, for
    /// tests of what happens while a fill runs; devices take a fraction of a
    /// second). `None`: at once.
    pub search_delay: Option<std::time::Duration>,
    /// The device's offset from UTC in seconds (east positive). Recording
    /// times, searches, and seeks are device local; the wall-clock seconds
    /// in keyframes' extra headers are UTC, that is local time less this
    /// (observed, docs/protocol.md, "Rates, keyframes, and how a play
    /// ends"). Default 0.
    pub utc_offset_s: i64,
    /// Whether a play that reaches the end of the recordings ends with an
    /// empty message (the mock's choice; default true). A device was never
    /// seen ending a play (docs/protocol.md): with `false` the play just
    /// stops sending, as far as anything is known.
    pub play_end_message: bool,
    /// Recording files that do not exist: (channel, file start). The file
    /// search leaves them out and plays skip their time (a camera that did
    /// not record then).
    pub missing: Vec<(u8, DateTime)>,
    /// Channels whose plays are accepted but send nothing (a fault for
    /// tests of a play that never delivers).
    pub silent_plays: Vec<u8>,
    /// Faults for the next plays of chosen channels (for tests of how
    /// cameras of one playback are kept together). Shared: a test keeps a
    /// clone and adds faults while the mock runs.
    pub play_faults: PlayFaults,
}

/// How one play misbehaves (a fault for tests).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlayFaultKind {
    /// The play's first media comes this long after the play started (a
    /// camera slow to start); its pacing starts then.
    FirstMediaAfter(Duration),
    /// The play begins at the first keyframe at least this many seconds
    /// after the seek time instead of the one at or before it (devices were
    /// seen starting a play at a keyframe after the seek time,
    /// docs/protocol.md, "Rates, keyframes, and how a play ends").
    FirstKeyframeAfter(u32),
    /// After this many seconds of video the play sends nothing for `pause`,
    /// then goes on at its pace, that much later than before.
    Stall {
        /// Seconds of video sent before the stall.
        after_secs: u32,
        /// How long nothing is sent.
        pause: Duration,
    },
}

/// Faults waiting for the next served play of a channel; each is used by
/// one play.
#[derive(Clone, Debug, Default)]
pub struct PlayFaults(Arc<Mutex<Vec<(u8, PlayFaultKind)>>>);

impl PlayFaults {
    /// Adds a fault for the next served play of `channel`.
    pub fn add(&self, channel: u8, kind: PlayFaultKind) {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push((channel, kind));
    }

    /// Takes the faults waiting for a play of `channel`.
    pub(crate) fn take(&self, channel: u8) -> Vec<PlayFaultKind> {
        let mut v = self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let (mine, rest): (Vec<_>, Vec<_>) = v.drain(..).partition(|(c, _)| *c == channel);
        *v = rest;
        mine.into_iter().map(|(_, k)| k).collect()
    }
}

/// One play (command 5) the fake served, for test assertions.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlayLog {
    /// Channel.
    pub channel: u8,
    /// The file `Id` it named.
    pub file_id: String,
    /// The latest seek's time when the play arrived.
    pub at: DateTime,
    /// The latest seek's `seq` when the play arrived.
    pub seq: u32,
    /// `playSpeed` as sent.
    pub speed: Option<u32>,
    /// Stream requested.
    pub stream: Option<StreamTier>,
    /// `supportSub` as sent.
    pub support_sub: Option<u8>,
    /// Whether the named file holds the seek time, so the play was served
    /// (a play whose file does not hold the session's seek position gets
    /// nothing, as observed: docs/protocol.md, "Live view and plays on one
    /// session").
    pub served: bool,
}

impl ArchiveConfig {
    /// Continuous recordings on `days`, 60-minute files, 6 generated events
    /// per channel and day, 10 fps, a keyframe every 2 s, no audio.
    pub fn new(days: Vec<DateTime>) -> ArchiveConfig {
        ArchiveConfig {
            days,
            file_minutes: 60,
            events_per_day: 6,
            event_types: vec!["md".into(), "people".into(), "vehicle".into()],
            events: Vec::new(),
            file_page: usize::MAX,
            event_page: 50,
            fps: 10,
            gop_secs: 2,
            gop_by_channel: Vec::new(),
            audio: None,
            download_rate: None,
            play_burst_secs: 3,
            max_plays: None,
            search_delay: None,
            utc_offset_s: 0,
            play_end_message: true,
            missing: Vec::new(),
            silent_plays: Vec::new(),
            play_faults: PlayFaults::default(),
        }
    }

    /// One past the last recorded second (naive seconds).
    pub(crate) fn recorded_end(&self) -> i64 {
        self.sorted_days().last().map_or(0, |d| ts(*d) + 86_400)
    }

    /// Whether `channel` has no recording at naive second `t` (a file
    /// listed in [`ArchiveConfig::missing`]).
    pub(crate) fn missing_at(&self, channel: u8, t: i64) -> bool {
        if self.missing.is_empty() {
            return false;
        }
        let len = i64::from(self.file_minutes.max(1)) * 60;
        let day = t.div_euclid(86_400) * 86_400;
        let start = day + (t - day).div_euclid(len) * len;
        self.missing
            .iter()
            .any(|(c, s)| *c == channel && ts(*s) == start)
    }

    fn has_day(&self, t: DateTime) -> bool {
        self.days
            .iter()
            .any(|d| (d.year, d.month, d.day) == (t.year, t.month, t.day))
    }

    fn sorted_days(&self) -> Vec<DateTime> {
        let mut v: Vec<DateTime> = self.days.iter().map(|d| d.start_of_day()).collect();
        v.sort();
        v.dedup();
        v
    }
}

/// Whether the file `Id` of `channel` names a file of the store that holds
/// `at` (files tile each recorded day from midnight).
pub(crate) fn file_holds(cfg: &ArchiveConfig, channel: u8, file_id: &str, at: DateTime) -> bool {
    let Some(start) = file_id
        .strip_prefix(&format!("mock-file-{channel}-"))
        .and_then(|s| s.parse::<i64>().ok())
    else {
        return false;
    };
    let len = i64::from(cfg.file_minutes.max(1)) * 60;
    let day_end = start.div_euclid(86_400) * 86_400 + 86_400;
    let t = ts(at);
    cfg.has_day(dt(start))
        && !cfg.missing_at(channel, start)
        && start <= t
        && t < (start + len).min(day_end)
}

/// A recording file of the fake store.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct FileEntry {
    pub channel: u8,
    pub start: DateTime,
    pub end: DateTime,
}

impl FileEntry {
    pub fn id(&self) -> String {
        format!("mock-file-{}-{}", self.channel, self.start.to_naive_secs())
    }
}

pub(crate) fn ts(t: DateTime) -> i64 {
    t.to_naive_secs()
}

pub(crate) fn dt(s: i64) -> DateTime {
    DateTime::from_naive_secs(s).unwrap_or(DateTime {
        year: 1970,
        month: 1,
        day: 1,
        hour: 0,
        minute: 0,
        second: 0,
    })
}

/// Files of `channel` overlapping `[start, end]`.
pub(crate) fn files(
    cfg: &ArchiveConfig,
    channel: u8,
    start: DateTime,
    end: DateTime,
) -> Vec<FileEntry> {
    let len = i64::from(cfg.file_minutes.max(1)) * 60;
    let mut out = Vec::new();
    for day in cfg.sorted_days() {
        let d0 = ts(day);
        let mut s = d0;
        while s < d0 + 86_400 {
            let e = (s + len).min(d0 + 86_400) - 1;
            if e >= ts(start) && s <= ts(end) && !cfg.missing_at(channel, s) {
                out.push(FileEntry {
                    channel,
                    start: dt(s),
                    end: dt(e),
                });
            }
            s += len;
        }
    }
    out
}

/// Small deterministic generator.
struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Rng {
        Rng(seed.wrapping_mul(0x9e37_79b9_7f4a_7c15) | 1)
    }
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
}

/// Every event of `channel` overlapping `[start, end]`, sorted by start.
pub(crate) fn events(
    cfg: &ArchiveConfig,
    channel: u8,
    start: DateTime,
    end: DateTime,
) -> Vec<MockEvent> {
    let mut out: Vec<MockEvent> = Vec::new();
    for day in cfg.sorted_days() {
        let d0 = ts(day);
        let mut rng = Rng::new((d0 as u64) ^ (u64::from(channel) << 40));
        for i in 0..cfg.events_per_day {
            let s = d0 + (rng.next() % 86_000) as i64;
            let dur = 10 + (rng.next() % 16) as i64;
            let kind = cfg
                .event_types
                .get(i as usize % cfg.event_types.len().max(1))
                .cloned()
                .unwrap_or_else(|| "md".into());
            out.push(MockEvent {
                channel,
                kind,
                start: dt(s),
                end: dt(s + dur),
            });
        }
    }
    out.extend(cfg.events.iter().filter(|e| e.channel == channel).cloned());
    out.retain(|e| cfg.has_day(e.start) && ts(e.end) >= ts(start) && ts(e.start) <= ts(end));
    out.sort_by_key(|e| (ts(e.start), ts(e.end), e.kind.clone()));
    out
}

pub(crate) fn time_of(el: &Element, name: &str) -> Option<DateTime> {
    let t = el.child(name)?;
    let n = |k: &'static str| t.num::<u32>(k).ok().flatten();
    DateTime::new(
        u16::try_from(n("year")?).ok()?,
        n("month")? as u8,
        n("day")? as u8,
        n("hour")? as u8,
        n("minute")? as u8,
        n("second")? as u8,
    )
}

/// The parts of a request body the fake reads.
pub(crate) struct Req {
    pub channel: Option<u8>,
    pub start: Option<DateTime>,
    pub end: Option<DateTime>,
    pub handle: Option<u32>,
    pub stream: Option<StreamTier>,
}

/// Reads channel, times, handle, and stream from the first element named
/// `record` (or the whole document).
pub(crate) fn read_req(xml: &str, record: &str) -> Req {
    let root = Element::parse(xml).unwrap_or_default();
    let el = root.find(record).unwrap_or(&root);
    Req {
        channel: el.num("channelId").ok().flatten(),
        start: time_of(el, "startTime"),
        end: time_of(el, "endTime"),
        handle: el
            .num("handle")
            .ok()
            .flatten()
            .or_else(|| el.num("fileHandle").ok().flatten()),
        stream: el.text_of("streamType").and_then(StreamTier::from_name),
    }
}

fn time_xml(t: DateTime, tag: &str) -> String {
    t.xml(tag)
}

/// Reply to a 142 request.
pub(crate) fn day_records_reply(cfg: &ArchiveConfig, channels: u8, xml: &str) -> Option<String> {
    let root = Element::parse(xml).ok()?;
    let dr = root.find("DayRecords")?;
    let start = time_of(dr, "startTime")?;
    let end = time_of(dr, "endTime")?;
    let mut s = String::from("<DayRecords version=\"1.1\">\n");
    s.push_str(&time_xml(start, "startTime"));
    s.push_str(&time_xml(end, "endTime"));
    s.push_str("<DayRecordList>\n");
    for rec in dr.find_all("DayRecord") {
        let index: u32 = rec.num("index").ok().flatten().unwrap_or(0);
        let Some(ch) = rec.num::<u8>("channelId").ok().flatten() else {
            continue;
        };
        s.push_str(&format!(
            "<DayRecord>\n<index>{index}</index>\n<channelId>{ch}</channelId>\n<dayTypeList>\n"
        ));
        if ch < channels {
            for day in cfg.sorted_days() {
                if day >= start.start_of_day() && day <= end {
                    s.push_str(&format!(
                        "<dayType>\n<index>{}</index>\n<type>normal</type>\n</dayType>\n",
                        day.day - 1
                    ));
                }
            }
        }
        s.push_str("</dayTypeList>\n</DayRecord>\n");
    }
    s.push_str("</DayRecordList>\n</DayRecords>\n");
    Some(s)
}

/// The 14 reply summary.
pub(crate) fn file_search_summary(
    channel: u8,
    handle: u32,
    files: &[FileEntry],
    start: DateTime,
    end: DateTime,
) -> String {
    let types = if files.is_empty() { "" } else { "sched ,md" };
    format!(
        "<FileInfoList version=\"1.1\">\n<FileInfo>\n<channelId>{channel}</channelId>\n\
         <handle>{handle}</handle>\n<name></name>\n<containsAudio>1</containsAudio>\n\
         <recordType>{types}</recordType>\n<supportSub>1</supportSub>\n<sizeL>0</sizeL>\n<sizeH>0</sizeH>\n\
         {}{}</FileInfo>\n</FileInfoList>\n",
        time_xml(start, "startTime"),
        time_xml(end, "endTime"),
    )
}

/// One 15 reply page.
pub(crate) fn file_page(handle: u32, files: &[FileEntry]) -> String {
    let mut s = String::from("<FileInfoList version=\"1.1\">\n");
    for f in files {
        let size: u64 = (ts(f.end) - ts(f.start)) as u64 * 250_000;
        s.push_str(&format!(
            "<FileInfo>\n<channelId>{}</channelId>\n<handle>{handle}</handle>\n<name>{}</name>\n\
             <Id>{}</Id>\n<streamType>mainStream</streamType>\n<containsAudio>1</containsAudio>\n\
             <recordType>sched</recordType>\n<sizeL>{}</sizeL>\n<sizeH>{}</sizeH>\n\
             <supportSub>1</supportSub>\n<encrypt>0</encrypt>\n{}{}</FileInfo>\n",
            f.channel,
            format_args!("mock-name-{}-{}", f.channel, ts(f.start)),
            f.id(),
            size & 0xffff_ffff,
            size >> 32,
            time_xml(f.start, "startTime"),
            time_xml(f.end, "endTime"),
        ));
    }
    s.push_str("</FileInfoList>\n");
    s
}

/// One 273 reply page.
pub(crate) fn event_page(
    cfg: &ArchiveConfig,
    channel: u8,
    handle: u32,
    events: &[MockEvent],
    finished: bool,
) -> String {
    let mut s = format!(
        "<alarmVideoInfo version=\"1.1\">\n<channelId>{channel}</channelId>\n<fileHandle>{handle}</fileHandle>\n\
         <bFinished>{}</bFinished>\n<alarmVideoList>\n",
        u8::from(finished)
    );
    for e in events {
        let file = files(cfg, channel, e.start, e.start)
            .into_iter()
            .next()
            .map(|f| f.id())
            .unwrap_or_default();
        s.push_str(&format!(
            "<alarmVideo>\n<fileName>mock-event-{channel}-{}</fileName>\n<bHasRecFile>1</bHasRecFile>\n\
             <bEncrypted>0</bEncrypted>\n<alarmType>{}</alarmType>\n<fileId>{file}</fileId>\n{}{}</alarmVideo>\n",
            ts(e.start),
            e.kind,
            time_xml(e.start, "startTime"),
            time_xml(e.end, "endTime"),
        ));
    }
    s.push_str("</alarmVideoList>\n</alarmVideoInfo>\n");
    s
}

/// What kind of record a media record is, for choosing `encryptLen`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Kind {
    Info,
    Key,
    Predicted,
    Audio,
}

/// Synthetic recorded media for `[start, end]` of a channel: starts at the
/// keyframe at or before `start`. Video timestamps are microseconds of the
/// device's local wall clock (as naive seconds), and every video record
/// carries the 8-byte extra header whose first four bytes are those seconds
/// in UTC (less [`ArchiveConfig::utc_offset_s`]).
/// Empty when the window has no recording.
pub(crate) fn media(
    cfg: &ArchiveConfig,
    fixture: &Fixture,
    width: u32,
    height: u32,
    start: DateTime,
    end: DateTime,
) -> Vec<(Kind, Vec<u8>)> {
    let mut out = Vec::new();
    let gop = i64::from(cfg.gop_secs.max(1));
    let first = ts(start).div_euclid(gop) * gop;
    let last = ts(end);
    if last < first {
        return out;
    }
    let fps = u64::from(cfg.fps.max(1));
    let frame_us = 1_000_000 / fps;
    out.push((
        Kind::Info,
        encode_info(width, height, cfg.fps.min(255) as u8),
    ));
    let key_index = fixture.first_keyframe();
    let mut p_index = fixture.next_index(key_index);
    let mut audio_k: u64 = 0;
    // Audio records whose 64 ms frame ended at or before this time (µs) have
    // been sent.
    let audio_us = |k: u64| (k + 1) * 64_000;
    let audio_frames = cfg.audio.as_ref().map(|a| a.frames());
    if let Some(frames) = audio_frames {
        // Audio that precedes the first picture, as devices send it.
        for _ in 0..2 {
            out.push((Kind::Audio, encode_audio(5, &frames[0])));
        }
    }
    let span_us = ((last + 1 - first) as u64) * 1_000_000;
    let mut t_us: u64 = 0;
    let mut have_pictures = false;
    while t_us < span_us {
        let secs = first + (t_us / 1_000_000) as i64;
        if cfg.has_day(dt(secs)) {
            let abs_us = (secs as u64).wrapping_mul(1_000_000) + t_us % 1_000_000;
            let keyframe = t_us % (gop as u64 * 1_000_000) < frame_us;
            let data = if keyframe {
                &fixture.frames()[key_index].data
            } else {
                let mut i = p_index;
                while fixture.frames()[i].keyframe {
                    i = fixture.next_index(i);
                }
                p_index = fixture.next_index(i);
                &fixture.frames()[i].data
            };
            if keyframe || have_pictures {
                let mut extra = [0u8; 8];
                extra[..4].copy_from_slice(&((secs - cfg.utc_offset_s) as u32).to_le_bytes());
                let rec = encode_video(0, keyframe, fixture.codec(), abs_us, &extra, data);
                out.push((if keyframe { Kind::Key } else { Kind::Predicted }, rec));
                have_pictures = true;
            }
            if let Some(frames) = audio_frames {
                while audio_us(audio_k) <= t_us + frame_us {
                    let f = &frames[(audio_k as usize) % frames.len()];
                    out.push((Kind::Audio, encode_audio(5, f)));
                    audio_k += 1;
                }
            }
        } else {
            have_pictures = false;
        }
        t_us += frame_us;
    }
    if !out.iter().any(|(k, _)| *k == Kind::Key) {
        out.clear();
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use bc_protocol::media::{Demuxer, Record};
    use std::path::PathBuf;

    fn d(y: u16, m: u8, day: u8, h: u8, mi: u8, s: u8) -> DateTime {
        DateTime::new(y, m, day, h, mi, s).unwrap()
    }

    fn fixture() -> Fixture {
        Fixture::load(
            &PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../../fixtures/synthetic/h264-basic.h264"),
        )
        .unwrap()
    }

    #[test]
    fn files_tile_each_day() {
        let cfg = ArchiveConfig::new(vec![d(2026, 10, 9, 0, 0, 0)]);
        let all = files(&cfg, 1, d(2026, 10, 9, 0, 0, 0), d(2026, 10, 9, 23, 59, 59));
        assert_eq!(all.len(), 24);
        assert_eq!(all[0].start, d(2026, 10, 9, 0, 0, 0));
        assert_eq!(all[23].end, d(2026, 10, 9, 23, 59, 59));
        assert!(
            files(
                &cfg,
                1,
                d(2026, 10, 10, 0, 0, 0),
                d(2026, 10, 10, 23, 59, 59)
            )
            .is_empty()
        );
    }

    #[test]
    fn events_are_deterministic_and_include_explicit_ones() {
        let mut cfg = ArchiveConfig::new(vec![d(2026, 10, 9, 0, 0, 0)]);
        cfg.events.push(MockEvent {
            channel: 2,
            kind: "vehicle".into(),
            start: d(2026, 10, 9, 7, 42, 5),
            end: d(2026, 10, 9, 7, 42, 25),
        });
        let day = (d(2026, 10, 9, 0, 0, 0), d(2026, 10, 9, 23, 59, 59));
        let a = events(&cfg, 2, day.0, day.1);
        assert_eq!(a, events(&cfg, 2, day.0, day.1));
        assert_eq!(a.len(), 7);
        assert_ne!(a, events(&cfg, 1, day.0, day.1));
        assert!(a.windows(2).all(|w| w[0].start <= w[1].start));
    }

    #[test]
    fn media_starts_at_a_keyframe_at_or_before_the_start() {
        let mut cfg = ArchiveConfig::new(vec![d(2026, 10, 9, 0, 0, 0)]);
        cfg.audio = Some(Arc::new(
            AudioFixture::load(
                &PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                    .join("../../fixtures/synthetic/aac-tone.aac"),
            )
            .unwrap(),
        ));
        let recs = media(
            &cfg,
            &fixture(),
            320,
            240,
            d(2026, 10, 9, 7, 42, 3),
            d(2026, 10, 9, 7, 42, 9),
        );
        let mut dm = Demuxer::new();
        for (_, r) in &recs {
            dm.push(r);
        }
        let mut video = Vec::new();
        let mut audio = 0;
        while let Some(r) = dm.next_record().unwrap() {
            match r {
                Record::Video {
                    keyframe, pts_us, ..
                } => video.push((keyframe, pts_us)),
                Record::Audio { .. } => audio += 1,
                Record::Info { .. } => {}
            }
        }
        let start = d(2026, 10, 9, 7, 42, 2).to_naive_secs() as u64 * 1_000_000;
        assert_eq!(video[0], (true, start));
        // 7:42:02 to 7:42:09 inclusive at 10 fps.
        assert_eq!(video.len(), 80);
        assert_eq!(video.iter().filter(|v| v.0).count(), 4);
        assert!(audio > 100);
        assert!(
            media(
                &cfg,
                &fixture(),
                320,
                240,
                d(2026, 10, 10, 1, 0, 0),
                d(2026, 10, 10, 1, 1, 0)
            )
            .is_empty()
        );
    }
}
