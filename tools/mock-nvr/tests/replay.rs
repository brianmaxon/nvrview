//! Replay over TCP: a synthetic recording written with the recording writer
//! comes back record for record, at the recorded pace, accelerated, and
//! looped.

use bc_protocol::framer::{Decoder, RawMessage};
use bc_protocol::media::{Codec, Record};
use bc_protocol::recording::{Writer, encode_record};
use bc_protocol::session::{Event, Session};
use bc_protocol::stream::StreamTier;
use mock_nvr::fixture::Fixture;
use mock_nvr::replay::{Replay, ReplayConfig};
use mock_nvr::{Config, MockNvr, spawn};
use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::PathBuf;
use std::time::{Duration, Instant};

const SPACING_US: u64 = 20_000;
const VIDEOS: u64 = 25;

/// A made-up stream: an info record, then a picture every 20 ms with an
/// audio record after every third picture, a discontinuity in the middle.
/// Returns the recording and its records (with arrival times) in order.
fn synthetic() -> (Vec<u8>, Vec<(u64, Vec<u8>)>) {
    let mut w = Writer::new(Vec::new()).unwrap();
    let mut records = Vec::new();
    let mut put = |w: &mut Writer<Vec<u8>>, at: u64, r: Record| {
        w.record(at, &r).unwrap();
        records.push((at, encode_record(&r)));
    };
    put(
        &mut w,
        0,
        Record::Info {
            width: 160,
            height: 120,
            fps: 50,
        },
    );
    for i in 0..VIDEOS {
        let at = 1_000 + i * SPACING_US;
        put(
            &mut w,
            at,
            Record::Video {
                codec: Codec::H264,
                keyframe: i % 10 == 0,
                pts_us: 777_000 + i * SPACING_US,
                data: (0..(100 + i * 700)).map(|b| (b * 7 + i) as u8).collect(),
                extra_header: Vec::new(),
            },
        );
        if i % 3 == 2 {
            put(
                &mut w,
                at + 5_000,
                Record::Audio {
                    stream_index: 5,
                    data: vec![0xff, 0xf1, i as u8, 2, 3, 4, 5, 6, 7],
                },
            );
        }
        if i == 12 {
            w.discontinuity(at + 6_000).unwrap();
        }
    }
    (w.finish().unwrap(), records)
}

fn start(rep: Replay, speed: f64, looped: bool) -> MockNvr {
    let path =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/synthetic/h264-basic.h264");
    let mut cfg = Config::new(Fixture::load(&path).unwrap());
    let mut rc = ReplayConfig::new(rep);
    rc.speed = speed;
    rc.looped = looped;
    cfg.replay = Some(rc);
    spawn(cfg).unwrap()
}

struct Client {
    tcp: TcpStream,
    dec: Decoder,
    session: Session,
}

impl Client {
    fn next(&mut self) -> RawMessage {
        loop {
            if let Some(m) = self.dec.next_message().unwrap() {
                return m;
            }
            let mut buf = [0u8; 65536];
            let n = self.tcp.read(&mut buf).unwrap();
            assert!(n > 0, "connection closed");
            self.dec.push(&buf[..n]);
        }
    }
}

/// Logs in, starts a preview, and collects media bytes (with the time each
/// message arrived since the start request) until `want` bytes are in.
fn receive(nvr: &MockNvr, want: usize) -> Vec<(Duration, Vec<u8>)> {
    let tcp = TcpStream::connect(nvr.addr()).unwrap();
    tcp.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
    let mut c = Client {
        tcp,
        dec: Decoder::new(),
        session: Session::new("example-user", "example-password"),
    };
    let hs = c.session.handshake().unwrap();
    c.tcp.write_all(&hs).unwrap();
    let m = c.next();
    assert_eq!(c.session.handle(&m).unwrap(), Event::NonceReceived);
    let login = c.session.login().unwrap();
    c.tcp.write_all(&login).unwrap();
    let m = c.next();
    assert_eq!(c.session.handle(&m).unwrap(), Event::LoginOk);
    let (_, bytes) = c.session.start_preview(2, StreamTier::Main).unwrap();
    let t0 = Instant::now();
    c.tcp.write_all(&bytes).unwrap();
    let mut out = Vec::new();
    let mut got = 0;
    let deadline = Instant::now() + Duration::from_secs(20);
    while got < want {
        assert!(Instant::now() < deadline, "timed out");
        let m = c.next();
        if let Event::Media { data, .. } = c.session.handle(&m).unwrap() {
            got += data.len();
            out.push((t0.elapsed(), data));
        }
    }
    out
}

/// Splits received messages back into records using the expected lengths,
/// returning the time the last byte of each record arrived.
fn arrival_of_records(msgs: &[(Duration, Vec<u8>)], expected: &[(u64, Vec<u8>)]) -> Vec<Duration> {
    let all: Vec<u8> = msgs.iter().flat_map(|(_, d)| d.clone()).collect();
    let mut ends = Vec::new();
    let mut pos = 0;
    for (_, rec) in expected {
        assert_eq!(&all[pos..pos + rec.len()], &rec[..], "record bytes differ");
        pos += rec.len();
        let mut seen = 0;
        let t = msgs
            .iter()
            .find(|(_, d)| {
                seen += d.len();
                seen >= pos
            })
            .unwrap()
            .0;
        ends.push(t);
    }
    ends
}

fn total(expected: &[(u64, Vec<u8>)]) -> usize {
    expected.iter().map(|(_, r)| r.len()).sum()
}

#[test]
fn replays_byte_exact_at_the_recorded_pace() {
    let (bytes, expected) = synthetic();
    let nvr = start(Replay::from_reader(&bytes[..]).unwrap(), 1.0, false);
    let msgs = receive(&nvr, total(&expected));
    let arrived = arrival_of_records(&msgs, &expected);
    for ((at, _), got) in expected.iter().zip(&arrived) {
        let want = Duration::from_micros(*at);
        // Never early (the clock starts just before the server's); late only
        // by scheduling noise.
        assert!(
            *got + Duration::from_millis(15) >= want,
            "early: {got:?} < {want:?}"
        );
        assert!(
            *got < want + Duration::from_millis(250),
            "late: {got:?} > {want:?}"
        );
    }
    // The discontinuity is not on the wire: the pace is all that shows it.
    assert!(arrived.last().unwrap() >= &Duration::from_millis(450));
}

#[test]
fn accelerated_replay_keeps_order_and_timestamps() {
    let (bytes, expected) = synthetic();
    let nvr = start(Replay::from_reader(&bytes[..]).unwrap(), 10.0, false);
    let msgs = receive(&nvr, total(&expected));
    let arrived = arrival_of_records(&msgs, &expected);
    for ((at, _), got) in expected.iter().zip(&arrived) {
        let want = Duration::from_micros(*at / 10);
        assert!(
            *got + Duration::from_millis(15) >= want,
            "early: {got:?} < {want:?}"
        );
        assert!(
            *got < want + Duration::from_millis(250),
            "late: {got:?} > {want:?}"
        );
    }
    // 10x of a half-second recording: far sooner than real time.
    assert!(*arrived.last().unwrap() < Duration::from_millis(400));
}

#[test]
fn looping_starts_over_with_later_timestamps() {
    let (bytes, expected) = synthetic();
    let rep = Replay::from_reader(&bytes[..]).unwrap();
    let pass = rep.pass_us();
    let nvr = start(rep, 20.0, true);
    let want = total(&expected) * 2 + 1;
    let msgs = receive(&nvr, want);
    let all: Vec<u8> = msgs.iter().flat_map(|(_, d)| d.clone()).collect();
    let first_pass: Vec<u8> = expected.iter().flat_map(|(_, r)| r.clone()).collect();
    assert_eq!(&all[..first_pass.len()], &first_pass[..]);

    // The second pass: the same records, video timestamps one pass later.
    let mut pos = first_pass.len();
    for (_, rec) in &expected {
        let got = &all[pos..pos + rec.len()];
        pos += rec.len();
        if &rec[2..4] == b"dc" {
            let pts = |b: &[u8]| u64::from_le_bytes(b[16..24].try_into().unwrap());
            assert_eq!(pts(got), pts(rec) + pass);
            assert_eq!(got[..16], rec[..16]);
            assert_eq!(got[24..], rec[24..]);
        } else {
            assert_eq!(got, &rec[..]);
        }
    }
}

/// Two tiers on one clock, as `bc-cli capture` records them: sub pictures
/// every 100 ms (keyframe every 10), arriving 20 ms after their capture
/// time; main pictures every 50 ms (keyframe every 20), arriving 120 ms
/// after theirs. Both use one picture clock.
fn two_tiers() -> Vec<u8> {
    use bc_protocol::recording::TierWriter;
    let mut items: Vec<(u64, StreamTier, Record)> = vec![
        (
            1_000,
            StreamTier::Sub,
            Record::Info {
                width: 64,
                height: 48,
                fps: 10,
            },
        ),
        (
            2_000,
            StreamTier::Main,
            Record::Info {
                width: 640,
                height: 480,
                fps: 20,
            },
        ),
    ];
    let pic = |keyframe, pts_us| Record::Video {
        codec: Codec::H264,
        keyframe,
        pts_us,
        data: vec![0, 0, 0, 1, if keyframe { 0x65 } else { 0x41 }, 1, 2, 3],
        extra_header: Vec::new(),
    };
    for i in 0..80u64 {
        items.push((
            i * 100_000 + 20_000,
            StreamTier::Sub,
            pic(i % 10 == 0, i * 100_000),
        ));
    }
    for i in 0..160u64 {
        items.push((
            i * 50_000 + 120_000,
            StreamTier::Main,
            pic(i % 20 == 0, i * 50_000),
        ));
    }
    items.sort_by_key(|(at, t, _)| (*at, t.number()));
    let mut w = TierWriter::new(Vec::new()).unwrap();
    w.start_requested(StreamTier::Sub, 0).unwrap();
    w.start_requested(StreamTier::Main, 0).unwrap();
    for (at, tier, r) in &items {
        w.record(*tier, *at, r).unwrap();
    }
    w.finish().unwrap()
}

fn login(nvr: &MockNvr) -> Client {
    let tcp = TcpStream::connect(nvr.addr()).unwrap();
    tcp.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
    let mut c = Client {
        tcp,
        dec: Decoder::new(),
        session: Session::new("example-user", "example-password"),
    };
    let hs = c.session.handshake().unwrap();
    c.tcp.write_all(&hs).unwrap();
    let m = c.next();
    assert_eq!(c.session.handle(&m).unwrap(), Event::NonceReceived);
    let login = c.session.login().unwrap();
    c.tcp.write_all(&login).unwrap();
    let m = c.next();
    assert_eq!(c.session.handle(&m).unwrap(), Event::LoginOk);
    c
}

/// A client watching the sub-stream starts main: main joins at the same
/// capture moment, at its most recent keyframe, with the recorded latency
/// between the tiers.
#[test]
fn a_tier_started_later_joins_the_shared_clock() {
    use bc_protocol::media::Demuxer;
    const SPEED: f64 = 2.0;
    let rep = Replay::from_reader(&two_tiers()[..]).unwrap();
    assert_eq!(rep.version(), 2);
    let nvr = start(rep, SPEED, false);
    let mut c = login(&nvr);
    let (sub_id, bytes) = c.session.start_preview(0, StreamTier::Sub).unwrap();
    c.tcp.write_all(&bytes).unwrap();
    let t0 = Instant::now();
    let (mut sub_demux, mut main_demux) = (Demuxer::new(), Demuxer::new());
    // (arrival since t0, record) per tier.
    let (mut sub, mut main) = (Vec::new(), Vec::new());
    let mut main_id = None;
    let deadline = Instant::now() + Duration::from_secs(20);
    while main.len() < 40 {
        assert!(Instant::now() < deadline, "timed out");
        let m = c.next();
        let Event::Media { message_id, data } = c.session.handle(&m).unwrap() else {
            continue;
        };
        let (demux, out) = if message_id == sub_id {
            (&mut sub_demux, &mut sub)
        } else {
            assert_eq!(Some(message_id), main_id);
            (&mut main_demux, &mut main)
        };
        demux.push(&data);
        while let Some(r) = demux.next_record().unwrap() {
            out.push((t0.elapsed(), r));
        }
        // Start main once sub has shown the picture captured at 3.4 s
        // (replay time 3.42 s): main's latest keyframe then is the one
        // captured at 3.0 s (arrived at 3.12 s).
        let seen = sub
            .iter()
            .any(|(_, r)| matches!(r, Record::Video { pts_us, .. } if *pts_us >= 3_400_000));
        if main_id.is_none() && seen {
            let (id, bytes) = c.session.start_preview(0, StreamTier::Main).unwrap();
            c.tcp.write_all(&bytes).unwrap();
            main_id = Some(id);
        }
    }
    // Main begins with its own stream info, then that keyframe.
    assert!(matches!(main[0].1, Record::Info { width: 640, .. }));
    let pts = |r: &Record| match r {
        Record::Video {
            pts_us, keyframe, ..
        } => Some((*keyframe, *pts_us)),
        _ => None,
    };
    let videos: Vec<(Duration, bool, u64)> = main
        .iter()
        .filter_map(|(t, r)| pts(r).map(|(k, p)| (*t, k, p)))
        .collect();
    assert_eq!((videos[0].1, videos[0].2), (true, 3_000_000));
    // Pictures 3.0..3.4 s are due already: they come as a burst, then the
    // pace is the recorded one.
    let burst = videos.iter().filter(|(_, _, p)| *p <= 3_300_000).count();
    assert!(burst >= 6, "burst of {burst}");
    let burst_span = videos[burst - 1].0 - videos[0].0;
    assert!(burst_span < Duration::from_millis(60), "{burst_span:?}");
    // Same capture moment on both tiers: main arrives (120 - 20) ms / speed
    // after sub, as recorded.
    for (t, _, p) in videos.iter().filter(|(_, _, p)| *p >= 3_800_000) {
        let Some((ts, _)) = sub
            .iter()
            .find(|(_, r)| pts(r).is_some_and(|(_, q)| q == *p))
        else {
            continue;
        };
        let lag = t.as_secs_f64() - ts.as_secs_f64();
        let want = 0.100 / SPEED;
        assert!((lag - want).abs() < 0.03, "lag {lag} at pts {p}");
    }
}
