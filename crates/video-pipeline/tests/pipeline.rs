//! TilePipeline against the synthetic fixtures and a fake decoder.

mod common;

use common::*;
use video_pipeline::{
    AccessUnit, Codec, DecodeFailure, ParameterSets, PipelineConfig, Rect, Size, TileStatus,
    VideoFormat,
};

const CODECS: [(&str, Codec); 2] = [("h264", Codec::H264), ("h265", Codec::H265)];

fn name(stem: &str, codec: Codec) -> String {
    match codec {
        Codec::H264 => format!("h264-{stem}.h264"),
        Codec::H265 => format!("h265-{stem}.h265"),
    }
}

fn feed_all(h: &mut Harness, codec: Codec, aus: &[Vec<u8>], first_pts: u64) {
    for (i, data) in aus.iter().enumerate() {
        let pts = first_pts + i as u64 * FRAME_US;
        h.feed(au(codec, pts, data.clone()));
    }
}

#[test]
fn fixtures_split_into_expected_access_units() {
    for (_, codec) in CODECS {
        let (_, basic) = load(&name("basic", codec));
        assert_eq!(basic.len(), 60);
        let (_, resize) = load(&name("resize", codec));
        assert_eq!(resize.len(), 80);
        let (_, leading) = load(&name("leading-p", codec));
        assert_eq!(leading.len(), 59);
    }
}

#[test]
fn parameter_sets_from_fixtures() {
    for (_, codec) in CODECS {
        let (_, aus) = load(&name("resize", codec));
        let first =
            VideoFormat::from_parameter_sets(ParameterSets::from_access_unit(codec, &aus[0]))
                .unwrap();
        assert_eq!(first.visible_rect, Rect::new(0, 0, 320, 240));
        assert_eq!(first.coded_size, Size::new(320, 240));
        let second =
            VideoFormat::from_parameter_sets(ParameterSets::from_access_unit(codec, &aus[40]))
                .unwrap();
        assert_eq!(second.visible_rect, Rect::new(0, 0, 480, 270));
        // Both encoders pad 270 lines to whole blocks and crop.
        assert_eq!(second.coded_size, Size::new(480, 272));
        let kinds = match codec {
            Codec::H264 => 2,
            Codec::H265 => 3,
        };
        assert_eq!(first.parameter_sets.len(), kinds);
        // No VUI color description: BT.601 video range below 720 lines.
        assert_eq!(first.color.matrix, video_pipeline::ColorMatrix::Bt601);
        assert!(!first.color.full_range);
    }
}

#[test]
fn plays_a_clean_stream() {
    for (_, codec) in CODECS {
        let (_, aus) = load(&name("basic", codec));
        let mut h = Harness::new(PipelineConfig::default());
        feed_all(&mut h, codec, &aus, 0);
        let s = h.p.stats();
        assert_eq!(s.received, 60);
        assert_eq!(s.decoded, 60);
        assert_eq!(s.gated + s.dropped + s.failures + s.resets, 0);
        assert_eq!(s.configures, 1);
        assert_eq!(s.keyframe_flag_mismatches, 0);
        assert_eq!(h.p.status(), TileStatus::Playing);
        assert!(matches!(h.events[0].1, Event::Configure { .. }));
        assert_eq!(
            h.events[1].1,
            Event::Decode {
                pts: 0,
                keyframe: true
            }
        );
        assert_eq!(
            h.count(|e| matches!(e, Event::Decode { keyframe: true, .. })),
            3
        );
        assert_eq!(h.count(|e| *e == Event::Status(TileStatus::Playing)), 1);
    }
}

#[test]
fn gates_until_the_first_keyframe() {
    for (_, codec) in CODECS {
        let (_, aus) = load(&name("leading-p", codec));
        let mut h = Harness::new(PipelineConfig::default());
        feed_all(&mut h, codec, &aus, 0);
        let s = h.p.stats();
        assert_eq!(s.gated, 19, "{codec:?}");
        assert_eq!(s.decoded, aus.len() as u64 - 19);
        assert!(matches!(h.events[0].1, Event::Configure { .. }));
        assert_eq!(
            h.events[1].1,
            Event::Decode {
                pts: 19 * FRAME_US,
                keyframe: true
            }
        );
    }
}

#[test]
fn format_change_resets_and_configures_before_the_keyframe() {
    for (_, codec) in CODECS {
        let (_, aus) = load(&name("resize", codec));
        let mut h = Harness::new(PipelineConfig::default());
        feed_all(&mut h, codec, &aus, 0);
        let s = h.p.stats();
        assert_eq!(s.decoded, 80);
        assert_eq!(s.format_changes, 1);
        assert_eq!(s.configures, 2);
        assert_eq!(s.resets, 0, "a format change is not a recovery reset");
        let cfg = h.configures();
        assert_eq!(cfg[0].1, Rect::new(0, 0, 320, 240));
        assert_eq!(cfg[1].1, Rect::new(0, 0, 480, 270));
        // Reset, Configure, then the keyframe at frame 40.
        let i = h
            .events
            .iter()
            .position(|(_, e)| *e == Event::Reset)
            .unwrap();
        assert!(matches!(h.events[i + 1].1, Event::Configure { .. }));
        assert_eq!(
            h.events[i + 2].1,
            Event::Decode {
                pts: 40 * FRAME_US,
                keyframe: true
            }
        );
        assert_eq!(h.p.format().unwrap().visible_rect.height, 270);
    }
}

#[test]
fn intermittent_failures_flush_without_resets() {
    for (_, codec) in CODECS {
        let (_, aus) = load(&name("basic", codec));
        // Every 7th submitted picture fails.
        let mut h = Harness::new(PipelineConfig::default()).with_decoder(|_, n| {
            if n % 7 == 6 {
                Answer::Fail(DecodeFailure::Corrupt)
            } else {
                Answer::Ok
            }
        });
        for round in 0..5u64 {
            feed_all(&mut h, codec, &aus, round * 60 * FRAME_US);
        }
        let s = h.p.stats();
        assert!(s.failures > 0);
        assert_eq!(s.resets, 0);
        assert_eq!(s.flushes, s.failures);
        assert!(s.decoded > 0);
        assert_ne!(h.p.status(), TileStatus::Undecodable);
    }
}

#[test]
fn persistent_failures_escalate_boundedly_then_give_up() {
    for (_, codec) in CODECS {
        let (_, aus) = load(&name("basic", codec));
        let config = PipelineConfig::default();
        let mut h = Harness::new(config).with_decoder(|_, _| Answer::Fail(DecodeFailure::Corrupt));
        // Ten simulated minutes of the same stream.
        let rounds = 10 * 60 * 20 / 60;
        for round in 0..rounds as u64 {
            feed_all(&mut h, codec, &aus, round * 60 * FRAME_US);
        }
        let s = h.p.stats();
        let window = config.window_us;
        assert!(s.resets <= u64::from(config.max_resets_per_window));
        // Recovery resets plus one release when giving up.
        assert!(
            h.max_in_window(window, |e| *e == Event::Reset)
                <= config.max_resets_per_window as usize + 1
        );
        assert!(
            h.max_in_window(window, |e| matches!(e, Event::Configure { .. }))
                <= config.max_configures_per_window as usize
        );
        assert_eq!(h.p.status(), TileStatus::Undecodable);
        // Initial decoder plus one per recovery reset; nothing after giving up.
        assert_eq!(s.configures, 1 + s.resets);
        assert!(s.failures <= 20, "failures {}", s.failures);
        // Cooldowns grow: gaps between recovery resets increase.
        let resets: Vec<u64> = h
            .events
            .iter()
            .filter(|(_, e)| *e == Event::Reset)
            .map(|(t, _)| *t)
            .collect();
        assert!(resets.len() >= 3);
        let gaps: Vec<u64> = resets.windows(2).map(|w| w[1] - w[0]).collect();
        assert!(gaps.windows(2).all(|g| g[1] >= g[0]), "{gaps:?}");

        // A thousand more failure reports with nothing in flight are ignored.
        let actions_before = h.events.len();
        for _ in 0..1000 {
            h.p.decode_failed(DecodeFailure::Corrupt);
            h.drain();
        }
        assert_eq!(h.events.len(), actions_before);
        assert_eq!(h.p.stats().stale_results, 1000);

        // A new format revives the tile.
        h.decide = Box::new(|_, _| Answer::Ok);
        let (_, resize) = load(&name("resize", codec));
        feed_all(&mut h, codec, &resize[40..], 1_000_000_000_000);
        assert_eq!(h.p.status(), TileStatus::Playing);
        assert_eq!(h.p.stats().decoded, 40);
    }
}

#[test]
fn every_failure_kind_is_bounded() {
    for kind in [
        DecodeFailure::Corrupt,
        DecodeFailure::DecoderLost,
        DecodeFailure::Unsupported,
    ] {
        let (_, aus) = load("h264-basic.h264");
        let config = PipelineConfig::default();
        let mut h = Harness::new(config).with_decoder(move |_, _| Answer::Fail(kind));
        for round in 0..200u64 {
            feed_all(&mut h, Codec::H264, &aus, round * 60 * FRAME_US);
        }
        let s = h.p.stats();
        assert!(
            s.resets <= u64::from(config.max_resets_per_window),
            "{kind:?}"
        );
        assert!(s.configures <= 1 + u64::from(config.max_resets_per_window));
        assert_eq!(h.p.status(), TileStatus::Undecodable);
        if kind == DecodeFailure::Unsupported {
            assert_eq!(s.failures, 1);
            assert_eq!(s.resets, 0);
        }
    }
}

#[test]
fn overflow_drops_to_the_next_keyframe() {
    for (_, codec) in CODECS {
        let (_, aus) = load(&name("basic", codec));
        let config = PipelineConfig {
            stall_timeout_us: 0,
            ..PipelineConfig::default()
        };
        // A decoder that answers nothing: four in flight, then the queue fills.
        let mut h = Harness::new(config).with_decoder(|_, _| Answer::Silent);
        for (i, data) in aus.iter().enumerate() {
            h.p.push(au(codec, i as u64, data.clone()));
            h.drain();
        }
        let s = h.p.stats();
        assert_eq!(h.p.in_flight(), 4);
        assert!(h.p.queued_units() <= 8);
        assert!(s.dropped > 0);
        assert_eq!(s.received, 60);
        assert_eq!(
            s.dropped + s.gated + h.p.queued_units() as u64 + 4,
            60,
            "every access unit is accounted for"
        );
        // Anything still queued starts with a keyframe.
        assert_eq!(h.count(|e| matches!(e, Event::Decode { .. })), 4);

        // Once the decoder catches up, decoding resumes at a keyframe.
        h.decide = Box::new(|_, _| Answer::Ok);
        h.run();
        assert!(h.p.stats().decoded >= 4);
    }
}

#[test]
fn slow_decoder_stays_bounded() {
    let (_, aus) = load("h265-basic.h265");
    let mut h = Harness::new(PipelineConfig::default());
    let mut pts = 0;
    for _ in 0..20 {
        for data in &aus {
            h.p.tick(h.now);
            h.p.push(au(Codec::H265, pts, data.clone()));
            h.drain();
            // The decoder catches up only every 16th frame period, more than
            // the in-flight limit plus the queue can absorb.
            if pts % 16 == 0 {
                h.run();
            }
            pts += 1;
            h.now += FRAME_US;
        }
    }
    let s = h.p.stats();
    assert!(s.dropped > 0);
    assert!(s.decoded > 0);
    assert_eq!(s.failures, 0);
}

#[test]
fn stalled_decoder_is_reset_after_timeout() {
    let (_, aus) = load("h264-basic.h264");
    let config = PipelineConfig::default();
    let mut h = Harness::new(config).with_decoder(|_, _| Answer::Silent);
    h.feed(au(Codec::H264, 0, aus[0].clone()));
    assert_eq!(h.p.in_flight(), 1);
    // feed() already advanced one frame period.
    h.advance(config.stall_timeout_us - FRAME_US - 1);
    assert_eq!(h.p.stats().stalls, 0);
    h.advance(1);
    let s = h.p.stats();
    assert_eq!(s.stalls, 1);
    assert_eq!(s.resets, 1);
    assert_eq!(h.p.status(), TileStatus::Recovering);
    assert_eq!(
        h.events.last().unwrap().1,
        Event::Status(TileStatus::Recovering)
    );
    assert!(h.events.iter().any(|(_, e)| *e == Event::Reset));
}

#[test]
fn stale_picture_is_cleared() {
    let (_, aus) = load("h264-basic.h264");
    let config = PipelineConfig::default();
    let mut h = Harness::new(config);
    feed_all(&mut h, Codec::H264, &aus[..10], 0);
    assert_eq!(h.p.status(), TileStatus::Playing);
    h.advance(config.clear_after_us);
    assert_eq!(h.p.stats().clears, 1);
    assert_eq!(h.p.status(), TileStatus::Waiting);
    assert!(h.events.iter().any(|(_, e)| *e == Event::Clear));
    h.advance(config.clear_after_us);
    assert_eq!(h.p.stats().clears, 1, "cleared once");
}

#[test]
fn discontinuity_flushes_and_regates() {
    let (_, aus) = load("h264-basic.h264");
    let mut h = Harness::new(PipelineConfig::default());
    feed_all(&mut h, Codec::H264, &aus[..10], 0);
    h.p.discontinuity();
    h.run();
    assert_eq!(
        h.events.last().unwrap().1,
        Event::Status(TileStatus::Waiting)
    );
    assert!(h.events.iter().any(|(_, e)| *e == Event::Flush));
    // P-frames are gated until the keyframe at 20.
    feed_all(&mut h, Codec::H264, &aus[10..30], 10 * FRAME_US);
    assert_eq!(h.p.stats().gated, 10);
    assert_eq!(h.p.stats().configures, 1, "same format, decoder kept");
    // A second discontinuity before anything new was decoded needs no flush.
    let flushes = h.p.stats().flushes;
    h.p.discontinuity();
    h.p.discontinuity();
    h.run();
    assert_eq!(h.p.stats().flushes, flushes + 1);
}

#[test]
fn parameter_sets_sent_separately_are_used() {
    for (_, codec) in CODECS {
        let (_, aus) = load(&name("basic", codec));
        // Split the first access unit into parameter sets and the rest.
        let params = ParameterSets::from_access_unit(codec, &aus[0]);
        let mut ps_only = Vec::new();
        for s in &params {
            ps_only.extend_from_slice(&[0, 0, 0, 1]);
            ps_only.extend_from_slice(&s.data);
        }
        let mut rest = Vec::new();
        for nal in video_pipeline::nal_units(&aus[0]) {
            if !params.iter().any(|s| *s.data == *nal) {
                rest.extend_from_slice(&[0, 0, 0, 1]);
                rest.extend_from_slice(nal);
            }
        }
        let mut h = Harness::new(PipelineConfig::default());
        h.feed(AccessUnit {
            codec,
            keyframe: false,
            pts: 0,
            data: ps_only,
        });
        h.feed(au(codec, 1, rest));
        assert_eq!(h.p.stats().configures, 1);
        assert_eq!(h.p.stats().decoded, 1);
        assert_eq!(h.p.format().unwrap().parameter_sets, params);
    }
}

/// Builds an H.265 access unit around the fixture's parameter sets with a
/// fake slice of the given NAL type.
fn h265_picture(params: Option<&ParameterSets>, nal_type: u8) -> Vec<u8> {
    let mut v = Vec::new();
    if let Some(p) = params {
        for s in p {
            v.extend_from_slice(&[0, 0, 0, 1]);
            v.extend_from_slice(&s.data);
        }
    }
    v.extend_from_slice(&[0, 0, 1, nal_type << 1, 1, 0x80, 0x55, 0xAA]);
    v
}

#[test]
fn rasl_pictures_after_a_starting_cra_are_skipped() {
    let (_, aus) = load("h265-basic.h265");
    let params = ParameterSets::from_access_unit(Codec::H265, &aus[0]);
    let mut h = Harness::new(PipelineConfig::default());
    let seq = [
        (Some(&params), 21u8), // CRA starts decoding
        (None, 8),             // RASL: skipped
        (None, 9),             // RASL: skipped
        (None, 7),             // RADL: decodable
        (None, 1),             // trailing
        (Some(&params), 21),   // CRA mid-stream
        (None, 8),             // RASL: decodable now
        (None, 1),
    ];
    for (i, (p, t)) in seq.iter().enumerate() {
        h.feed(au(Codec::H265, i as u64, h265_picture(*p, *t)));
    }
    let s = h.p.stats();
    assert_eq!(s.gated, 2);
    assert_eq!(s.decoded, 6);
}

#[test]
fn codec_switch_reconfigures() {
    let (_, a) = load("h264-basic.h264");
    let (_, b) = load("h265-basic.h265");
    let mut h = Harness::new(PipelineConfig::default());
    feed_all(&mut h, Codec::H264, &a[..25], 0);
    feed_all(&mut h, Codec::H265, &b, 25 * FRAME_US);
    let s = h.p.stats();
    assert_eq!(s.format_changes, 1);
    assert_eq!(s.decoded, 25 + 60);
    assert_eq!(h.p.format().unwrap().codec, Codec::H265);
}

#[test]
fn format_flapping_is_rate_limited() {
    let (_, aus) = load("h264-resize.h264");
    let config = PipelineConfig::default();
    let mut h = Harness::new(config);
    // Alternate between the two formats at every keyframe for ten minutes.
    let mut pts = 0;
    for i in 0..(10 * 60 * 20 / 20) {
        let start = if i % 2 == 0 { 0 } else { 40 };
        for data in &aus[start..start + 20] {
            h.feed(au(Codec::H264, pts, data.clone()));
            pts += FRAME_US;
        }
    }
    assert!(
        h.max_in_window(config.window_us, |e| matches!(e, Event::Configure { .. }))
            <= config.max_configures_per_window as usize
    );
    assert!(h.p.stats().decoded > 0);
}

#[test]
fn keyframe_flag_disagreement_is_counted_not_trusted() {
    let (_, aus) = load("h264-leading-p.h264");
    let mut h = Harness::new(PipelineConfig::default());
    // Claim every picture is a keyframe; gating still waits for the IDR.
    for (i, data) in aus.iter().enumerate() {
        h.feed(AccessUnit {
            codec: Codec::H264,
            keyframe: true,
            pts: i as u64,
            data: data.clone(),
        });
    }
    let s = h.p.stats();
    assert_eq!(s.gated, 19);
    assert!(s.keyframe_flag_mismatches >= 19);
}
