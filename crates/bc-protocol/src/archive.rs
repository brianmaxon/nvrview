//! Recorded video on the device: which days have recordings (command 142),
//! recording files (14, 15, 16), motion and detection events (272, 273, 274),
//! cover images (298), and download by time range (143, 144).
//!
//! This module holds the request bodies and the reply parsers; the session
//! ([`crate::session::Session`]) builds the messages. Every command here is
//! read-only (viewing tier): it lists or copies recordings and changes nothing
//! on the device. The facts are in `docs/protocol.md`, "Recordings: search and
//! download", "Playback", "Cover images: command 298", and "Download by time
//! range: command 143".

use crate::stream::StreamTier;
use crate::xml::{XML_DECLARATION, XmlError};
use quick_xml::Reader;
use quick_xml::events::Event;
use std::fmt;
use std::str::FromStr;

/// A time in the device's local wall clock, with no zone or offset, as every
/// recording body carries it (six elements: year, month 1–12, day, hour,
/// minute, second).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct DateTime {
    /// Year, e.g. 2026.
    pub year: u16,
    /// Month, 1–12.
    pub month: u8,
    /// Day of the month, from 1.
    pub day: u8,
    /// Hour, 0–23.
    pub hour: u8,
    /// Minute, 0–59.
    pub minute: u8,
    /// Second, 0–59.
    pub second: u8,
}

/// Number of days in a month (`month` 1–12), with Gregorian leap years.
pub const fn days_in_month(year: u16, month: u8) -> u8 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if (year.is_multiple_of(4) && !year.is_multiple_of(100)) || year.is_multiple_of(400) => {
            29
        }
        2 => 28,
        _ => 0,
    }
}

/// Days since 1970-01-01 of a civil date (proleptic Gregorian).
const fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// Civil date of a day count since 1970-01-01.
const fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + if m <= 2 { 1 } else { 0 };
    (y, m, d)
}

impl DateTime {
    /// A validated time; `None` if any field is out of range.
    pub fn new(year: u16, month: u8, day: u8, hour: u8, minute: u8, second: u8) -> Option<Self> {
        let ok = (1..=12).contains(&month)
            && day >= 1
            && day <= days_in_month(year, month)
            && hour < 24
            && minute < 60
            && second < 60;
        ok.then_some(DateTime {
            year,
            month,
            day,
            hour,
            minute,
            second,
        })
    }

    /// Midnight at the start of a date.
    pub fn date(year: u16, month: u8, day: u8) -> Option<Self> {
        DateTime::new(year, month, day, 0, 0, 0)
    }

    /// 00:00:00 of this time's day.
    pub fn start_of_day(self) -> Self {
        DateTime {
            hour: 0,
            minute: 0,
            second: 0,
            ..self
        }
    }

    /// 23:59:59 of this time's day, the end the official client sends.
    pub fn end_of_day(self) -> Self {
        DateTime {
            hour: 23,
            minute: 59,
            second: 59,
            ..self
        }
    }

    /// Seconds since 1970-01-01 00:00:00 *of the same wall clock*: an
    /// ordering and arithmetic aid, not a UTC instant (the device's zone is
    /// unknown).
    pub fn to_naive_secs(self) -> i64 {
        days_from_civil(
            i64::from(self.year),
            i64::from(self.month),
            i64::from(self.day),
        ) * 86_400
            + i64::from(self.hour) * 3600
            + i64::from(self.minute) * 60
            + i64::from(self.second)
    }

    /// Inverse of [`DateTime::to_naive_secs`]; `None` outside years 0–65535.
    pub fn from_naive_secs(secs: i64) -> Option<Self> {
        let days = secs.div_euclid(86_400);
        let rem = secs.rem_euclid(86_400);
        let (y, m, d) = civil_from_days(days);
        let year = u16::try_from(y).ok()?;
        Some(DateTime {
            year,
            month: m as u8,
            day: d as u8,
            hour: (rem / 3600) as u8,
            minute: (rem / 60 % 60) as u8,
            second: (rem % 60) as u8,
        })
    }

    /// The six-element block, wrapped in `<tag>`.
    pub fn xml(&self, tag: &str) -> String {
        format!(
            "<{tag}>\n<year>{}</year>\n<month>{}</month>\n<day>{}</day>\n<hour>{}</hour>\n\
             <minute>{}</minute>\n<second>{}</second>\n</{tag}>\n",
            self.year, self.month, self.day, self.hour, self.minute, self.second
        )
    }

    fn from_element(e: &Element) -> Result<DateTime, XmlError> {
        let n = |name: &'static str| -> Result<u32, XmlError> {
            e.num::<u32>(name)?.ok_or(XmlError::Missing(name))
        };
        let (y, mo, d) = (n("year")?, n("month")?, n("day")?);
        let (h, mi, s) = (n("hour")?, n("minute")?, n("second")?);
        let small =
            |v: u32, name: &'static str| u8::try_from(v).map_err(|_| XmlError::BadNumber(name));
        DateTime::new(
            u16::try_from(y).map_err(|_| XmlError::BadNumber("year"))?,
            small(mo, "month")?,
            small(d, "day")?,
            small(h, "hour")?,
            small(mi, "minute")?,
            small(s, "second")?,
        )
        .ok_or(XmlError::BadNumber("time"))
    }
}

impl fmt::Display for DateTime {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{:04}-{:02}-{:02} {:02}:{:02}:{:02}",
            self.year, self.month, self.day, self.hour, self.minute, self.second
        )
    }
}

/// Largest number of elements [`Element::parse`] accepts, to bound memory on
/// corrupt or hostile input. A busy day's event pages and file lists are far
/// smaller (a full 273 page is about 50 × 20 elements).
pub const MAX_ELEMENTS: usize = 200_000;
/// Deepest nesting [`Element::parse`] accepts.
pub const MAX_DEPTH: usize = 32;

/// A minimal XML element tree: enough to read nested replies such as the
/// time blocks inside each file or event.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Element {
    /// Local name (no namespace prefix).
    pub name: String,
    /// Concatenated, trimmed text directly inside the element.
    pub text: String,
    /// Child elements in document order.
    pub children: Vec<Element>,
}

impl Element {
    /// Parses a document. The result is an unnamed root whose children are
    /// the document's top-level elements.
    pub fn parse(xml: &str) -> Result<Element, XmlError> {
        let mut reader = Reader::from_str(xml);
        let mut stack: Vec<Element> = vec![Element::default()];
        let mut count = 0usize;
        loop {
            match reader.read_event().map_err(|_| XmlError::Malformed)? {
                Event::Start(e) => {
                    count += 1;
                    if count > MAX_ELEMENTS || stack.len() > MAX_DEPTH {
                        return Err(XmlError::Malformed);
                    }
                    stack.push(Element {
                        name: String::from_utf8_lossy(e.local_name().as_ref()).into_owned(),
                        ..Element::default()
                    });
                }
                Event::Empty(e) => {
                    count += 1;
                    if count > MAX_ELEMENTS {
                        return Err(XmlError::Malformed);
                    }
                    let el = Element {
                        name: String::from_utf8_lossy(e.local_name().as_ref()).into_owned(),
                        ..Element::default()
                    };
                    stack
                        .last_mut()
                        .ok_or(XmlError::Malformed)?
                        .children
                        .push(el);
                }
                Event::Text(t) => {
                    let s = t.xml10_content().map_err(|_| XmlError::Malformed)?;
                    stack
                        .last_mut()
                        .ok_or(XmlError::Malformed)?
                        .text
                        .push_str(&s);
                }
                Event::GeneralRef(r) => {
                    let s = crate::xml::resolve_ref(&r)?;
                    stack
                        .last_mut()
                        .ok_or(XmlError::Malformed)?
                        .text
                        .push_str(&s);
                }
                Event::End(_) => {
                    if stack.len() < 2 {
                        return Err(XmlError::Malformed);
                    }
                    let mut done = stack.pop().ok_or(XmlError::Malformed)?;
                    done.text = done.text.trim().to_owned();
                    stack
                        .last_mut()
                        .ok_or(XmlError::Malformed)?
                        .children
                        .push(done);
                }
                Event::Eof => break,
                _ => {}
            }
        }
        if stack.len() != 1 {
            return Err(XmlError::Malformed);
        }
        Ok(stack.pop().unwrap_or_default())
    }

    /// The first direct child named `name`.
    pub fn child(&self, name: &str) -> Option<&Element> {
        self.children.iter().find(|c| c.name == name)
    }

    /// Every direct child named `name`.
    pub fn children_named<'a>(&'a self, name: &'a str) -> impl Iterator<Item = &'a Element> + 'a {
        self.children.iter().filter(move |c| c.name == name)
    }

    /// The first element named `name` at any depth below this one
    /// (depth-first, document order).
    pub fn find(&self, name: &str) -> Option<&Element> {
        for c in &self.children {
            if c.name == name {
                return Some(c);
            }
            if let Some(f) = c.find(name) {
                return Some(f);
            }
        }
        None
    }

    /// Every element named `name` at any depth below this one, not looking
    /// inside matches.
    pub fn find_all<'a>(&'a self, name: &str) -> Vec<&'a Element> {
        let mut out = Vec::new();
        self.collect(name, &mut out);
        out
    }

    fn collect<'a>(&'a self, name: &str, out: &mut Vec<&'a Element>) {
        for c in &self.children {
            if c.name == name {
                out.push(c);
            } else {
                c.collect(name, out);
            }
        }
    }

    /// Text of the first direct child named `name`.
    pub fn text_of(&self, name: &str) -> Option<&str> {
        self.child(name).map(|c| c.text.as_str())
    }

    /// The first direct child named `name` parsed as a number; `Ok(None)`
    /// when absent, an error when present but not a number.
    pub fn num<T: FromStr>(&self, name: &'static str) -> Result<Option<T>, XmlError> {
        match self.text_of(name) {
            None => Ok(None),
            Some(s) => s.parse().map(Some).map_err(|_| XmlError::BadNumber(name)),
        }
    }

    fn time(&self, name: &'static str) -> Result<DateTime, XmlError> {
        DateTime::from_element(self.child(name).ok_or(XmlError::Missing(name))?)
    }
}

fn wrap(inner: &str) -> String {
    format!("{XML_DECLARATION}\n<body>\n{inner}</body>\n")
}

fn opt_uid(uid: Option<&str>) -> String {
    uid.map(|u| format!("<uid>{}</uid>\n", escape(u)))
        .unwrap_or_default()
}

/// Escapes text for an XML element (identifiers come from device replies and
/// are echoed back; never trust them to be markup-free).
fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            c => out.push(c),
        }
    }
    out
}

// ---------------------------------------------------------------------------
// 142: days with recordings

/// One channel to ask about in a 142 request.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DayRecordChannel {
    /// Channel number.
    pub channel: u8,
    /// The camera's device ID from the channel list (command 145), when known.
    pub uid: Option<String>,
}

/// Body of a day-records request (command 142). The official client sends
/// one per month: from the first day 00:00:00 to the last 23:59:59.
pub fn day_records_body(start: DateTime, end: DateTime, channels: &[DayRecordChannel]) -> String {
    let mut s = String::from("<DayRecords version=\"1.1\">\n");
    s.push_str(&start.xml("startTime"));
    s.push_str(&end.xml("endTime"));
    s.push_str("<DayRecordList>\n");
    for (i, c) in channels.iter().enumerate() {
        s.push_str(&format!(
            "<DayRecord>\n<index>{i}</index>\n<channelId>{}</channelId>\n{}</DayRecord>\n",
            c.channel,
            opt_uid(c.uid.as_deref())
        ));
    }
    s.push_str("</DayRecordList>\n</DayRecords>\n");
    wrap(&s)
}

/// One `dayType` entry of a 142 reply.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DayType {
    /// `index`: observed to be the day of the month minus one (inferred).
    pub index: u32,
    /// `type`: `normal` was seen; the client also knows `none` and `alarm`.
    pub kind: String,
}

impl DayType {
    /// Day of the month this entry stands for (`index + 1`; the meaning is
    /// inferred from observation, see docs/protocol.md).
    pub fn day_of_month(&self) -> u32 {
        self.index + 1
    }

    /// Whether the entry says there are recordings (`none` says not).
    pub fn has_recordings(&self) -> bool {
        self.kind != "none"
    }
}

/// One channel's days in a 142 reply.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DayRecords {
    /// Channel number.
    pub channel: u8,
    /// The `dayType` entries, in reply order.
    pub days: Vec<DayType>,
}

/// Parses a 142 reply. Entries without a valid channel are skipped.
pub fn parse_day_records(xml: &str) -> Result<Vec<DayRecords>, XmlError> {
    let root = Element::parse(xml)?;
    let mut out = Vec::new();
    for rec in root.find_all("DayRecord") {
        let Some(channel) = rec.num::<u8>("channelId").ok().flatten() else {
            continue;
        };
        let mut days = Vec::new();
        for list in rec.children_named("dayTypeList") {
            for d in list.children_named("dayType") {
                let Some(index) = d.num::<u32>("index")? else {
                    continue;
                };
                days.push(DayType {
                    index,
                    kind: d.text_of("type").unwrap_or_default().to_owned(),
                });
            }
        }
        out.push(DayRecords { channel, days });
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// 14, 15, 16: recording files

/// Recording causes the official client asked for in `recordType` (14).
pub const FILE_RECORD_TYPES: &[&str] =
    &["manual", "sched", "io", "md", "people", "face", "vehicle"];
/// Extra causes it listed only in `fileRecordType` (14).
pub const FILE_RECORD_TYPES_EXTRA: &[&str] = &["answer", "nonmotorveh", "IPCInputOutput"];
/// Event types the official client asked for in `alarmType` (272).
pub const ALARM_TYPES: &[&str] = &[
    "md", "pir", "other", "package", "io", "people", "face", "vehicle",
];

/// A file search (command 14) of one channel and one day.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileSearch {
    /// Channel number.
    pub channel: u8,
    /// The camera's device ID, when known.
    pub uid: Option<String>,
    /// Stream to search (the lens and tier).
    pub stream: StreamTier,
    /// Start of the window (the client sends 00:00:00 of the day).
    pub start: DateTime,
    /// End of the window (the client sends 23:59:59 of the same day).
    pub end: DateTime,
}

/// Body of a file search open (command 14).
pub fn file_search_open_body(q: &FileSearch) -> String {
    let items: String = FILE_RECORD_TYPES
        .iter()
        .chain(FILE_RECORD_TYPES_EXTRA)
        .map(|t| format!("<i>{t}</i>"))
        .collect();
    wrap(&format!(
        "<FileInfoList version=\"1.1\">\n<FileInfo>\n{}<searchAITrack>1</searchAITrack>\n\
         <channelId>{}</channelId>\n<logicChnBitmap>255</logicChnBitmap>\n\
         <streamType>{}</streamType>\n<recordType>{}</recordType>\n\
         <fileRecordType>{items}</fileRecordType>\n{}{}</FileInfo>\n</FileInfoList>\n",
        opt_uid(q.uid.as_deref()),
        q.channel,
        q.stream.name(),
        FILE_RECORD_TYPES.join(", "),
        q.start.xml("startTime"),
        q.end.xml("endTime"),
    ))
}

/// Body of a file search fetch (command 15) and close (command 16): the
/// channel, uid, `searchAITrack`, and the handle from the 14 reply.
pub fn file_search_handle_body(channel: u8, uid: Option<&str>, handle: u32) -> String {
    wrap(&format!(
        "<FileInfoList version=\"1.1\">\n<FileInfo>\n<channelId>{channel}</channelId>\n{}\
         <searchAITrack>1</searchAITrack>\n<handle>{handle}</handle>\n</FileInfo>\n</FileInfoList>\n",
        opt_uid(uid)
    ))
}

/// The summary a 14 reply carries.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileSearchOpened {
    /// Channel number.
    pub channel: Option<u8>,
    /// Search handle to send with 15 and 16.
    pub handle: u32,
    /// Recording causes present in the window (`recordType`, split on commas).
    pub record_types: Vec<String>,
}

fn split_types(s: &str) -> Vec<String> {
    s.split(',')
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .map(str::to_owned)
        .collect()
}

/// Parses a 14 reply.
pub fn parse_file_search_opened(xml: &str) -> Result<FileSearchOpened, XmlError> {
    let root = Element::parse(xml)?;
    let f = root.find("FileInfo").ok_or(XmlError::Missing("FileInfo"))?;
    Ok(FileSearchOpened {
        channel: f.num("channelId")?,
        handle: f.num("handle")?.ok_or(XmlError::Missing("handle"))?,
        record_types: split_types(f.text_of("recordType").unwrap_or_default()),
    })
}

/// One recording file from a 15 reply.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RecordFile {
    /// Channel number.
    pub channel: u8,
    /// `name`: an opaque identifier.
    pub name: String,
    /// `Id`: an opaque identifier (events refer to it as `fileId`).
    pub id: String,
    /// `streamType`, when present and known.
    pub stream: Option<StreamTier>,
    /// `containsAudio` is non-zero.
    pub contains_audio: bool,
    /// Recording causes (`recordType`).
    pub record_types: Vec<String>,
    /// File size in bytes (`sizeH` << 32 | `sizeL`).
    pub size: u64,
    /// Start of the recording.
    pub start: DateTime,
    /// End of the recording.
    pub end: DateTime,
}

/// Parses a 15 reply into files. A reply with no `FileInfo` is an empty
/// page. Entries that lack a channel or times are skipped.
pub fn parse_file_list(xml: &str) -> Result<Vec<RecordFile>, XmlError> {
    let root = Element::parse(xml)?;
    let mut out = Vec::new();
    for f in root.find_all("FileInfo") {
        let (Some(channel), Ok(start), Ok(end)) = (
            f.num::<u8>("channelId").ok().flatten(),
            f.time("startTime"),
            f.time("endTime"),
        ) else {
            continue;
        };
        let lo = f.num::<u64>("sizeL").ok().flatten().unwrap_or(0) & 0xffff_ffff;
        let hi = f.num::<u64>("sizeH").ok().flatten().unwrap_or(0) & 0xffff_ffff;
        out.push(RecordFile {
            channel,
            name: f.text_of("name").unwrap_or_default().to_owned(),
            id: f.text_of("Id").unwrap_or_default().to_owned(),
            stream: f.text_of("streamType").and_then(StreamTier::from_name),
            contains_audio: f.num::<u32>("containsAudio").ok().flatten().unwrap_or(0) != 0,
            record_types: split_types(f.text_of("recordType").unwrap_or_default()),
            size: hi << 32 | lo,
            start,
            end,
        });
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// 272, 273, 274: motion and detection events

/// An event search (command 272) of one channel and one day.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EventSearch {
    /// Channel number.
    pub channel: u8,
    /// The camera's device ID, when known.
    pub uid: Option<String>,
    /// The lens to search. Here `streamType` is a number: 0 was seen for the
    /// first lens's main stream and 2 for the second lens's `externStream`;
    /// the full mapping is unknown, so low tiers are sent as their lens's
    /// high tier (see [`event_stream_number`]).
    pub stream: StreamTier,
    /// Start of the window (00:00:00 of the day in the official client).
    pub start: DateTime,
    /// End of the window (23:59:59 of the same day).
    pub end: DateTime,
}

/// The numeric `streamType` of a 272 request for a tier: 0 for the first
/// lens, 2 for the second. Only those two values were observed (0 with
/// `mainStream`, 2 with `externStream`); sub and mobile tiers map to their
/// lens, since events belong to a camera, not to one of its streams.
pub const fn event_stream_number(tier: StreamTier) -> u32 {
    match tier {
        StreamTier::Main | StreamTier::Sub => 0,
        StreamTier::Extern | StreamTier::Mobile => 2,
    }
}

/// Extension that carries the channel of a 272, 273 or 274 request (whose
/// body travels as the payload).
pub fn event_search_extension(channel: u8) -> String {
    format!(
        "{XML_DECLARATION}\n<Extension version=\"1.1\">\n<channelId>{channel}</channelId>\n</Extension>\n"
    )
}

/// Payload of an event search open (command 272).
pub fn event_search_open_body(q: &EventSearch) -> String {
    let items: String = ALARM_TYPES.iter().map(|t| format!("<i>{t}</i>")).collect();
    wrap(&format!(
        "<findAlarmVideo version=\"1.1\">\n<channelId>{}</channelId>\n{}\
         <logicChnBitmap>255</logicChnBitmap>\n<streamType>{}</streamType>\n\
         <notSearchVideo>0</notSearchVideo>\n{}{}<alarmType>{}</alarmType>\n\
         <eventAlarmType>{items}</eventAlarmType>\n</findAlarmVideo>\n",
        q.channel,
        opt_uid(q.uid.as_deref()),
        event_stream_number(q.stream),
        q.start.xml("startTime"),
        q.end.xml("endTime"),
        ALARM_TYPES.join(", "),
    ))
}

/// Payload of an event search page fetch (273) or close (274).
pub fn event_search_handle_body(channel: u8, file_handle: u32) -> String {
    wrap(&format!(
        "<findAlarmVideo version=\"1.1\">\n<channelId>{channel}</channelId>\n\
         <fileHandle>{file_handle}</fileHandle>\n</findAlarmVideo>\n"
    ))
}

/// Parses a 272 reply: the `fileHandle` for 273 and 274.
pub fn parse_event_search_opened(xml: &str) -> Result<u32, XmlError> {
    let root = Element::parse(xml)?;
    let f = root
        .find("findAlarmVideo")
        .ok_or(XmlError::Missing("findAlarmVideo"))?;
    f.num("fileHandle")?.ok_or(XmlError::Missing("fileHandle"))
}

/// One event from a 273 page.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EventRecord {
    /// Channel number (from the page).
    pub channel: u8,
    /// Event type as the device names it (`md`, `people`, `vehicle`, ...).
    pub kind: String,
    /// `fileName`.
    pub file_name: String,
    /// `fileId`: the `Id` of the recording file that contains the event.
    pub file_id: String,
    /// `bHasRecFile` is non-zero.
    pub has_recording: bool,
    /// Start of the event.
    pub start: DateTime,
    /// End of the event.
    pub end: DateTime,
}

/// One page of a 273 reply.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EventPage {
    /// `bFinished` is non-zero: no more pages.
    pub finished: bool,
    /// Events on this page (up to 50 were seen).
    pub events: Vec<EventRecord>,
}

/// Parses a 273 reply. Events without times are skipped. A reply without
/// `bFinished` counts as finished, so a fetch loop always ends.
pub fn parse_event_page(xml: &str) -> Result<EventPage, XmlError> {
    let root = Element::parse(xml)?;
    let info = root
        .find("alarmVideoInfo")
        .ok_or(XmlError::Missing("alarmVideoInfo"))?;
    let channel: Option<u8> = info.num("channelId")?;
    let finished = info.num::<u32>("bFinished")?.is_none_or(|v| v != 0);
    let mut events = Vec::new();
    for v in info.find_all("alarmVideo") {
        let (Ok(start), Ok(end)) = (v.time("startTime"), v.time("endTime")) else {
            continue;
        };
        let Some(channel) = v.num::<u8>("channelId").ok().flatten().or(channel) else {
            continue;
        };
        events.push(EventRecord {
            channel,
            kind: v.text_of("alarmType").unwrap_or_default().to_owned(),
            file_name: v.text_of("fileName").unwrap_or_default().to_owned(),
            file_id: v.text_of("fileId").unwrap_or_default().to_owned(),
            has_recording: v.num::<u32>("bHasRecFile").ok().flatten().unwrap_or(1) != 0,
            start,
            end,
        });
    }
    Ok(EventPage { finished, events })
}

// ---------------------------------------------------------------------------
// 298: cover images

/// A cover-image request (command 298): one keyframe for a time range.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CoverRequest {
    /// Channel number.
    pub channel: u8,
    /// The camera's device ID, when known.
    pub uid: Option<String>,
    /// Stream (lens and tier) to take the picture from.
    pub stream: StreamTier,
    /// Start of the range.
    pub start: DateTime,
    /// End of the range.
    pub end: DateTime,
}

/// Body of a cover-image request (command 298).
pub fn cover_body(q: &CoverRequest) -> String {
    wrap(&format!(
        "<CoverPreview version=\"1.1\">\n<channelId>{}</channelId>\n{}\
         <logicChnBitmap>255</logicChnBitmap>\n<desc>0</desc>\n<streamType>{}</streamType>\n\
         {}{}<frameList>\n<frameNo>1</frameNo>\n</frameList>\n</CoverPreview>\n",
        q.channel,
        opt_uid(q.uid.as_deref()),
        q.stream.name(),
        q.start.xml("startTime"),
        q.end.xml("endTime"),
    ))
}

// ---------------------------------------------------------------------------
// 143, 144: download by time range

/// A download of one stream's time range (command 143).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DownloadRequest {
    /// Channel number.
    pub channel: u8,
    /// The camera's device ID: optional for a first lens, sent for a second.
    pub uid: Option<String>,
    /// `streamType` alone selects lens and tier.
    pub stream: StreamTier,
    /// Start of the window; the device starts at the keyframe at or before it.
    pub start: DateTime,
    /// End of the window.
    pub end: DateTime,
}

/// Body of a download request (command 143).
pub fn download_body(q: &DownloadRequest) -> String {
    wrap(&format!(
        "<FileInfoList version=\"1.1\">\n<FileInfo>\n{}<logicChnBitmap>255</logicChnBitmap>\n\
         <channelId>{}</channelId>\n<supportSub>0</supportSub>\n<streamType>{}</streamType>\n\
         {}{}</FileInfo>\n</FileInfoList>\n",
        opt_uid(q.uid.as_deref()),
        q.channel,
        q.stream.name(),
        q.start.xml("startTime"),
        q.end.xml("endTime"),
    ))
}

/// The device's estimate of a download's size in bytes, from the first 143
/// reply (`sizeH` << 32 | `sizeL`); `None` without `sizeL`.
pub fn parse_download_estimate(xml: &str) -> Result<Option<u64>, XmlError> {
    let root = Element::parse(xml)?;
    let Some(f) = root.find("FileInfo").or_else(|| root.find("FileInfoList")) else {
        return Ok(None);
    };
    let Some(lo) = f.num::<u64>("sizeL")? else {
        return Ok(None);
    };
    let hi = f.num::<u64>("sizeH")?.unwrap_or(0);
    Ok(Some((hi & 0xffff_ffff) << 32 | (lo & 0xffff_ffff)))
}

// ---------------------------------------------------------------------------
// 123, 5, 7: playback of a recording file (docs/protocol.md, "Playback")

/// The only `playSpeed` value observed in a decryptable capture: normal
/// speed. Other rates were seen only encrypted, so their values are not
/// known and are never sent (docs/MILESTONE-P.md, B1).
pub const PLAY_SPEED_NORMAL: u32 = 1;

/// The `channelId` the official client put in its 123 (seek), observed as 0
/// although the recording belonged to another channel; why is an open
/// question in docs/protocol.md. Sent as observed.
pub const SEEK_CHANNEL: u8 = 0;

/// A play of one recording file (command 5), stopped with command 7.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlayRequest {
    /// Channel number.
    pub channel: u8,
    /// The camera's device ID, when known.
    pub uid: Option<String>,
    /// Stream (lens and tier).
    pub stream: StreamTier,
    /// The file's `Id` from the file search (command 15).
    pub file_id: String,
    /// The file's `name` from the file search (the stop names it).
    pub file_name: String,
}

/// Body of a seek (command 123): where the next play starts, tagged with
/// the `seq` its media will carry.
pub fn replay_seek_body(seq: u32, at: DateTime) -> String {
    wrap(&format!(
        "<ReplaySeek version=\"1.1\">\n<channelId>{SEEK_CHANNEL}</channelId>\n<seq>{seq}</seq>\n\
         {}</ReplaySeek>\n",
        at.xml("seekTime"),
    ))
}

/// Body of a play (command 5) at normal speed.
pub fn play_body(q: &PlayRequest) -> String {
    wrap(&format!(
        "<FileInfoList version=\"1.1\">\n<FileInfo>\n<channelId>{}</channelId>\n<Id>{}</Id>\n{}\
         <supportSub>0</supportSub>\n<playSpeed>{PLAY_SPEED_NORMAL}</playSpeed>\n\
         <streamType>{}</streamType>\n</FileInfo>\n</FileInfoList>\n",
        q.channel,
        escape(&q.file_id),
        opt_uid(q.uid.as_deref()),
        q.stream.name(),
    ))
}

/// Body of a play's stop (command 7).
pub fn play_stop_body(q: &PlayRequest) -> String {
    wrap(&format!(
        "<FileInfoList version=\"1.1\">\n<FileInfo>\n<channelId>{}</channelId>\n<name>{}</name>\n\
         <streamType>{}</streamType>\n</FileInfo>\n</FileInfoList>\n",
        q.channel,
        escape(&q.file_name),
        q.stream.name(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::xml::find_text;

    fn t(y: u16, mo: u8, d: u8, h: u8, mi: u8, s: u8) -> DateTime {
        DateTime::new(y, mo, d, h, mi, s).unwrap()
    }

    #[test]
    fn date_time_arithmetic_and_validation() {
        assert_eq!(days_in_month(2024, 2), 29);
        assert_eq!(days_in_month(2026, 2), 28);
        assert_eq!(days_in_month(2000, 2), 29);
        assert_eq!(days_in_month(1900, 2), 28);
        assert!(DateTime::new(2026, 2, 29, 0, 0, 0).is_none());
        assert!(DateTime::new(2026, 13, 1, 0, 0, 0).is_none());
        assert!(DateTime::new(2026, 1, 1, 24, 0, 0).is_none());
        assert_eq!(t(1970, 1, 1, 0, 0, 0).to_naive_secs(), 0);
        assert_eq!(t(2000, 3, 1, 0, 0, 1).to_naive_secs(), 951_868_801);
        for secs in [0i64, 86_399, 951_868_801, 1_790_000_000, -1, -86_401] {
            let dt = DateTime::from_naive_secs(secs).unwrap();
            assert_eq!(dt.to_naive_secs(), secs, "{dt}");
        }
        let a = t(2026, 10, 9, 23, 59, 59);
        let b = DateTime::from_naive_secs(a.to_naive_secs() + 1).unwrap();
        assert_eq!(b, t(2026, 10, 10, 0, 0, 0));
        assert!(a < b);
        assert_eq!(a.to_string(), "2026-10-09 23:59:59");
        assert_eq!(a.start_of_day(), t(2026, 10, 9, 0, 0, 0));
        assert_eq!(b.end_of_day(), t(2026, 10, 10, 23, 59, 59));
    }

    #[test]
    fn element_tree() {
        let root =
            Element::parse("<body><a x=\"1\"><b> one </b><c/><b>two</b></a><d>&lt;3</d></body>")
                .unwrap();
        let body = root.child("body").unwrap();
        let a = body.child("a").unwrap();
        assert_eq!(a.text_of("b"), Some("one"));
        assert_eq!(a.children_named("b").count(), 2);
        assert_eq!(a.child("c").unwrap().text, "");
        assert_eq!(root.find("d").unwrap().text, "<3");
        assert_eq!(root.find_all("b").len(), 2);
        assert!(Element::parse("<a><b></a>").is_err());
        assert!(Element::parse("<a>").is_err());
        let deep = "<a>".repeat(MAX_DEPTH + 2) + &"</a>".repeat(MAX_DEPTH + 2);
        assert!(Element::parse(&deep).is_err());
    }

    #[test]
    fn day_records_round_trip() {
        let body = day_records_body(
            t(2026, 1, 1, 0, 0, 0),
            t(2026, 1, 31, 23, 59, 59),
            &[
                DayRecordChannel {
                    channel: 3,
                    uid: Some("UID-EXAMPLE".into()),
                },
                DayRecordChannel {
                    channel: 5,
                    uid: None,
                },
            ],
        );
        let root = Element::parse(&body).unwrap();
        let dr = root.find("DayRecords").unwrap();
        assert_eq!(dr.child("startTime").unwrap().text_of("month"), Some("1"));
        assert_eq!(dr.child("endTime").unwrap().text_of("day"), Some("31"));
        let recs = dr.find_all("DayRecord");
        assert_eq!(recs.len(), 2);
        assert_eq!(recs[0].text_of("uid"), Some("UID-EXAMPLE"));
        assert_eq!(recs[1].text_of("index"), Some("1"));
        assert_eq!(recs[1].text_of("uid"), None);

        let reply = "<body><DayRecords version=\"1.1\">\
            <startTime><year>2026</year><month>1</month><day>1</day><hour>0</hour><minute>0</minute><second>0</second></startTime>\
            <endTime><year>2026</year><month>1</month><day>31</day><hour>23</hour><minute>59</minute><second>59</second></endTime>\
            <DayRecordList><DayRecord><index>0</index><channelId>3</channelId>\
            <dayTypeList><dayType><index>0</index><type>normal</type></dayType>\
            <dayType><index>4</index><type>normal</type></dayType></dayTypeList></DayRecord>\
            <DayRecord><index>1</index><channelId>5</channelId></DayRecord></DayRecordList>\
            </DayRecords></body>";
        let got = parse_day_records(reply).unwrap();
        assert_eq!(got.len(), 2);
        assert_eq!(got[0].channel, 3);
        assert_eq!(
            got[0]
                .days
                .iter()
                .map(DayType::day_of_month)
                .collect::<Vec<_>>(),
            vec![1, 5]
        );
        assert!(got[0].days[0].has_recordings());
        assert!(got[1].days.is_empty());
    }

    #[test]
    fn file_search_bodies_and_replies() {
        let q = FileSearch {
            channel: 2,
            uid: Some("UID-EXAMPLE".into()),
            stream: StreamTier::Extern,
            start: t(2026, 10, 9, 0, 0, 0),
            end: t(2026, 10, 9, 23, 59, 59),
        };
        let body = file_search_open_body(&q);
        assert_eq!(
            find_text(&body, "streamType").unwrap().as_deref(),
            Some("externStream")
        );
        assert_eq!(
            find_text(&body, "searchAITrack").unwrap().as_deref(),
            Some("1")
        );
        assert_eq!(
            find_text(&body, "logicChnBitmap").unwrap().as_deref(),
            Some("255")
        );
        assert!(
            find_text(&body, "recordType")
                .unwrap()
                .unwrap()
                .contains("md, people")
        );
        let root = Element::parse(&body).unwrap();
        assert_eq!(root.find("fileRecordType").unwrap().children.len(), 10);
        let h = file_search_handle_body(2, Some("UID-EXAMPLE"), 7);
        assert_eq!(find_text(&h, "handle").unwrap().as_deref(), Some("7"));

        let opened = "<body><FileInfoList version=\"1.1\"><FileInfo><channelId>2</channelId>\
            <handle>0</handle><name></name><containsAudio>1</containsAudio>\
            <recordType>manual ,io ,md ,sched</recordType><sizeL>0</sizeL><sizeH>0</sizeH>\
            </FileInfo></FileInfoList></body>";
        let o = parse_file_search_opened(opened).unwrap();
        assert_eq!(o.handle, 0);
        assert_eq!(o.record_types, vec!["manual", "io", "md", "sched"]);
        assert_eq!(o.channel, Some(2));

        let list = "<body><FileInfoList version=\"1.1\">\
            <FileInfo><channelId>2</channelId><handle>0</handle><name>example-name-1</name>\
            <Id>example-id-1</Id><streamType>externStream</streamType><containsAudio>1</containsAudio>\
            <recordType>md</recordType><sizeL>4294967295</sizeL><sizeH>1</sizeH>\
            <startTime><year>2026</year><month>10</month><day>8</day><hour>23</hour><minute>40</minute><second>0</second></startTime>\
            <endTime><year>2026</year><month>10</month><day>9</day><hour>0</hour><minute>30</minute><second>5</second></endTime>\
            </FileInfo>\
            <FileInfo><channelId>2</channelId><name>broken</name></FileInfo>\
            </FileInfoList></body>";
        let files = parse_file_list(list).unwrap();
        assert_eq!(files.len(), 1);
        let f = &files[0];
        assert_eq!(f.id, "example-id-1");
        assert_eq!(f.size, (1u64 << 32) + 0xffff_ffff);
        assert_eq!(f.stream, Some(StreamTier::Extern));
        assert!(f.contains_audio);
        assert_eq!(f.start, t(2026, 10, 8, 23, 40, 0));
        assert_eq!(f.end, t(2026, 10, 9, 0, 30, 5));
        assert!(parse_file_list("<body/>").unwrap().is_empty());
    }

    #[test]
    fn event_search_bodies_and_replies() {
        let q = EventSearch {
            channel: 1,
            uid: None,
            stream: StreamTier::Mobile,
            start: t(2026, 10, 9, 0, 0, 0),
            end: t(2026, 10, 9, 23, 59, 59),
        };
        let body = event_search_open_body(&q);
        assert_eq!(
            find_text(&body, "streamType").unwrap().as_deref(),
            Some("2")
        );
        assert_eq!(
            find_text(&body, "notSearchVideo").unwrap().as_deref(),
            Some("0")
        );
        assert_eq!(find_text(&body, "uid").unwrap(), None);
        assert_eq!(event_stream_number(StreamTier::Main), 0);
        let ext = event_search_extension(1);
        assert_eq!(find_text(&ext, "channelId").unwrap().as_deref(), Some("1"));
        let h = event_search_handle_body(1, 9);
        assert_eq!(find_text(&h, "fileHandle").unwrap().as_deref(), Some("9"));

        let opened = "<body><findAlarmVideo version=\"1.1\"><channelId>1</channelId>\
                      <fileHandle>9</fileHandle></findAlarmVideo></body>";
        assert_eq!(parse_event_search_opened(opened).unwrap(), 9);
        assert!(parse_event_search_opened("<body/>").is_err());

        let page = "<body><alarmVideoInfo version=\"1.1\"><channelId>1</channelId><fileHandle>9</fileHandle>\
            <bFinished>0</bFinished><alarmVideoList>\
            <alarmVideo><fileName>example-file</fileName><bHasRecFile>1</bHasRecFile><bEncrypted>0</bEncrypted>\
            <alarmType>vehicle</alarmType><fileId>example-id-1</fileId>\
            <startTime><year>2026</year><month>10</month><day>9</day><hour>7</hour><minute>42</minute><second>5</second></startTime>\
            <endTime><year>2026</year><month>10</month><day>9</day><hour>7</hour><minute>42</minute><second>25</second></endTime>\
            </alarmVideo>\
            <alarmVideo><alarmType>md</alarmType></alarmVideo>\
            </alarmVideoList></alarmVideoInfo></body>";
        let p = parse_event_page(page).unwrap();
        assert!(!p.finished);
        assert_eq!(p.events.len(), 1);
        let e = &p.events[0];
        assert_eq!((e.channel, e.kind.as_str()), (1, "vehicle"));
        assert_eq!(e.file_id, "example-id-1");
        assert_eq!(e.start, t(2026, 10, 9, 7, 42, 5));
        assert_eq!(e.end, t(2026, 10, 9, 7, 42, 25));
        let last = "<body><alarmVideoInfo><channelId>1</channelId><bFinished>1</bFinished></alarmVideoInfo></body>";
        let p = parse_event_page(last).unwrap();
        assert!(p.finished && p.events.is_empty());
    }

    #[test]
    fn cover_and_download_bodies() {
        let q = CoverRequest {
            channel: 4,
            uid: Some("UID-EXAMPLE".into()),
            stream: StreamTier::Main,
            start: t(2026, 10, 9, 0, 0, 0),
            end: t(2026, 10, 9, 1, 0, 0),
        };
        let b = cover_body(&q);
        assert_eq!(find_text(&b, "desc").unwrap().as_deref(), Some("0"));
        assert_eq!(find_text(&b, "frameNo").unwrap().as_deref(), Some("1"));
        assert_eq!(
            find_text(&b, "streamType").unwrap().as_deref(),
            Some("mainStream")
        );

        let d = DownloadRequest {
            channel: 4,
            uid: None,
            stream: StreamTier::Sub,
            start: t(2026, 10, 9, 7, 42, 0),
            end: t(2026, 10, 9, 7, 43, 0),
        };
        let b = download_body(&d);
        let root = Element::parse(&b).unwrap();
        let fi = root.find("FileInfo").unwrap();
        assert_eq!(fi.text_of("supportSub"), Some("0"));
        assert_eq!(fi.text_of("streamType"), Some("subStream"));
        assert_eq!(fi.text_of("uid"), None);
        assert_eq!(fi.child("endTime").unwrap().text_of("minute"), Some("43"));

        let est = "<body><FileInfoList version=\"1.1\"><FileInfo><FileCount>1</FileCount>\
                   <sizeL>20000000</sizeL><sizeH>0</sizeH></FileInfo></FileInfoList></body>";
        assert_eq!(parse_download_estimate(est).unwrap(), Some(20_000_000));
        assert_eq!(parse_download_estimate("<body/>").unwrap(), None);
    }

    #[test]
    fn identifiers_are_escaped() {
        let b = file_search_handle_body(0, Some("a<b>&"), 1);
        assert_eq!(find_text(&b, "uid").unwrap().as_deref(), Some("a<b>&"));
    }
}
