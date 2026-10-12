//! The timeline, the policy, and the mixer together on simulated clocks:
//! where the user's words land in a call recording (docs/CAPTURE.md, "The
//! user's side": within +-40 ms), across a camera restart. No real time
//! passes; every timestamp is computed.

use capture::caps::Limits;
use capture::epoch::{Timeline, TimelineConfig};
use capture::mixer::{CallMixer, FRAME, MixedFrame, MixerConfig, OUT_RATE, Side};
use capture::policy::{
    Codec, PolicyConfig, SegmentPolicy, VideoDecision, VideoFormat, VideoSample,
};

const FMT: VideoFormat = VideoFormat {
    codec: Codec::H264,
    width: 1920,
    height: 1080,
    param_hash: 1,
};

/// Host arrival jitter of frame `i`: mostly none, some late (median 0).
fn jitter(i: i64) -> i64 {
    [0, 0, 20_000, 0, 30_000][(i % 5) as usize]
}

struct Call {
    timeline: Timeline,
    policy: SegmentPolicy,
    mixer: CallMixer,
    origin: Option<i64>,
    frames: Vec<MixedFrame>,
}

impl Call {
    fn new() -> Call {
        Call {
            timeline: Timeline::new(TimelineConfig::default()),
            policy: SegmentPolicy::new(PolicyConfig::new(Limits::call()), 0),
            mixer: CallMixer::new(MixerConfig::new(16_000, 16_000)),
            origin: None,
            frames: Vec::new(),
        }
    }

    fn video(&mut self, cam_ts: u64, host_us: i64, key: bool) {
        let pos = self.timeline.push_video(cam_ts, host_us);
        let d = self.policy.on_video(VideoSample {
            file_us: pos.file_us,
            host_us,
            keyframe: key,
            format: FMT,
            total_bytes: 0,
            frame_bytes: 1000,
        });
        if let VideoDecision::Start { origin_us, .. } = d {
            self.origin = Some(origin_us);
        }
        // Drive the mixer from the recording's progress.
        if let Some(o) = self.origin {
            self.mixer.advance(pos.file_us - o, 64, &mut self.frames);
        }
    }

    /// We sent a record at `host_us` (what the talk sender observed).
    fn sent(&mut self, host_us: i64, pcm: &[f32]) {
        let file = self.timeline.sent_file_us(host_us).expect("video seen");
        let rel = self.policy.audio_rel_us(file).expect("segment open");
        self.mixer.push(Side::You, rel, pcm);
    }

    fn finish(&mut self) {
        let end = self.timeline.last_file_us() - self.origin.unwrap();
        while self.mixer.flush(end, 64, &mut self.frames) == 64 {}
    }

    fn you_peak_us(&self) -> i64 {
        let mut best = (0.0f32, 0usize);
        for f in &self.frames {
            for (i, s) in f.you.iter().enumerate() {
                if s.abs() > best.0 {
                    best = (s.abs(), f.start_sample as usize + i);
                }
            }
        }
        assert!(best.0 > 0.3, "no sound found in the You track");
        (best.1 as i64) * 1_000_000 / i64::from(OUT_RATE)
    }
}

fn impulse_record() -> Vec<f32> {
    let mut v = vec![0.0f32; 1024];
    v[0] = 0.5;
    v
}

#[test]
fn your_words_land_where_the_camera_received_them() {
    let mut c = Call::new();
    // Camera clock starts near the wrap; host is file time + 700 ms.
    let base = (1u64 << 32) - 2_000_000;
    let host0 = 5_000_000i64;
    for i in 0..250i64 {
        c.video(
            base + (i as u64) * 40_000,
            host0 + i * 40_000 + jitter(i),
            i % 50 == 0,
        );
        // At frame 100 (file 4.0 s) the user's record goes out.
        if i == 100 {
            c.sent(host0 + 100 * 40_000, &impulse_record());
        }
    }
    c.finish();
    let got = c.you_peak_us();
    // The record went out at the host instant that matches file 4.0 s.
    let want = 4_000_000;
    assert!((got - want).abs() <= 40_000, "got {got} want {want}");
}

#[test]
fn placement_survives_a_camera_restart_and_a_long_outage() {
    let mut c = Call::new();
    let host0 = 1_000_000i64;
    for i in 0..100i64 {
        c.video((i as u64) * 40_000, host0 + i * 40_000 + jitter(i), i == 0);
    }
    // The camera restarts its clock after 8 s of silence on the host.
    let host1 = host0 + 99 * 40_000 + 8_000_000;
    for i in 0..200i64 {
        c.video(
            500 + (i as u64) * 40_000,
            host1 + i * 40_000 + jitter(i),
            i == 0,
        );
        // A record sent at the host instant of the restarted video's frame 50.
        if i == 50 {
            c.sent(host1 + 50 * 40_000, &impulse_record());
        }
    }
    // The gap was counted and the file continued (8 s < 60 s).
    assert_eq!(c.policy.stats().gaps, 1);
    assert_eq!(c.policy.segment(), 1);
    for i in 200..230i64 {
        c.video(
            500 + (i as u64) * 40_000,
            host1 + i * 40_000 + jitter(i),
            false,
        );
    }
    c.finish();
    // File time of that frame: 99 frames before + 8 s gap + 50 frames.
    let want = 99 * 40_000 + 8_000_000 + 50 * 40_000;
    let got = c.you_peak_us();
    assert!((got - want).abs() <= 40_000, "got {got} want {want}");
}

#[test]
fn frames_are_contiguous_and_cover_the_recording() {
    let mut c = Call::new();
    for i in 0..500i64 {
        c.video((i as u64) * 40_000, 1_000_000 + i * 40_000, i == 0);
    }
    c.finish();
    for (n, f) in c.frames.iter().enumerate() {
        assert_eq!(f.start_sample, (n * FRAME) as i64);
        assert_eq!(f.conversation.len(), FRAME);
    }
    let covered = c.frames.len() as i64 * FRAME as i64 * 1_000_000 / i64::from(OUT_RATE);
    let dur = 499 * 40_000;
    assert!((covered - dur).abs() < 22_000, "{covered} vs {dur}");
}
