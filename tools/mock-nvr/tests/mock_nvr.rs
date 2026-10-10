//! End-to-end tests: the real `bc-protocol` client session against the mock.

mod common;

use bc_protocol::framer::{Decoder, RawMessage};
use bc_protocol::media::{Codec, Demuxer, Record};
use bc_protocol::session::{Command, Event, Session};
use bc_protocol::stream::StreamTier;
use common::{Link, TestNvr};
use mock_nvr::Config;
use mock_nvr::fixture::Fixture;
use std::io::{Read, Write};
use std::path::PathBuf;
use std::time::Duration;

const USER: &str = "example-user";
const PASSWORD: &str = "example-password";

fn fixture_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/synthetic")
        .join(name)
}

fn start(tweak: impl FnOnce(&mut Config)) -> TestNvr {
    let fix = Fixture::load(&fixture_path("h264-basic.h264")).unwrap();
    let mut cfg = Config::new(fix);
    cfg.channels = 3;
    tweak(&mut cfg);
    TestNvr::sim(cfg)
}

struct Client {
    tcp: Link,
    dec: Decoder,
    session: Session,
}

impl Client {
    fn connect(nvr: &TestNvr, user: &str, password: &str) -> Client {
        let tcp = nvr.connect();
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

/// Over real TCP, in real time: the TCP server itself.
#[test]
fn login_ok_pushes_channel_list_and_answers_queries() {
    let fix = Fixture::load(&fixture_path("h264-basic.h264")).unwrap();
    let mut cfg = Config::new(fix);
    cfg.channels = 3;
    let nvr = TestNvr::tcp(cfg);
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
    let deadline = c.tcp.now() + Duration::from_secs(15);
    while out
        .iter()
        .filter(|r| matches!(r, Record::Video { .. }))
        .count()
        < n
    {
        assert!(c.tcp.now() < deadline, "timed out waiting for video");
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
                ..
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
    let deadline = c.tcp.now() + Duration::from_secs(10);
    loop {
        assert!(c.tcp.now() < deadline, "no stop reply");
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
    let deadline = nvr.now() + Duration::from_secs(5);
    while nvr.active_logins() != 1 {
        assert!(nvr.now() < deadline, "slot was not released");
        nvr.sleep(Duration::from_millis(20));
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
        ("fisheye/h264-fisheye.h264", Codec::H264, 20, 1, 0),
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
    let started = c.tcp.now();
    // 2 s of stream.
    let recs = read_until_video(&mut c, id, &mut demux, 21);
    let elapsed = c.tcp.now() - started;

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

/// Waits for the reply to command `cmd` with message ID `id`, skipping
/// pushes; panics if media arrives meanwhile.
fn reply_status(c: &mut Client, cmd: u32, id: u32) -> u16 {
    loop {
        let m = c.next();
        let h = m.header;
        if h.command == cmd && h.message_id == id {
            assert!(m.body.is_empty(), "expected an empty reply");
            return h.status;
        }
        assert_ne!(h.command, 3, "unexpected preview message");
    }
}

#[test]
fn third_party_channel_answers_400_then_streams_on_that_message_id() {
    let nvr = start(|c| {
        c.fps = 100;
        let mut tp = mock_nvr::ThirdPartyConfig::new(vec![2], 1);
        tp.media_delay = Duration::from_millis(100);
        c.third_party = Some(tp);
    });
    let mut c = Client::connect(&nvr, USER, PASSWORD);
    assert_eq!(c.login(), Event::LoginOk);

    // The first start: 400, and nothing follows it.
    let (id, bytes) = c.session.start_preview(2, StreamTier::Sub).unwrap();
    c.tcp.write_all(&bytes).unwrap();
    assert_eq!(reply_status(&mut c, 3, id), 400);
    c.tcp
        .set_read_timeout(Some(Duration::from_millis(400)))
        .unwrap();
    let mut buf = [0u8; 4096];
    assert!(c.tcp.read(&mut buf).is_err(), "nothing after the first 400");
    c.tcp
        .set_read_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    let stop = c.session.stop_preview(id).unwrap();
    c.tcp.write_all(&stop).unwrap();
    assert_eq!(reply_status(&mut c, 4, id + 1), 200);

    // The second: 400 again, then the stream on the same message ID.
    let (id, bytes) = c.session.start_preview(2, StreamTier::Sub).unwrap();
    c.tcp.write_all(&bytes).unwrap();
    assert_eq!(reply_status(&mut c, 3, id), 400);
    let mut raw = Vec::new();
    let mut demux = Demuxer::new();
    let mut videos = Vec::new();
    while videos.len() < 5 {
        if let Event::Media { message_id, data } = c.event() {
            assert_eq!(message_id, id);
            raw.extend_from_slice(&data);
            demux.push(&data);
            while let Some(r) = demux.next_record().unwrap() {
                if let Record::Video { keyframe, .. } = r {
                    videos.push(keyframe);
                }
            }
        }
    }
    assert!(videos[0], "starts at a keyframe");
    // After the 32-byte info record: a third-party keyframe magic.
    assert_eq!(&raw[32..36], b":0dc");
    assert!(raw.windows(4).any(|w| w == b":1dc"));
    assert!(!raw.windows(4).any(|w| w == b"00dc" || w == b"01dc"));
}

#[test]
fn doorbell_talk_back_round_trip() {
    use bc_protocol::talk::{
        AdpcmEncoder, SAMPLES_PER_RECORD, parse_talk_ability, parse_talk_channels,
    };
    use mock_nvr::talk::TalkEnd;
    let nvr = start(|c| c.talk_channels = vec![2]);
    let mut c = Client::connect(&nvr, USER, PASSWORD);
    assert_eq!(c.login(), Event::LoginOk);
    // 199 marks only the doorbell's channel.
    let (_, q) = c.session.query_info(Command::Support, None).unwrap();
    c.tcp.write_all(&q).unwrap();
    let Event::Control { xml, .. } = c.control() else {
        panic!()
    };
    assert_eq!(parse_talk_channels(&xml).unwrap(), vec![2]);
    // 10: the doorbell answers its ability, another channel 400.
    let (_, q) = c.session.talk_ability(2).unwrap();
    c.tcp.write_all(&q).unwrap();
    let Event::Control { status, xml, .. } = c.control() else {
        panic!()
    };
    assert_eq!(status, 200);
    let cfg = parse_talk_ability(&xml).unwrap().choose().unwrap();
    let (_, q) = c.session.talk_ability(1).unwrap();
    c.tcp.write_all(&q).unwrap();
    assert!(matches!(c.control(), Event::Control { status: 400, .. }));

    // A talk on a channel without a speaker is refused.
    c.session.set_motion_allowed(1, true);
    let (_, q) = c.session.talk_open(1, &cfg).unwrap();
    c.tcp.write_all(&q).unwrap();
    assert!(matches!(
        c.control(),
        Event::Control {
            command: 201,
            status: 400,
            ..
        }
    ));
    c.session.forget_talk(1);

    // The doorbell: open, three records, close.
    c.session.set_motion_allowed(2, true);
    let (oid, q) = c.session.talk_open(2, &cfg).unwrap();
    c.tcp.write_all(&q).unwrap();
    match c.control() {
        Event::Control {
            command: 201,
            message_id,
            status: 200,
            ..
        } => assert_eq!(message_id, oid),
        e => panic!("{e:?}"),
    }
    let mut enc = AdpcmEncoder::new();
    let sent: Vec<i16> = (0..SAMPLES_PER_RECORD * 3)
        .map(|i| ((i as f64 * 0.2).sin() * 6000.0) as i16)
        .collect();
    let mut expected = Vec::new();
    for block in sent.as_chunks::<SAMPLES_PER_RECORD>().0 {
        let rec = enc.encode_record(block);
        expected.extend(bc_protocol::talk::decode_record(&rec).unwrap().samples);
        c.tcp
            .write_all(&c.session.talk_audio(2, &rec).unwrap())
            .unwrap();
        match c.control() {
            Event::Control {
                command: 202,
                message_id: 0,
                status: 200,
                ..
            } => {}
            e => panic!("{e:?}"),
        }
    }
    let (_, q) = c.session.talk_close(2).unwrap();
    c.tcp.write_all(&q).unwrap();
    assert!(matches!(
        c.control(),
        Event::Control {
            command: 11,
            status: 200,
            ..
        }
    ));
    let log = nvr.talk_log();
    assert_eq!(log.len(), 1);
    assert_eq!((log[0].channel, log[0].end), (2, TalkEnd::Closed));
    assert_eq!(log[0].records.len(), 3);
    assert!(log[0].records.iter().all(|r| r.stream_index == 1));
    assert!(log[0].continuous());
    assert_eq!(log[0].samples().unwrap(), expected);

    // An open talk that the connection drops is logged as such.
    let (_, q) = c.session.talk_open(2, &cfg).unwrap();
    c.tcp.write_all(&q).unwrap();
    assert!(matches!(
        c.control(),
        Event::Control {
            command: 201,
            status: 200,
            ..
        }
    ));
    drop(c);
    let t0 = nvr.now();
    while nvr
        .talk_log()
        .last()
        .is_some_and(|t| t.end == TalkEnd::Open)
    {
        assert!(nvr.now() - t0 < Duration::from_secs(5));
        nvr.sleep(Duration::from_millis(10));
    }
    assert_eq!(nvr.talk_log()[1].end, TalkEnd::Disconnected);
}

#[test]
fn fisheye_channel_is_flagged_and_streams_its_own_square_fixture() {
    let fisheye = Fixture::load(&fixture_path("fisheye/h264-fisheye.h264")).unwrap();
    let first = fisheye.frames()[0].data.clone();
    let nvr = start(|c| {
        c.fps = 100;
        c.fisheye = Some(mock_nvr::FisheyeConfig::new(vec![2], fisheye, 384));
    });
    let mut c = Client::connect(&nvr, USER, PASSWORD);
    assert_eq!(c.login(), Event::LoginOk);

    let (_, q) = c.session.query_info(Command::Support, None).unwrap();
    c.tcp.write_all(&q).unwrap();
    let xml = match c.control() {
        Event::Control {
            command: 199, xml, ..
        } => xml,
        e => panic!("{e:?}"),
    };
    let ai = bc_protocol::detection::parse_channel_ai(&xml).unwrap();
    assert_eq!(
        ai.iter()
            .map(|a| (a.channel, a.fisheye))
            .collect::<Vec<_>>(),
        vec![(0, false), (1, false), (2, true)]
    );
    assert_eq!(
        xml.matches("<channelType>fisheye_ipc</channelType>")
            .count(),
        1
    );

    let (_, q) = c.session.query_info(Command::StreamInfoList, None).unwrap();
    c.tcp.write_all(&q).unwrap();
    match c.control() {
        Event::Control {
            command: 146, xml, ..
        } => {
            // The ordinary channels in one entry, the fisheye camera in its own.
            assert!(xml.contains("<channelBits>3</channelBits>"), "{xml}");
            assert!(xml.contains("<channelBits>4</channelBits>"), "{xml}");
            assert!(
                xml.contains("<width>384</width>\n<height>384</height>"),
                "{xml}"
            );
        }
        e => panic!("{e:?}"),
    }

    let (id, bytes) = c.session.start_preview(2, StreamTier::Main).unwrap();
    c.tcp.write_all(&bytes).unwrap();
    let mut demux = Demuxer::new();
    let recs = read_until_video(&mut c, id, &mut demux, 1);
    assert!(matches!(
        recs[0],
        Record::Info {
            width: 384,
            height: 384,
            ..
        }
    ));
    let video = recs.iter().find_map(|r| match r {
        Record::Video { data, .. } => Some(data.clone()),
        _ => None,
    });
    assert_eq!(video.as_deref(), Some(&first[..]));
    drop(c);
    nvr.shutdown();
}
