//! Test helpers: fixture loading, access unit splitting, and a fake decoder
//! that checks the action protocol while it runs.

#![allow(dead_code)]

use std::collections::VecDeque;
use std::path::PathBuf;

use video_pipeline::{
    AccessUnit, Action, Codec, DecodeFailureKind, PipelineConfig, Rect, Size, TilePipeline,
    TileStatus, analyze, nal_units,
};

/// 20 fps.
pub const FRAME_US: u64 = 50_000;

pub fn fixture(name: &str) -> Vec<u8> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/synthetic")
        .join(name);
    std::fs::read(&path).unwrap_or_else(|e| panic!("reading {}: {e}", path.display()))
}

/// Splits an elementary stream into access units (Annex-B, 4-byte start
/// codes). A new access unit starts at a non-VCL NAL unit that follows a
/// picture, or at the first slice of a new picture.
pub fn split_access_units(codec: Codec, data: &[u8]) -> Vec<Vec<u8>> {
    let mut aus: Vec<Vec<u8>> = Vec::new();
    let mut cur: Vec<u8> = Vec::new();
    let mut cur_has_vcl = false;
    for nal in nal_units(data) {
        let (is_vcl, first_slice) = match codec {
            Codec::H264 => {
                let t = nal[0] & 0x1F;
                let vcl = (1..=5).contains(&t);
                // first_mb_in_slice == 0 is the single bit '1'.
                (vcl, vcl && nal.get(1).is_some_and(|b| b & 0x80 != 0))
            }
            Codec::H265 => {
                let t = (nal[0] >> 1) & 0x3F;
                let vcl = t < 32;
                (vcl, vcl && nal.get(2).is_some_and(|b| b & 0x80 != 0))
            }
        };
        let starts_new = cur_has_vcl && (!is_vcl || first_slice);
        if starts_new {
            aus.push(std::mem::take(&mut cur));
            cur_has_vcl = false;
        }
        cur.extend_from_slice(&[0, 0, 0, 1]);
        cur.extend_from_slice(nal);
        cur_has_vcl |= is_vcl;
    }
    if !cur.is_empty() {
        aus.push(cur);
    }
    aus
}

pub fn load(name: &str) -> (Codec, Vec<Vec<u8>>) {
    let codec = if name.ends_with(".h265") {
        Codec::H265
    } else {
        Codec::H264
    };
    (codec, split_access_units(codec, &fixture(name)))
}

pub fn au(codec: Codec, pts: u64, data: Vec<u8>) -> AccessUnit {
    let keyframe = analyze(codec, &data).random_access.is_random_access();
    AccessUnit {
        codec,
        keyframe,
        pts,
        data,
    }
}

/// What the fake decoder saw, in order.
#[derive(Clone, Debug, PartialEq)]
pub enum Event {
    Configure {
        codec: Codec,
        coded: Size,
        visible: Rect,
        sets: usize,
    },
    Decode {
        pts: u64,
        keyframe: bool,
    },
    Flush,
    Reset,
    Clear,
    Status(TileStatus),
}

/// How the fake decoder answers a submitted picture.
pub enum Answer {
    Ok,
    Fail(DecodeFailureKind),
    /// Never answer.
    Silent,
}

/// Drives a pipeline like a platform would, asserting the action protocol:
/// no Decode without a configured decoder, the first Decode after a
/// Configure or Flush is a random access point (by NAL type), and bounded
/// pending actions.
pub struct Harness {
    pub p: TilePipeline,
    pub now: u64,
    pub events: Vec<(u64, Event)>,
    configured: bool,
    codec: Option<Codec>,
    need_keyframe: bool,
    pending: VecDeque<u64>,
    pub decide: Box<dyn FnMut(u64, usize) -> Answer>,
    /// Answers each `Configure` (argument: how many came before); `Some`
    /// fails it, reported right after the action like a platform would.
    pub configure: Box<dyn FnMut(usize) -> Option<DecodeFailureKind>>,
    session_ok: bool,
    configures_seen: usize,
    /// Check the fake decoder's own count of pictures in flight against the
    /// limit. Off when a test sends spurious results the fake cannot see.
    pub strict_in_flight: bool,
    submitted: usize,
}

impl Harness {
    pub fn new(config: PipelineConfig) -> Harness {
        Harness {
            p: TilePipeline::new(1, config),
            now: 0,
            events: Vec::new(),
            configured: false,
            codec: None,
            need_keyframe: true,
            pending: VecDeque::new(),
            decide: Box::new(|_, _| Answer::Ok),
            configure: Box::new(|_| None),
            session_ok: false,
            configures_seen: 0,
            strict_in_flight: true,
            submitted: 0,
        }
    }

    pub fn with_decoder(mut self, f: impl FnMut(u64, usize) -> Answer + 'static) -> Harness {
        self.decide = Box::new(f);
        self
    }

    /// Carries out every pending action, then answers submitted pictures,
    /// repeating until nothing changes.
    pub fn run(&mut self) {
        for _ in 0..10_000 {
            self.drain();
            let Some(pts) = self.pending.pop_front() else {
                return;
            };
            let n = self.submitted;
            match (self.decide)(pts, n) {
                Answer::Ok => self.p.decoded(pts),
                Answer::Fail(kind) => self.p.decode_failed(kind),
                Answer::Silent => {
                    self.pending.push_front(pts);
                    self.drain();
                    return;
                }
            }
        }
        panic!("pipeline did not settle");
    }

    pub fn drain(&mut self) {
        let mut count = 0;
        while let Some(a) = self.p.next_action() {
            let mut configure_failure = None;
            count += 1;
            assert!(count < 10_000, "action loop does not end");
            let ev = match a {
                Action::Configure {
                    codec,
                    parameter_sets,
                    coded_size,
                    visible_rect,
                    color: _,
                } => {
                    assert!(!parameter_sets.is_empty());
                    assert_eq!(parameter_sets.codec(), codec);
                    self.configured = true;
                    configure_failure = (self.configure)(self.configures_seen);
                    self.configures_seen += 1;
                    self.session_ok = configure_failure.is_none();
                    self.codec = Some(codec);
                    self.need_keyframe = true;
                    self.pending.clear();
                    Event::Configure {
                        codec,
                        coded: coded_size,
                        visible: visible_rect,
                        sets: parameter_sets.len(),
                    }
                }
                Action::Decode {
                    access_unit,
                    keyframe,
                    pts,
                } => {
                    assert!(self.configured, "Decode without a configured decoder");
                    assert!(self.session_ok, "Decode after a failed Configure");
                    let codec = self.codec.expect("codec while decoding");
                    let info = analyze(codec, access_unit);
                    assert!(info.has_picture);
                    assert_eq!(keyframe, info.random_access.is_random_access());
                    if self.need_keyframe {
                        assert!(
                            keyframe,
                            "first Decode after Configure/Flush must be a keyframe"
                        );
                        self.need_keyframe = false;
                    }
                    self.pending.push_back(pts);
                    self.submitted += 1;
                    assert!(
                        !self.strict_in_flight
                            || self.pending.len() <= self.p.config().max_in_flight as usize
                    );
                    Event::Decode { pts, keyframe }
                }
                Action::Flush => {
                    assert!(self.configured, "Flush without a decoder");
                    assert!(self.session_ok, "Flush after a failed Configure");
                    self.need_keyframe = true;
                    self.pending.clear();
                    Event::Flush
                }
                Action::Reset => {
                    assert!(self.configured, "Reset without a decoder");
                    self.configured = false;
                    self.pending.clear();
                    Event::Reset
                }
                Action::Clear => Event::Clear,
                Action::Status(s) => {
                    assert_eq!(s, self.p.status());
                    Event::Status(s)
                }
            };
            self.events.push((self.now, ev));
            if let Some(kind) = configure_failure {
                self.p.decode_failed(kind);
            }
        }
        assert!(self.p.queued_units() <= self.p.config().max_queued_units as usize);
        assert!(self.p.in_flight() <= self.p.config().max_in_flight as usize);
    }

    /// One frame period: tick, push, and run the decoder.
    pub fn feed(&mut self, au: AccessUnit) {
        self.p.tick(self.now);
        self.p.push(au);
        self.run();
        self.now += FRAME_US;
    }

    pub fn advance(&mut self, us: u64) {
        self.now += us;
        self.p.tick(self.now);
        self.run();
    }

    pub fn count(&self, f: impl Fn(&Event) -> bool) -> usize {
        self.events.iter().filter(|(_, e)| f(e)).count()
    }

    pub fn configures(&self) -> Vec<(Size, Rect)> {
        self.events
            .iter()
            .filter_map(|(_, e)| match e {
                Event::Configure { coded, visible, .. } => Some((*coded, *visible)),
                _ => None,
            })
            .collect()
    }

    /// Largest number of matching events within any window of `span` µs.
    pub fn max_in_window(&self, span: u64, f: impl Fn(&Event) -> bool) -> usize {
        let times: Vec<u64> = self
            .events
            .iter()
            .filter(|(_, e)| f(e))
            .map(|(t, _)| *t)
            .collect();
        let mut best = 0;
        for (i, &t) in times.iter().enumerate() {
            let n = times[i..].iter().take_while(|&&u| u < t + span).count();
            best = best.max(n);
        }
        best
    }
}
