//! The fake recording store, driven by the real `bc-protocol` client session.

use bc_protocol::archive::{
    self, CoverRequest, DateTime, DayRecordChannel, DownloadRequest, EventSearch, FileSearch,
};
use bc_protocol::framer::{Decoder, RawMessage};
use bc_protocol::media::{Demuxer, Record};
use bc_protocol::session::{Event, Session};
use bc_protocol::stream::StreamTier;
use mock_nvr::archive::{ArchiveConfig, DownloadFault, FaultKind, MockEvent};
use mock_nvr::fixture::Fixture;
use mock_nvr::{Config, MockNvr, spawn};
use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::PathBuf;
use std::time::Duration;

fn d(day: u8, h: u8, m: u8, s: u8) -> DateTime {
    DateTime::new(2026, 10, day, h, m, s).unwrap()
}

fn start(tweak: impl FnOnce(&mut ArchiveConfig)) -> MockNvr {
    let path =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/synthetic/h264-basic.h264");
    let mut cfg = Config::new(Fixture::load(&path).unwrap());
    cfg.channels = 3;
    let mut a = ArchiveConfig::new(vec![d(1, 0, 0, 0), d(8, 0, 0, 0), d(9, 0, 0, 0)]);
    tweak(&mut a);
    cfg.archive = Some(a);
    spawn(cfg).unwrap()
}

struct Client {
    tcp: TcpStream,
    dec: Decoder,
    s: Session,
}

impl Client {
    fn login(nvr: &MockNvr) -> Client {
        let tcp = TcpStream::connect(nvr.addr()).unwrap();
        tcp.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
        let mut c = Client {
            tcp,
            dec: Decoder::new(),
            s: Session::new("example-user", "example-password"),
        };
        let hs = c.s.handshake().unwrap();
        c.send(&hs);
        assert_eq!(c.event(), Event::NonceReceived);
        let l = c.s.login().unwrap();
        c.send(&l);
        assert_eq!(c.event(), Event::LoginOk);
        c
    }

    fn send(&mut self, b: &[u8]) {
        self.tcp.write_all(b).unwrap();
    }

    fn next(&mut self) -> Option<RawMessage> {
        loop {
            if let Some(m) = self.dec.next_message().unwrap() {
                return Some(m);
            }
            let mut buf = [0u8; 65536];
            match self.tcp.read(&mut buf) {
                Ok(0) | Err(_) => return None,
                Ok(n) => self.dec.push(&buf[..n]),
            }
        }
    }

    fn event(&mut self) -> Event {
        let m = self.next().expect("connection closed");
        self.s.handle(&m).unwrap()
    }

    /// The reply to `id` (skipping pushes): status and XML.
    fn reply(&mut self, id: u32) -> (u32, u16, String) {
        loop {
            if let Event::Control {
                command,
                message_id,
                status,
                xml,
                ..
            } = self.event()
                && message_id == id
            {
                return (command, status, xml);
            }
        }
    }
}

#[test]
fn searches_list_days_files_and_paged_events() {
    let nvr = start(|a| {
        a.file_page = 10;
        a.event_page = 4;
        a.events_per_day = 9;
        a.events.push(MockEvent {
            channel: 1,
            kind: "vehicle".into(),
            start: d(9, 7, 42, 5),
            end: d(9, 7, 42, 25),
        });
    });
    let mut c = Client::login(&nvr);

    let (id, b) =
        c.s.day_records(
            d(1, 0, 0, 0),
            d(31, 23, 59, 59),
            &[DayRecordChannel {
                channel: 1,
                uid: None,
            }],
        )
        .unwrap();
    c.send(&b);
    let (_, status, xml) = c.reply(id);
    assert_eq!(status, 200);
    let days = archive::parse_day_records(&xml).unwrap();
    assert_eq!(
        days[0]
            .days
            .iter()
            .map(|t| t.day_of_month())
            .collect::<Vec<_>>(),
        vec![1, 8, 9]
    );

    let q = FileSearch {
        channel: 1,
        uid: None,
        stream: StreamTier::Main,
        start: d(9, 0, 0, 0),
        end: d(9, 23, 59, 59),
    };
    let (id, b) = c.s.file_search_open(&q).unwrap();
    c.send(&b);
    let (_, _, xml) = c.reply(id);
    let handle = archive::parse_file_search_opened(&xml).unwrap().handle;
    let mut files = Vec::new();
    loop {
        let b = c.s.file_search_step(id, 1, None, handle, false).unwrap();
        c.send(&b);
        let (cmd, status, xml) = c.reply(id);
        assert_eq!((cmd, status), (15, 200));
        let page = archive::parse_file_list(&xml).unwrap();
        if page.is_empty() {
            break;
        }
        files.extend(page);
    }
    assert_eq!(files.len(), 24);
    let b = c.s.file_search_step(id, 1, None, handle, true).unwrap();
    c.send(&b);
    assert_eq!(c.reply(id).1, 200);

    let (id, b) =
        c.s.event_search_open(&EventSearch {
            channel: 1,
            uid: None,
            stream: StreamTier::Main,
            start: d(9, 0, 0, 0),
            end: d(9, 23, 59, 59),
        })
        .unwrap();
    c.send(&b);
    let (_, _, xml) = c.reply(id);
    let fh = archive::parse_event_search_opened(&xml).unwrap();
    let mut events = Vec::new();
    let mut pages = 0;
    loop {
        let b = c.s.event_search_step(id, 1, fh, false).unwrap();
        c.send(&b);
        let page = archive::parse_event_page(&c.reply(id).2).unwrap();
        pages += 1;
        events.extend(page.events);
        if page.finished {
            break;
        }
    }
    assert_eq!((events.len(), pages), (10, 3));
    let v = events
        .iter()
        .find(|e| e.kind == "vehicle" && e.start == d(9, 7, 42, 5))
        .unwrap();
    assert!(files.iter().any(|f| f.id == v.file_id));
    let b = c.s.event_search_step(id, 1, fh, true).unwrap();
    c.send(&b);
    assert_eq!(c.reply(id).1, 200);
}

fn collect_download(c: &mut Client, id: u32) -> (Option<u64>, Vec<Record>, Option<u16>) {
    let mut demux = Demuxer::new();
    let mut recs = Vec::new();
    let mut estimate = None;
    while let Some(m) = c.next() {
        match c.s.handle(&m).unwrap() {
            Event::Control {
                command: 143, xml, ..
            } => estimate = archive::parse_download_estimate(&xml).unwrap(),
            Event::Media { message_id, data } if message_id == id => {
                demux.push(&data);
                while let Some(r) = demux.next_record().unwrap() {
                    recs.push(r);
                }
            }
            Event::TransferEnd { status, .. } => return (estimate, recs, Some(status)),
            _ => {}
        }
    }
    (estimate, recs, None)
}

fn request(ch: u8, a: DateTime, b: DateTime) -> DownloadRequest {
    DownloadRequest {
        channel: ch,
        uid: None,
        stream: StreamTier::Main,
        start: a,
        end: b,
    }
}

#[test]
fn download_cover_and_faults() {
    let nvr = start(|_| {});
    let mut c = Client::login(&nvr);

    let (id, b) =
        c.s.start_download(&request(2, d(9, 7, 42, 0), d(9, 7, 42, 59)))
            .unwrap();
    c.send(&b);
    let (est, recs, end) = collect_download(&mut c, id);
    assert_eq!(end, Some(300));
    assert!(est.unwrap() > 0);
    let video: Vec<_> = recs
        .iter()
        .filter_map(|r| match r {
            Record::Video {
                keyframe, pts_us, ..
            } => Some((*keyframe, *pts_us)),
            _ => None,
        })
        .collect();
    assert_eq!(video.len(), 600);
    assert!(video[0].0);
    assert_eq!(nvr.download_log().len(), 1);

    // A window without recordings ends normally with no video.
    let (id, b) =
        c.s.start_download(&request(2, d(10, 1, 0, 0), d(10, 1, 1, 0)))
            .unwrap();
    c.send(&b);
    let (_, recs, end) = collect_download(&mut c, id);
    assert_eq!(end, Some(300));
    assert!(recs.is_empty());

    // Cover image: ack, stream info and one keyframe, status-300 end.
    let (id, b) =
        c.s.cover(&CoverRequest {
            channel: 0,
            uid: None,
            stream: StreamTier::Main,
            start: d(9, 0, 0, 0),
            end: d(9, 6, 0, 0),
        })
        .unwrap();
    c.send(&b);
    let mut demux = Demuxer::new();
    let mut got = Vec::new();
    loop {
        match c.event() {
            Event::Media { data, .. } => {
                demux.push(&data);
                while let Some(r) = demux.next_record().unwrap() {
                    got.push(r);
                }
            }
            Event::TransferEnd {
                status, message_id, ..
            } if message_id == id => {
                assert_eq!(status, 300);
                break;
            }
            _ => {}
        }
    }
    assert!(matches!(got[0], Record::Info { .. }));
    assert!(matches!(got[1], Record::Video { keyframe: true, .. }));
    assert_eq!(got.len(), 2);

    // A stalled download: data stops; 144 stops it and is answered.
    nvr.inject_download_fault(DownloadFault {
        channel: Some(1),
        after_bytes: 20_000,
        kind: FaultKind::Stall,
        skip: 0,
    });
    let (id, b) =
        c.s.start_download(&request(1, d(9, 7, 0, 0), d(9, 7, 0, 59)))
            .unwrap();
    c.send(&b);
    c.tcp
        .set_read_timeout(Some(Duration::from_millis(500)))
        .unwrap();
    let (_, recs, end) = collect_download(&mut c, id);
    assert_eq!(end, None, "stalled");
    assert!(!recs.is_empty());
    c.tcp
        .set_read_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    let stop = c.s.stop_download(id).unwrap();
    c.send(&stop);
    assert_eq!(c.reply(id), (144, 200, String::new()));
    assert_eq!(nvr.command_count(144), 1);

    // A disconnect midway closes the connection.
    nvr.inject_download_fault(DownloadFault {
        channel: None,
        after_bytes: 10_000,
        kind: FaultKind::Disconnect,
        skip: 0,
    });
    let (id, b) =
        c.s.start_download(&request(0, d(9, 8, 0, 0), d(9, 8, 0, 59)))
            .unwrap();
    c.send(&b);
    let (_, _, end) = collect_download(&mut c, id);
    assert_eq!(end, None);
}
