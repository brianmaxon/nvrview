//! End-to-end tests: the real `bc-protocol` client session against the mock.

use bc_protocol::framer::{Decoder, RawMessage};
use bc_protocol::media::{Codec, Demuxer, Record};
use bc_protocol::session::{Command, Event, Session};
use bc_protocol::stream::StreamTier;
use mock_nvr::fixture::Fixture;
use mock_nvr::{Config, MockNvr, spawn};
use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::PathBuf;
use std::time::{Duration, Instant};

const USER: &str = "example-user";
const PASSWORD: &str = "example-password";

fn fixture_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/synthetic")
        .join(name)
}

fn start(tweak: impl FnOnce(&mut Config)) -> MockNvr {
    let fix = Fixture::load(&fixture_path("h264-basic.h264")).unwrap();
    let mut cfg = Config::new(fix);
    cfg.channels = 3;
    tweak(&mut cfg);
    spawn(cfg).unwrap()
}

struct Client {
    tcp: TcpStream,
    dec: Decoder,
    session: Session,
}

impl Client {
    fn connect(nvr: &MockNvr, user: &str, password: &str) -> Client {
        let tcp = TcpStream::connect(nvr.addr()).unwrap();
        tcp.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
        Client {
            tcp,
            dec: Decoder::new(),
            session: Session::new(user, password),
        }
    }

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

    fn event(&mut self) -> Event {
        let m = self.next();
        self.session.handle(&m).unwrap()
    }

    /// Handshake and login; returns the login outcome event.
    fn login(&mut self) -> Event {
        let hs = self.session.handshake().unwrap();
        self.tcp.write_all(&hs).unwrap();
        assert_eq!(self.event(), Event::NonceReceived);
        let login = self.session.login().unwrap();
        self.tcp.write_all(&login).unwrap();
        self.event()
    }

    /// Next event that is not the unsolicited channel list push.
    fn control(&mut self) -> Event {
        loop {
            match self.event() {
                Event::Control { command: 145, .. } => {}
                e => return e,
            }
        }
    }
}

#[test]
fn login_ok_pushes_channel_list_and_answers_queries() {
    let nvr = start(|_| {});
    let mut c = Client::connect(&nvr, USER, PASSWORD);
    assert_eq!(c.login(), Event::LoginOk);
    match c.event() {
        Event::Control {
            command: 145, xml, ..
        } => assert_eq!(xml.matches("<ChannelInfo>").count(), 3),
        e => panic!("{e:?}"),
    }
    assert_eq!(nvr.active_logins(), 1);

    let ka = c.session.keepalive().unwrap();
    c.tcp.write_all(&ka).unwrap();
    match c.control() {
        Event::Control {
            command: 93,
            status: 200,
            xml,
            ..
        } => assert!(xml.contains("<type>LAN</type>")),
        e => panic!("{e:?}"),
    }
    let (_, q) = c.session.query_info(Command::StreamInfoList, None).unwrap();
    c.tcp.write_all(&q).unwrap();
    match c.control() {
        Event::Control {
            command: 146,
            status,
            xml,
            ..
        } => {
            // Status 300 with a full body, as real devices answer; one entry
            // whose bitmask covers all 3 channels.
            assert_eq!(status, 300);
            assert_eq!(xml.matches("<StreamInfo>").count(), 1);
            assert!(xml.contains("<channelBits>7</channelBits>"));
        }
        e => panic!("{e:?}"),
    }
    let (_, q) = c
        .session
        .query_info(Command::ChannelVersionInfo, Some(2))
        .unwrap();
    c.tcp.write_all(&q).unwrap();
    match c.control() {
        Event::Control {
            command: 318,
            status: 200,
            xml,
            ..
        } => assert!(xml.contains("<channelId>2</channelId>")),
        e => panic!("{e:?}"),
    }
    let (_, q) = c
        .session
        .query_info(Command::ChannelVersionInfo, Some(3))
        .unwrap();
    c.tcp.write_all(&q).unwrap();
    assert!(matches!(
        c.control(),
        Event::Control {
            command: 318,
            status: 400,
            ..
        }
    ));
}

#[test]
fn bad_credentials_get_401_and_lock_out() {
    let nvr = start(|c| c.max_failed_logins = 2);
    let mut c = Client::connect(&nvr, USER, "wrong");
    assert_eq!(
        c.login(),
        Event::LoginRejected {
            status: 401,
            remain_times: Some(1),
            unlock_secs: None,
        }
    );
    let mut c = Client::connect(&nvr, "someone-else", PASSWORD);
    assert_eq!(
        c.login(),
        Event::LoginRejected {
            status: 401,
            remain_times: Some(0),
            unlock_secs: None,
        }
    );
    // Locked out: even correct credentials are refused now.
    let mut c = Client::connect(&nvr, USER, PASSWORD);
    assert!(matches!(
        c.login(),
        Event::LoginRejected {
            status: 401,
            remain_times: Some(0),
            unlock_secs: None,
        }
    ));
    assert_eq!(nvr.active_logins(), 0);
}

fn read_until_video(c: &mut Client, id: u32, demux: &mut Demuxer, n: usize) -> Vec<Record> {
    let mut out = Vec::new();
    let deadline = Instant::now() + Duration::from_secs(15);
    while out
        .iter()
        .filter(|r| matches!(r, Record::Video { .. }))
        .count()
        < n
    {
        assert!(Instant::now() < deadline, "timed out waiting for video");
        if let Event::Media { message_id, data } = c.event() {
            assert_eq!(message_id, id);
            demux.push(&data);
            while let Some(r) = demux.next_record().unwrap() {
                out.push(r);
            }
        }
    }
    out
}

#[test]
fn preview_delivers_fixture_from_first_keyframe_then_stops() {
    let fix = Fixture::load(&fixture_path("h264-leading-p.h264")).unwrap();
    let first_key = fix.first_keyframe();
    assert!(first_key > 0, "this fixture starts with undecodable frames");
    let expected = fix.frames()[first_key].data.clone();
    let key_data: Vec<Vec<u8>> = fix.frames()[first_key..]
        .iter()
        .filter(|f| f.keyframe)
        .map(|f| f.data.clone())
        .collect();
    let nvr = start(|c| {
        c.fixture = std::sync::Arc::new(fix);
        c.fps = 100;
    });
    let mut c = Client::connect(&nvr, USER, PASSWORD);
    assert_eq!(c.login(), Event::LoginOk);

    let (id, bytes) = c.session.start_preview(1, StreamTier::Sub).unwrap();
    c.tcp.write_all(&bytes).unwrap();

    // No separate reply: media (status 200) answers the start directly.
    let mut demux = Demuxer::new();
    // 45 frames: past one loop of the 60-frame file's tail from the keyframe.
    let recs = read_until_video(&mut c, id, &mut demux, 45);
    assert!(matches!(
        recs[0],
        Record::Info {
            width: 320,
            height: 240,
            fps: 100
        }
    ));
    let videos: Vec<_> = recs
        .iter()
        .filter_map(|r| match r {
            Record::Video {
                codec,
                keyframe,
                pts_us,
                data,
            } => Some((*codec, *keyframe, *pts_us, data.clone())),
            _ => None,
        })
        .collect();
    // The very first video record is the fixture's first keyframe, intact.
    assert_eq!(videos[0], (Codec::H264, true, 0, expected.clone()));
    // Timestamps rise by one frame period; the stream loops back to the key.
    for (i, v) in videos.iter().enumerate() {
        assert_eq!(v.2, i as u64 * 10_000);
    }
    let keys: Vec<usize> = (0..videos.len()).filter(|&i| videos[i].1).collect();
    assert!(keys.len() >= 2, "expected the loop to repeat a keyframe");
    for (k, want) in keys.iter().zip(&key_data) {
        assert_eq!(&videos[*k].3, want);
    }

    // Stop: the reply arrives, and nothing media-like follows it.
    let stop = c.session.stop_preview(id).unwrap();
    c.tcp.write_all(&stop).unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        assert!(Instant::now() < deadline, "no stop reply");
        let m = c.next();
        if m.header.command == 4 {
            assert_eq!(m.header.status, 200);
            break;
        }
    }
    c.tcp
        .set_read_timeout(Some(Duration::from_millis(400)))
        .unwrap();
    let mut buf = [0u8; 4096];
    match c.tcp.read(&mut buf) {
        Err(e) => assert!(matches!(
            e.kind(),
            std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
        )),
        Ok(n) => panic!("{n} bytes arrived after the stop reply"),
    }
}

#[test]
fn login_limit_and_release() {
    let nvr = start(|c| c.max_logins = 2);
    let mut a = Client::connect(&nvr, USER, PASSWORD);
    let mut b = Client::connect(&nvr, USER, PASSWORD);
    assert_eq!(a.login(), Event::LoginOk);
    assert_eq!(b.login(), Event::LoginOk);
    assert_eq!(nvr.active_logins(), 2);
    let mut c = Client::connect(&nvr, USER, PASSWORD);
    assert!(matches!(
        c.login(),
        Event::LoginRejected { status: 400, .. }
    ));
    assert_eq!(nvr.active_logins(), 2);
    // Closing a session frees its slot.
    drop(a);
    let deadline = Instant::now() + Duration::from_secs(5);
    while nvr.active_logins() != 1 {
        assert!(Instant::now() < deadline, "slot was not released");
        std::thread::sleep(Duration::from_millis(20));
    }
    let mut d = Client::connect(&nvr, USER, PASSWORD);
    assert_eq!(d.login(), Event::LoginOk);
    drop((b, d));
    nvr.shutdown();
}

#[test]
fn bundled_fixtures_split_into_expected_pictures() {
    for (name, codec, frames, keys, first_key) in [
        ("h264-basic.h264", Codec::H264, 60, 3, 0),
        ("h265-basic.h265", Codec::H265, 60, 3, 0),
        ("h264-leading-p.h264", Codec::H264, 59, 2, 19),
        ("h265-leading-p.h265", Codec::H265, 59, 2, 19),
    ] {
        let f = Fixture::load(&fixture_path(name)).unwrap();
        assert_eq!(f.codec(), codec, "{name}");
        assert_eq!(f.frames().len(), frames, "{name}");
        assert_eq!(
            f.frames().iter().filter(|x| x.keyframe).count(),
            keys,
            "{name}"
        );
        assert_eq!(f.first_keyframe(), first_key, "{name}");
    }
}

#[test]
fn audio_records_are_interleaved_like_a_device() {
    use mock_nvr::audio::{AudioConfig, AudioFixture};
    let afix = AudioFixture::load(&fixture_path("aac-clicks.aac")).unwrap();
    let frames = afix.frames().to_vec();
    let nvr = start(|c| {
        c.fps = 10;
        let mut a = AudioConfig::new(afix);
        a.clock_hz = 16_013.0;
        a.jitter_us = 20_000;
        c.audio = Some(std::sync::Arc::new(a));
    });
    let mut c = Client::connect(&nvr, USER, PASSWORD);
    assert_eq!(c.login(), Event::LoginOk);
    let (id, bytes) = c.session.start_preview(0, StreamTier::Sub).unwrap();
    c.tcp.write_all(&bytes).unwrap();
    let mut demux = Demuxer::new();
    let started = Instant::now();
    // 2 s of stream.
    let recs = read_until_video(&mut c, id, &mut demux, 21);
    let elapsed = started.elapsed();

    let mut last_pts: Option<u64> = None;
    let mut k = 0u64;
    for r in &recs {
        match r {
            Record::Video { pts_us, .. } => last_pts = Some(*pts_us),
            Record::Audio { stream_index, data } => {
                assert_eq!(*stream_index, 5);
                assert_eq!(data, &frames[k as usize % frames.len()]);
                // Sent once its last sample exists: after the picture
                // captured at or before then, before the next one.
                let send_us = (k + 1) as f64 * 1024.0 / 16_013.0 * 1e6;
                let p = last_pts.expect("a picture comes first") as f64;
                assert!(p <= send_us && send_us < p + 100_000.0, "record {k}");
                k += 1;
            }
            Record::Info { .. } => {}
        }
    }
    // 2 s of video time hold about 31 records; paced in real time.
    assert!((30..=32).contains(&k), "{k} audio records");
    assert!(elapsed >= Duration::from_millis(1900), "{elapsed:?}");
}
