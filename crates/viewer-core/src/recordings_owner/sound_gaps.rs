//! The reasons behind empty stretches of a loudness lane
//! (`recordings_loudness` with `gaps`): see [`split_gap`]. Split from
//! [`super::sound`].

use super::{Hub, lock};
use recordings::{DeviceCamera, Secs};
use serde_json::{Value, json};
use std::sync::Arc;

/// Why a stretch of a loudness lane is empty (`recordings_loudness` with
/// `gaps`). Inclusive device-local seconds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Gap {
    pub start: Secs,
    pub end: Secs,
    pub reason: &'static str,
    pub analyzable: bool,
}

/// `a` split by the spans `cut`: the parts inside them, and the parts left.
type Spans = Vec<(Secs, Secs)>;

fn partition(a: (Secs, Secs), cut: &[(Secs, Secs)]) -> (Spans, Spans) {
    let (mut inside, mut outside) = (Vec::new(), Vec::new());
    let mut from = a.0;
    let mut spans: Vec<(Secs, Secs)> = cut.to_vec();
    spans.sort_unstable();
    for (s, e) in spans {
        if e < from || s > a.1 {
            continue;
        }
        if s > from {
            outside.push((from, s - 1));
        }
        let (is, ie) = (s.max(from), e.min(a.1));
        inside.push((is, ie));
        from = ie + 1;
        if from > a.1 {
            break;
        }
    }
    if from <= a.1 {
        outside.push((from, a.1));
    }
    (inside, outside)
}

/// Splits an empty stretch `a` of a loudness lane by what is known about
/// it: outside every recording it is `not_recorded`; inside one it is
/// `no_audio` when the camera, or measurement of that span, says it has no
/// sound, else `not_analyzed` (`analyzable` where a finished download
/// covers it). Results are in time order.
pub(super) fn split_gap(
    a: (Secs, Secs),
    recorded: &[(Secs, Secs)],
    camera_no_audio: bool,
    no_audio: &[(Secs, Secs)],
    downloaded: &[(Secs, Secs)],
) -> Vec<Gap> {
    let mut out: Vec<Gap> = Vec::new();
    let mut push = |(start, end): (Secs, Secs), reason: &'static str, analyzable: bool| {
        out.push(Gap {
            start,
            end,
            reason,
            analyzable,
        });
    };
    let (rec, unrec) = partition(a, recorded);
    for p in unrec {
        push(p, "not_recorded", false);
    }
    for r in rec {
        let (none, rest) = if camera_no_audio {
            (vec![r], Vec::new())
        } else {
            partition(r, no_audio)
        };
        for p in none {
            push(p, "no_audio", false);
        }
        for p in rest {
            let (have, lack) = partition(p, downloaded);
            for q in have {
                push(q, "not_analyzed", true);
            }
            for q in lack {
                push(q, "not_analyzed", false);
            }
        }
    }
    out.sort_by_key(|g| g.start);
    // Neighbors with the same meaning are one gap.
    let mut merged: Vec<Gap> = Vec::new();
    for g in out {
        match merged.last_mut() {
            Some(l)
                if l.reason == g.reason && l.analyzable == g.analyzable && g.start <= l.end + 1 =>
            {
                l.end = l.end.max(g.end);
            }
            _ => merged.push(g),
        }
    }
    merged
}

/// The gaps of one lane as JSON; `None` for a device that is not enabled
/// (nothing is known about its recordings).
pub(super) fn lane_gaps(
    h: &Arc<Hub>,
    key: &str,
    cam: DeviceCamera,
    lane: &recordings::sound::Lane,
    end: Secs,
) -> Option<Value> {
    let start = lane.start;
    let recorded: Vec<(Secs, Secs)> = {
        let st = lock(&h.state);
        let dev = st.devices.get(key)?;
        let idx = lock(&dev.index);
        idx.files(cam, start, end)
            .iter()
            .map(|f| (f.start, f.end))
            .collect()
    };
    let (camera_no_audio, no_audio) = lock(&h.sound.work.no_audio).of(key, cam.channel);
    // Downloaded video, by day (at most a month of days is looked at).
    let mut downloaded: Vec<(Secs, Secs)> = Vec::new();
    let mut day = recordings::day_start(start);
    let mut days = 0;
    while day <= end && days < 31 {
        for f in super::day_audio::files_for_day(h, key, cam, day).unwrap_or_default() {
            downloaded.push((f.start_ms.div_euclid(1000), f.end_ms.div_euclid(1000)));
        }
        day += 86_400;
        days += 1;
    }
    let b = i64::from(lane.bucket_s);
    let mut gaps: Vec<Gap> = Vec::new();
    let mut i = 0;
    while i < lane.peak_db.len() {
        if lane.peak_db[i].is_some() {
            i += 1;
            continue;
        }
        let from = i;
        while i < lane.peak_db.len() && lane.peak_db[i].is_none() {
            i += 1;
        }
        let a = (start + from as i64 * b, (start + i as i64 * b - 1).min(end));
        gaps.extend(split_gap(
            a,
            &recorded,
            camera_no_audio,
            &no_audio,
            &downloaded,
        ));
    }
    Some(Value::Array(
        gaps.iter()
            .map(|g| {
                json!({"start": g.start, "end": g.end, "reason": g.reason,
                       "analyzable": g.analyzable})
            })
            .collect(),
    ))
}

#[cfg(test)]
mod gap_tests {
    use super::*;

    fn g(start: Secs, end: Secs, reason: &'static str, analyzable: bool) -> Gap {
        Gap {
            start,
            end,
            reason,
            analyzable,
        }
    }

    #[test]
    fn a_gap_is_split_by_what_is_known() {
        // Recorded 100..=299 and 400..=499; a download covers 150..=199;
        // 250..=299 is known to have no sound.
        let got = split_gap(
            (50, 549),
            &[(100, 299), (400, 499)],
            false,
            &[(250, 299)],
            &[(150, 199)],
        );
        assert_eq!(
            got,
            vec![
                g(50, 99, "not_recorded", false),
                g(100, 149, "not_analyzed", false),
                g(150, 199, "not_analyzed", true),
                g(200, 249, "not_analyzed", false),
                g(250, 299, "no_audio", false),
                g(300, 399, "not_recorded", false),
                g(400, 499, "not_analyzed", false),
                g(500, 549, "not_recorded", false),
            ]
        );
    }

    #[test]
    fn a_camera_without_sound_is_no_audio_wherever_it_recorded() {
        let got = split_gap((0, 99), &[(10, 59)], true, &[], &[(10, 59)]);
        assert_eq!(
            got,
            vec![
                g(0, 9, "not_recorded", false),
                g(10, 59, "no_audio", false),
                g(60, 99, "not_recorded", false),
            ]
        );
    }

    #[test]
    fn no_recordings_at_all_is_one_not_recorded_gap() {
        assert_eq!(
            split_gap((0, 9), &[], false, &[], &[]),
            vec![g(0, 9, "not_recorded", false)]
        );
    }

    #[test]
    fn touching_files_make_one_gap() {
        let got = split_gap((0, 99), &[(0, 49), (50, 99)], false, &[], &[]);
        assert_eq!(got, vec![g(0, 99, "not_analyzed", false)]);
    }
}
