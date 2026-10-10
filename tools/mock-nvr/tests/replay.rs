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
