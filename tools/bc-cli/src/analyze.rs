//! Timing analysis of a recording (`bc-cli analyze`): how the tiers of one
//! channel relate in time, from an `NVRREC02` recording made by
//! `bc-cli capture` (an `NVRREC01` recording gets the per-stream part).
//!
//! It answers:
//!
//! - **Per tier**: frame rate (from the video timestamps and from arrivals),
//!   GOP length, frame and keyframe sizes, bit rate.
//! - **Clock relation**: whether the tiers' video timestamps (record header
//!   `+16`) run on one clock. For each run of a tier (from a start or a
//!   discontinuity to the next), the median of *pts − arrival* is compared
//!   with the reference tier's over the same span: on one clock the
//!   difference is minus the latency difference (a fraction of a second)
//!   and the same in every run; on separate clocks it is arbitrary and
//!   changes when a tier restarts. Pictures of the same moment with
//!   identical timestamps in both tiers are decisive: separate clocks do
//!   not agree to the microsecond picture after picture. Relative drift
//!   is the slope of the difference of the tiers' *pts − arrival*, pooled
//!   over every run and reported with its standard error; arrival jitter
//!   makes it uncertain over short runs, so it counts against a shared
//!   clock only when it is both large and significant. Keyframes' extra
//!   headers carry the device's wall-clock seconds (predicted frames carry
//!   something else there); each tier's *pts − seconds × 10⁶* is reported
//!   as a spread around its median, and on one clock the tiers' spreads
//!   overlap.
//! - **Latency difference**: for pictures of the same moment (equal or
//!   nearest timestamps), how much later the other tier arrives than the
//!   reference tier, corrected by their timestamp difference.
//! - **Starts**: for each start request in the recording, the time to the
//!   first record, the first video record, and the first keyframe; the
//!   keyframe's age compared with the run's steady latency; how many
//!   pictures came as a burst of buffered frames; and when the stream was
//!   live (back to its steady latency).
//!
//! Output is Markdown with aggregates only: no absolute times, wall-clock
//! values, or picture contents, so it may be quoted in docs. The reference
//! tier is the sub-stream when recorded.
//!
//! `bc-cli capture` receives records through the session's subscriber
//! queue, which holds back predicted frames and audio until the first
//! keyframe of a start; a start that began with predicted frames would show
//! here as a later first record, not as predicted frames.

use bc_protocol::media::{Demuxer, Record, wall_clock_secs};
use bc_protocol::recording::{Entry, Reader};
use bc_protocol::stream::StreamTier;
use std::fmt::Write as _;
use std::io::{self, Read};

/// One video record.
#[derive(Clone, Copy, Debug)]
struct Frame {
    at: i64,
    pts: i64,
    key: bool,
    size: usize,
    /// The device's wall-clock seconds, from a keyframe's extra header.
    /// Predicted frames' extra headers hold another value at the same
    /// offset (docs/protocol.md), so they get none.
    wall: Option<u32>,
    run: u32,
    /// Part of the burst of buffered pictures at the start of its run;
    /// left out of the clock and latency comparison.
    burst: bool,
}

/// Everything recorded for one tier.
#[derive(Clone, Debug, Default)]
struct Tier {
    tier: Option<StreamTier>,
    frames: Vec<Frame>,
    /// Arrival of every media record (info, video, audio), with its run.
    records: Vec<(i64, u32)>,
    infos: Vec<(u32, u32, u8)>,
    audio: u64,
    /// Start requests: arrival and the run they begin.
    starts: Vec<(i64, u32)>,
    stops: u64,
    gaps: u64,
    bad_records: u64,
    runs: u32,
}

/// A loaded recording, ready to analyze.
#[derive(Clone, Debug)]
pub struct Analysis {
    version: u8,
    tiers: Vec<Tier>,
}

fn percentile(sorted: &[f64], p: f64) -> f64 {
    if sorted.is_empty() {
        return f64::NAN;
    }
    let i = ((sorted.len() - 1) as f64 * p).round() as usize;
    sorted[i.min(sorted.len() - 1)]
}

fn sorted(mut v: Vec<f64>) -> Vec<f64> {
    v.sort_by(f64::total_cmp);
    v
}

fn median(v: Vec<f64>) -> f64 {
    percentile(&sorted(v), 0.5)
}

/// Least-squares slope of `y` over `x`.
fn slope(points: &[(f64, f64)]) -> Option<f64> {
    pooled_slope(&[points.to_vec()]).map(|f| f.slope)
}

/// A fitted slope and its standard error.
#[derive(Clone, Copy, Debug)]
struct Fit {
    slope: f64,
    se: f64,
}

/// Least-squares slope of `y` over `x` shared by several groups, each with
/// its own intercept (every group centered on its own mean), with the
/// slope's standard error.
fn pooled_slope(groups: &[Vec<(f64, f64)>]) -> Option<Fit> {
    let (mut sxx, mut sxy, mut n) = (0.0, 0.0, 0usize);
    let mut centered = Vec::new();
    for g in groups.iter().filter(|g| g.len() >= 2) {
        let k = g.len() as f64;
        let mx = g.iter().map(|p| p.0).sum::<f64>() / k;
        let my = g.iter().map(|p| p.1).sum::<f64>() / k;
        for p in g {
            let (x, y) = (p.0 - mx, p.1 - my);
            sxx += x * x;
            sxy += x * y;
            centered.push((x, y));
        }
        n += g.len();
    }
    let used = groups.iter().filter(|g| g.len() >= 2).count();
    if n < 3 || sxx <= 0.0 {
        return None;
    }
    let slope = sxy / sxx;
    let rss: f64 = centered.iter().map(|(x, y)| (y - slope * x).powi(2)).sum();
    let dof = n.saturating_sub(used + 1).max(1) as f64;
    Some(Fit {
        slope,
        se: (rss / dof / sxx).sqrt(),
    })
}

/// A drift (ppm) that is both larger than 200 ppm and more than three
/// standard errors from zero.
fn drift_significant(f: Fit) -> bool {
    f.slope.abs() > 200.0 && f.slope.abs() > 3.0 * f.se
}

fn tier_name(t: Option<StreamTier>) -> &'static str {
    match t {
        Some(StreamTier::Main) => "main",
        Some(StreamTier::Sub) => "sub",
        Some(StreamTier::Mobile) => "mobile",
        Some(StreamTier::Extern) => "extern",
        None => "stream",
    }
}

fn ms(us: f64) -> String {
    if us.is_nan() {
        "–".into()
    } else {
        format!("{:.1}", us / 1000.0)
    }
}

fn signed_ms(us: f64) -> String {
    if us.is_nan() {
        "–".into()
    } else {
        format!("{:+.1}", us / 1000.0)
    }
}

/// How a tier's `pts − wall seconds × 10⁶` (µs) spreads over its keyframes.
/// On one clock it stays within one second (the seconds are truncated) up
/// to the timestamps' own jitter.
#[derive(Clone, Copy, Debug, PartialEq)]
struct WallSpread {
    n: usize,
    min: i64,
    median: i64,
    max: i64,
}

impl WallSpread {
    fn of(t: &Tier) -> Option<WallSpread> {
        let mut d: Vec<i64> = t
            .frames
            .iter()
            .filter_map(|f| f.wall.map(|w| f.pts - i64::from(w) * 1_000_000))
            .collect();
        if d.is_empty() {
            return None;
        }
        d.sort_unstable();
        Some(WallSpread {
            n: d.len(),
            min: d[0],
            median: d[(d.len() - 1) / 2],
            max: d[d.len() - 1],
        })
    }

    fn within_one_second(&self) -> bool {
        self.max - self.min < 1_000_000 + 1_000
    }

    /// Whether the two spreads `[min, max]` share any value (1 ms slack).
    fn overlaps(&self, other: &WallSpread) -> bool {
        self.min.max(other.min) <= self.max.min(other.max) + 1_000
    }

    /// The seconds are truncated, so on a clock that follows them
    /// `pts − seconds × 10⁶ = C + (fraction of a second)` for one constant
    /// `C`: the window of `C` values these keyframes allow, `[max − 1 s,
    /// min]` (empty, `lo > hi`, when the spread exceeds a second).
    fn window(&self) -> (i64, i64) {
        (self.max - 999_999, self.min)
    }

    /// Whether one constant `C` fits both tiers' keyframes (1 ms slack).
    fn compatible(&self, other: &WallSpread) -> bool {
        let (a, b) = (self.window(), other.window());
        self.within_one_second()
            && other.within_one_second()
            && a.0.max(b.0) <= a.1.min(b.1) + 1_000
    }
}

impl Tier {
    fn new(tier: Option<StreamTier>) -> Tier {
        Tier {
            tier,
            ..Tier::default()
        }
    }

    fn run_frames(&self, run: u32) -> impl Iterator<Item = &Frame> {
        self.frames.iter().filter(move |f| f.run == run)
    }

    fn run_ids(&self) -> Vec<u32> {
        let mut ids: Vec<u32> = self.frames.iter().map(|f| f.run).collect();
        ids.dedup();
        ids
    }

    /// Median spacing of consecutive video timestamps within runs, µs.
    fn frame_interval(&self) -> f64 {
        let d: Vec<f64> = self
            .frames
            .windows(2)
            .filter(|w| w[0].run == w[1].run && w[1].pts > w[0].pts)
            .map(|w| (w[1].pts - w[0].pts) as f64)
            .collect();
        median(d)
    }

    /// Steady latency of a run (median of arrival − pts): from 3 s after
    /// its first picture on, or over the later half of a shorter run.
    fn steady(&self, run: u32) -> Option<f64> {
        let fr: Vec<&Frame> = self.run_frames(run).collect();
        let f0 = fr.first()?;
        let late: Vec<f64> = fr
            .iter()
            .filter(|f| f.at >= f0.at + 3_000_000)
            .map(|f| (f.at - f.pts) as f64)
            .collect();
        let late = if late.len() >= 5 {
            late
        } else {
            fr[fr.len() / 2..]
                .iter()
                .map(|f| (f.at - f.pts) as f64)
                .collect()
        };
        (late.len() >= 3).then(|| median(late))
    }

    /// How much older than the steady latency a picture must arrive to
    /// count as buffered: two frame intervals, at least 20 ms.
    fn burst_threshold(&self) -> f64 {
        let interval = self.frame_interval();
        if interval.is_nan() {
            40_000.0
        } else {
            (2.0 * interval).max(20_000.0)
        }
    }

    /// Leading pictures of a run that arrived as buffered frames.
    fn burst_len(&self, run: u32) -> Option<usize> {
        let st = self.steady(run)?;
        let threshold = self.burst_threshold();
        Some(
            self.run_frames(run)
                .take_while(|f| (f.at - f.pts) as f64 - st > threshold)
                .count(),
        )
    }

    /// Median of `pts - arrival` over live frames arriving in `[from, to]`.
    fn lag_median(&self, from: i64, to: i64) -> Option<f64> {
        let v: Vec<f64> = self
            .frames
            .iter()
            .filter(|f| !f.burst && f.at >= from && f.at <= to)
            .map(|f| (f.pts - f.at) as f64)
            .collect();
        (v.len() >= 5).then(|| median(v))
    }
}

impl Analysis {
    /// Reads a recording of either version.
    pub fn read<R: Read>(input: R) -> io::Result<Analysis> {
        let mut reader = Reader::new(io::BufReader::new(input))?;
        let version = reader.version();
        let mut tiers: Vec<Tier> = Vec::new();
        fn get(tiers: &mut Vec<Tier>, t: Option<StreamTier>) -> &mut Tier {
            if let Some(i) = tiers.iter().position(|x| x.tier == t) {
                return &mut tiers[i];
            }
            tiers.push(Tier::new(t));
            tiers.last_mut().expect("just pushed")
        }
        while let Some(e) = reader.next_entry()? {
            let at = e.arrival_us() as i64;
            let tier = get(&mut tiers, e.stream());
            match e {
                Entry::StartRequested { .. } => {
                    tier.runs += 1;
                    tier.starts.push((at, tier.runs));
                }
                Entry::StopRequested { .. } => tier.stops += 1,
                Entry::Discontinuity { .. } => {
                    tier.gaps += 1;
                    tier.runs += 1;
                }
                Entry::Record { bytes, .. } => {
                    let mut d = Demuxer::new();
                    d.push(&bytes);
                    let run = tier.runs;
                    tier.records.push((at, run));
                    match d.next_record() {
                        Ok(Some(Record::Video {
                            keyframe,
                            pts_us,
                            data,
                            extra_header,
                            ..
                        })) => tier.frames.push(Frame {
                            at,
                            pts: pts_us as i64,
                            key: keyframe,
                            size: data.len(),
                            wall: if keyframe {
                                wall_clock_secs(&extra_header)
                            } else {
                                None
                            },
                            run,
                            burst: false,
                        }),
                        Ok(Some(Record::Audio { .. })) => tier.audio += 1,
                        Ok(Some(Record::Info { width, height, fps })) => {
                            tier.infos.push((width, height, fps));
                        }
                        Ok(None) | Err(_) => tier.bad_records += 1,
                    }
                }
            }
        }
        tiers.retain(|t| !t.records.is_empty() || !t.starts.is_empty());
        for t in &mut tiers {
            for run in t.run_ids() {
                let n = t.burst_len(run).unwrap_or(0);
                for f in t.frames.iter_mut().filter(|f| f.run == run).take(n) {
                    f.burst = true;
                }
            }
        }
        Ok(Analysis { version, tiers })
    }

    fn reference(&self) -> Option<&Tier> {
        self.tiers
            .iter()
            .find(|t| t.tier == Some(StreamTier::Sub) && !t.frames.is_empty())
            .or_else(|| self.tiers.iter().find(|t| !t.frames.is_empty()))
    }

    /// The report, as Markdown.
    pub fn to_markdown(&self) -> String {
        let mut s = String::new();
        let _ = writeln!(s, "# Stream timing\n");
        let _ = writeln!(
            s,
            "Recording format NVRREC0{}; {} tier(s): {}.\n",
            self.version,
            self.tiers.len(),
            self.tiers
                .iter()
                .map(|t| tier_name(t.tier))
                .collect::<Vec<_>>()
                .join(", ")
        );
        self.per_tier(&mut s);
        let verdict = self.clocks(&mut s);
        self.starts(&mut s);
        let _ = writeln!(s, "## Verdict\n");
        let _ = writeln!(s, "{verdict}");
        s
    }

    fn per_tier(&self, s: &mut String) {
        let _ = writeln!(s, "## Tiers\n");
        let _ = writeln!(
            s,
            "| | {} |",
            self.tiers
                .iter()
                .map(|t| tier_name(t.tier))
                .collect::<Vec<_>>()
                .join(" | ")
        );
        let _ = writeln!(s, "|---|{}", "---|".repeat(self.tiers.len()));
        let row = |s: &mut String, name: &str, f: &dyn Fn(&Tier) -> String| {
            let cells: Vec<String> = self.tiers.iter().map(f).collect();
            let _ = writeln!(s, "| {name} | {} |", cells.join(" | "));
        };
        row(s, "starts / stops / gaps", &|t| {
            format!("{} / {} / {}", t.starts.len(), t.stops, t.gaps)
        });
        row(s, "video / audio records", &|t| {
            format!("{} / {}", t.frames.len(), t.audio)
        });
        row(s, "unreadable records", &|t| t.bad_records.to_string());
        row(s, "advertised size, fps", &|t| match t.infos.last() {
            Some((w, h, f)) => format!("{w}x{h}, {f}"),
            None => "–".into(),
        });
        row(s, "fps (timestamps)", &|t| fps(t, |f| f.pts));
        row(s, "fps (arrivals)", &|t| fps(t, |f| f.at));
        row(s, "frame interval, median ms", &|t| ms(t.frame_interval()));
        row(s, "GOP frames min / median / max", &|t| {
            let g = gops(t);
            if g.is_empty() {
                return "–".into();
            }
            let n = sorted(g.iter().map(|x| x.0 as f64).collect());
            format!("{} / {} / {}", n[0], percentile(&n, 0.5), n[n.len() - 1])
        });
        row(s, "GOP seconds min / median / max", &|t| {
            let g = gops(t);
            if g.is_empty() {
                return "–".into();
            }
            let d = sorted(g.iter().map(|x| x.1).collect());
            format!(
                "{:.2} / {:.2} / {:.2}",
                d[0] / 1e6,
                percentile(&d, 0.5) / 1e6,
                d[d.len() - 1] / 1e6
            )
        });
        row(s, "keyframe KB median / max", &|t| {
            let k = sorted(
                t.frames
                    .iter()
                    .filter(|f| f.key)
                    .map(|f| f.size as f64)
                    .collect(),
            );
            if k.is_empty() {
                return "–".into();
            }
            format!(
                "{:.1} / {:.1}",
                percentile(&k, 0.5) / 1024.0,
                k[k.len() - 1] / 1024.0
            )
        });
        row(s, "other frame KB median", &|t| {
            let p: Vec<f64> = t
                .frames
                .iter()
                .filter(|f| !f.key)
                .map(|f| f.size as f64)
                .collect();
            if p.is_empty() {
                "–".into()
            } else {
                format!("{:.1}", median(p) / 1024.0)
            }
        });
        row(s, "video kbit/s", &|t| {
            let span = pts_span(t);
            if span <= 0.0 {
                return "–".into();
            }
            let bytes: usize = t.frames.iter().map(|f| f.size).sum();
            format!("{:.0}", bytes as f64 * 8.0 / (span / 1e6) / 1000.0)
        });
        row(s, "pts drift vs arrivals, ppm", &|t| match drift(t) {
            Some(d) => format!("{d:+.0}"),
            None => "–".into(),
        });
        row(s, "keyframes with wall-clock seconds", &|t| {
            t.frames
                .iter()
                .filter(|f| f.wall.is_some())
                .count()
                .to_string()
        });
        row(
            s,
            "pts − seconds × 10⁶ around its median, ms min / median / max",
            &|t| match WallSpread::of(t) {
                None => "–".into(),
                Some(w) => format!(
                    "{} / {} / {}",
                    signed_ms((w.min - w.median) as f64),
                    signed_ms(0.0),
                    signed_ms((w.max - w.median) as f64)
                ),
            },
        );
        row(
            s,
            "pts within one wall-clock second",
            &|t| match WallSpread::of(t) {
                None => "–".into(),
                Some(w) if w.within_one_second() => {
                    format!("yes (spread {} ms)", ms((w.max - w.min) as f64))
                }
                Some(w) => format!("no (spread {} ms)", ms((w.max - w.min) as f64)),
            },
        );
        s.push('\n');
    }

    /// The clock comparison; returns the verdict line.
    fn clocks(&self, s: &mut String) -> String {
        let Some(reference) = self.reference() else {
            return "shared clock: unknown (no video)".into();
        };
        let others: Vec<&Tier> = self
            .tiers
            .iter()
            .filter(|t| t.tier != reference.tier && !t.frames.is_empty())
            .collect();
        if others.is_empty() {
            return "shared clock: unknown (only one tier recorded)".into();
        }
        let rname = tier_name(reference.tier);
        let _ = writeln!(s, "## Clock relation (reference: {rname})\n");
        let mut verdicts = Vec::new();
        for o in others {
            let oname = tier_name(o.tier);
            let _ = writeln!(s, "### {oname} vs {rname}\n");
            let _ = writeln!(
                s,
                "Offset: median of ({oname} pts − arrival) minus ({rname} pts − arrival) over \
                 each {oname} run. On one clock it is minus the latency difference and the \
                 same in every run.\n"
            );
            let _ = writeln!(
                s,
                "Drift: slope of the difference of the two tiers' pts − arrival, ppm ± one \
                 standard error; arrival jitter dominates it over short runs.\n"
            );
            let _ = writeln!(s, "| run | frames | seconds | offset ms | drift ppm |");
            let _ = writeln!(s, "|---|---|---|---|---|");
            let mut offsets = Vec::new();
            let mut drift_groups: Vec<Vec<(f64, f64)>> = Vec::new();
            for (n, run) in o.run_ids().into_iter().enumerate() {
                let fr: Vec<&Frame> = o.run_frames(run).filter(|f| !f.burst).collect();
                if fr.is_empty() {
                    continue;
                }
                let (from, to) = (fr[0].at, fr[fr.len() - 1].at);
                let mine: Vec<f64> = fr.iter().map(|f| (f.pts - f.at) as f64).collect();
                let off = (mine.len() >= 5)
                    .then(|| reference.lag_median(from, to).map(|r| median(mine) - r))
                    .flatten();
                // Relative drift: this tier's pts − arrival minus the
                // reference's (at the reference picture nearest in arrival),
                // against arrival seconds.
                let rel_points: Vec<(f64, f64)> = fr
                    .iter()
                    .filter_map(|f| {
                        let g = nearest_at(reference, f.at, from, to)?;
                        Some((f.at as f64 / 1e6, ((f.pts - f.at) - (g.pts - g.at)) as f64))
                    })
                    .collect();
                let rel = if to - from >= 10_000_000 {
                    pooled_slope(std::slice::from_ref(&rel_points))
                } else {
                    None
                };
                if let Some(x) = off {
                    offsets.push(x);
                }
                drift_groups.push(rel_points);
                let _ = writeln!(
                    s,
                    "| {} | {} | {:.1} | {} | {} |",
                    n + 1,
                    fr.len(),
                    (to - from) as f64 / 1e6,
                    off.map_or("–".into(), ms),
                    rel.map_or("–".into(), |d| format!("{:+.1} ± {:.1}", d.slope, d.se))
                );
            }
            s.push('\n');
            let pooled = pooled_slope(&drift_groups);
            if let Some(d) = pooled {
                let _ = writeln!(
                    s,
                    "Drift pooled over every run: {:+.1} ± {:.1} ppm{}.\n",
                    d.slope,
                    d.se,
                    if drift_significant(d) {
                        ""
                    } else {
                        " (not significant: under 200 ppm or within three standard errors)"
                    }
                );
            }

            // Pictures of the same moment.
            // Half the shorter frame interval: a picture of the other
            // tier between two reference pictures is not the same moment.
            let interval = reference.frame_interval().min(o.frame_interval());
            let tol = (interval / 2.0).clamp(1_000.0, 50_000.0);
            let mut lat = Vec::new();
            let mut exact = 0usize;
            let mut exact_span = (i64::MAX, i64::MIN);
            for f in o.frames.iter().filter(|f| !f.burst) {
                let Some(g) = nearest_pts(reference, f) else {
                    continue;
                };
                let dp = (f.pts - g.pts) as f64;
                if dp.abs() >= tol {
                    continue;
                }
                if f.pts == g.pts {
                    exact += 1;
                    exact_span = (exact_span.0.min(f.pts), exact_span.1.max(f.pts));
                }
                lat.push((f.at - g.at) as f64 - dp);
            }
            let lat = sorted(lat);
            let _ = writeln!(
                s,
                "Pictures of the same moment ({oname} frames with a {rname} frame closer \
                 than {} ms to its timestamp): {} of {}, {} with identical timestamps.\n",
                ms(tol),
                lat.len(),
                o.frames.len(),
                exact
            );
            if exact >= 2 {
                let _ = writeln!(
                    s,
                    "The identical timestamps span {:.1} s of {rname}'s clock: over that span the \
                     two clocks differ by 0 µs (drift 0 ppm, to the timestamps' 1 µs resolution).\n",
                    (exact_span.1 - exact_span.0) as f64 / 1e6
                );
            }
            if !lat.is_empty() {
                let mean = lat.iter().sum::<f64>() / lat.len() as f64;
                let sd =
                    (lat.iter().map(|x| (x - mean).powi(2)).sum::<f64>() / lat.len() as f64).sqrt();
                let _ = writeln!(
                    s,
                    "| {oname} arrives later than {rname} by | ms |\n|---|---|\n\
                     | median | {} |\n| p10 | {} |\n| p90 | {} |\n\
                     | jitter (p90 − p10) | {} |\n| standard deviation | {} |\n",
                    ms(percentile(&lat, 0.5)),
                    ms(percentile(&lat, 0.1)),
                    ms(percentile(&lat, 0.9)),
                    ms(percentile(&lat, 0.9) - percentile(&lat, 0.1)),
                    ms(sd)
                );
            }

            // Wall-clock seconds.
            let wall = match (WallSpread::of(reference), WallSpread::of(o)) {
                (Some(r), Some(w)) => {
                    let agree = r.compatible(&w);
                    let rel = |x: i64| signed_ms((x - r.median) as f64);
                    let (rw, ow) = (r.window(), w.window());
                    let _ = writeln!(
                        s,
                        "Wall-clock seconds (keyframes): pts − seconds × 10⁶ relative to \
                         {rname}'s median, ms:\n\n\
                         | tier | keyframes | min | median | max |\n|---|---|---|---|---|\n\
                         | {rname} | {} | {} | {} | {} |\n| {oname} | {} | {} | {} | {} |\n\n\
                         The spreads {}. Seconds are truncated, so on a clock that follows them \
                         pts − seconds × 10⁶ is one constant plus a fraction of a second: {rname}'s \
                         keyframes allow that constant from {} to {} ms, {oname}'s from {} to {} ms; \
                         {}.\n",
                        r.n,
                        rel(r.min),
                        rel(r.median),
                        rel(r.max),
                        w.n,
                        rel(w.min),
                        rel(w.median),
                        rel(w.max),
                        if r.overlaps(&w) {
                            "overlap"
                        } else {
                            "do not overlap"
                        },
                        rel(rw.0),
                        rel(rw.1),
                        rel(ow.0),
                        rel(ow.1),
                        if agree {
                            "one constant fits both tiers"
                        } else {
                            "no one constant fits both tiers"
                        }
                    );
                    Some(agree)
                }
                _ => {
                    let _ = writeln!(
                        s,
                        "Wall-clock seconds: not in both tiers' keyframes; not compared.\n"
                    );
                    None
                }
            };
            verdicts.push((oname, offsets, pooled, lat.len(), exact, wall));
        }

        let mut lines = Vec::new();
        for (oname, offsets, pooled, matched, exact, wall) in verdicts {
            // Identical timestamps on (nearly) every picture of the same
            // moment: separate clocks cannot do that.
            let identical = exact >= 10 && exact * 10 >= matched * 9;
            if offsets.is_empty() && !identical {
                lines.push(format!(
                    "shared clock ({oname}/{rname}): unknown (no overlapping runs)"
                ));
                continue;
            }
            let (lo, hi) = offsets
                .iter()
                .fold((f64::MAX, f64::MIN), |(a, b), &x| (a.min(x), b.max(x)));
            let small = lo.abs().max(hi.abs()) < 2_000_000.0;
            let steady = hi - lo < 200_000.0;
            let drifting = pooled.is_some_and(drift_significant);
            let wall_ok = wall.unwrap_or(true);
            let shared = identical || (small && steady && !drifting && wall_ok);
            let mut evidence = Vec::new();
            if identical {
                evidence.push(format!(
                    "{exact} of {matched} matched pictures with identical timestamps (decisive)"
                ));
            }
            if !offsets.is_empty() {
                evidence.push(format!(
                    "offset {} to {} ms over {} run(s)",
                    ms(lo),
                    ms(hi),
                    offsets.len()
                ));
            }
            if let Some(d) = pooled {
                evidence.push(format!(
                    "relative drift {:+.0} ± {:.0} ppm{}",
                    d.slope,
                    d.se,
                    if drifting { "" } else { " (not significant)" }
                ));
            }
            if !identical {
                evidence.push(format!(
                    "{exact} of {matched} matched pictures with identical timestamps"
                ));
            }
            match wall {
                Some(true) => evidence.push("wall-clock windows overlap".into()),
                Some(false) => evidence.push("wall-clock windows disagree".into()),
                None => evidence.push("no wall-clock seconds to compare".into()),
            }
            lines.push(format!(
                "shared clock ({oname}/{rname}): {} ({})",
                if shared { "yes" } else { "no" },
                evidence.join("; ")
            ));
            if !shared {
                lines.push(
                    "Future work (not implemented): align the tiers by correlating motion \
                     between their decoded pictures instead of by timestamps."
                        .into(),
                );
            }
        }
        lines.join("\n\n")
    }

    fn starts(&self, s: &mut String) {
        let any = self.tiers.iter().any(|t| !t.starts.is_empty());
        if !any {
            return;
        }
        let _ = writeln!(s, "## Starts\n");
        let _ = writeln!(
            s,
            "Times from the start request, ms. *Age*: how much older the first keyframe was \
             than the run's steady latency. *Burst*: pictures that arrived older than that by \
             more than two frame intervals (buffered frames). *Live*: the first picture back at \
             the steady latency.\n"
        );
        let _ = writeln!(
            s,
            "| tier | # | first record | first video | first keyframe | first video is key | age | burst | live |"
        );
        let _ = writeln!(s, "|---|---|---|---|---|---|---|---|---|");
        // Per tier: times to the first keyframe, times to live, burst sizes.
        type Samples = (Option<StreamTier>, Vec<f64>, Vec<f64>, Vec<f64>);
        let mut summary: Vec<Samples> = Vec::new();
        for t in &self.tiers {
            let mut keys = Vec::new();
            let mut lives = Vec::new();
            let mut bursts = Vec::new();
            for (n, &(at, run)) in t.starts.iter().enumerate() {
                let first_record = t.records.iter().find(|r| r.1 == run).map(|r| r.0 - at);
                let fr: Vec<&Frame> = t.run_frames(run).collect();
                let first_video = fr.first().map(|f| f.at - at);
                let first_key = fr.iter().find(|f| f.key).map(|f| f.at - at);
                let first_is_key = fr.first().map(|f| f.key);
                let (age, burst, live) = match t.steady(run) {
                    Some(st) => {
                        let excess = |f: &Frame| (f.at - f.pts) as f64 - st;
                        let age = fr.iter().find(|f| f.key).map(|f| excess(f));
                        let burst = fr.iter().take_while(|f| f.burst).count();
                        let live = fr.get(burst).map(|f| f.at - at);
                        (age, Some(burst), live)
                    }
                    None => (None, None, None),
                };
                if let Some(k) = first_key {
                    keys.push(k as f64);
                }
                if let Some(l) = live {
                    lives.push(l as f64);
                }
                if let Some(b) = burst {
                    bursts.push(b as f64);
                }
                let opt = |v: Option<i64>| v.map_or("–".into(), |x| ms(x as f64));
                let _ = writeln!(
                    s,
                    "| {} | {} | {} | {} | {} | {} | {} | {} | {} |",
                    tier_name(t.tier),
                    n + 1,
                    opt(first_record),
                    opt(first_video),
                    opt(first_key),
                    first_is_key.map_or("–", |k| if k { "yes" } else { "no" }),
                    age.map_or("–".into(), ms),
                    burst.map_or("–".into(), |b| b.to_string()),
                    opt(live)
                );
            }
            summary.push((t.tier, keys, lives, bursts));
        }
        s.push('\n');
        let _ = writeln!(
            s,
            "| tier | starts | to first keyframe, median (min–max) ms | to live, median ms | burst, median frames |"
        );
        let _ = writeln!(s, "|---|---|---|---|---|");
        for (tier, keys, lives, bursts) in summary {
            if keys.is_empty() {
                continue;
            }
            let k = sorted(keys);
            let _ = writeln!(
                s,
                "| {} | {} | {} ({}–{}) | {} | {} |",
                tier_name(tier),
                k.len(),
                ms(percentile(&k, 0.5)),
                ms(k[0]),
                ms(k[k.len() - 1]),
                ms(median(lives)),
                if bursts.is_empty() {
                    "–".into()
                } else {
                    format!("{}", median(bursts))
                }
            );
        }
        s.push('\n');
    }
}

/// Frames per second within runs, from `time` (µs) of each frame.
fn fps(t: &Tier, time: impl Fn(&Frame) -> i64) -> String {
    let (mut intervals, mut span) = (0u64, 0i64);
    for run in t.run_ids() {
        let fr: Vec<&Frame> = t.run_frames(run).collect();
        if fr.len() >= 2 {
            intervals += fr.len() as u64 - 1;
            span += time(fr[fr.len() - 1]) - time(fr[0]);
        }
    }
    if span <= 0 {
        "–".into()
    } else {
        format!("{:.2}", intervals as f64 / (span as f64 / 1e6))
    }
}

fn pts_span(t: &Tier) -> f64 {
    t.run_ids()
        .into_iter()
        .map(|run| {
            let fr: Vec<&Frame> = t.run_frames(run).collect();
            fr.last().map_or(0, |l| l.pts - fr[0].pts) as f64
        })
        .sum()
}

/// GOPs within runs: (frames from one keyframe to the next, timestamp span).
fn gops(t: &Tier) -> Vec<(usize, f64)> {
    let mut out = Vec::new();
    for run in t.run_ids() {
        let fr: Vec<&Frame> = t.run_frames(run).collect();
        let keys: Vec<usize> = (0..fr.len()).filter(|&i| fr[i].key).collect();
        for w in keys.windows(2) {
            out.push((w[1] - w[0], (fr[w[1]].pts - fr[w[0]].pts) as f64));
        }
    }
    out
}

/// Drift of a tier's timestamps against arrivals over its longest run, ppm.
fn drift(t: &Tier) -> Option<f64> {
    let run = t
        .run_ids()
        .into_iter()
        .max_by_key(|&r| t.run_frames(r).count())?;
    let pts: Vec<(f64, f64)> = t
        .run_frames(run)
        .map(|f| (f.at as f64 / 1e6, (f.pts - f.at) as f64))
        .collect();
    let span = pts.last()?.0 - pts.first()?.0;
    if span < 10.0 {
        return None;
    }
    slope(&pts)
}

/// The live reference frame that arrived nearest to `at`, within
/// `[from, to]`.
fn nearest_at(reference: &Tier, at: i64, from: i64, to: i64) -> Option<&Frame> {
    reference
        .frames
        .iter()
        .filter(|g| !g.burst && g.at >= from && g.at <= to)
        .min_by_key(|g| (g.at - at).abs())
}

/// The reference frame with the timestamp nearest to `f`'s.
fn nearest_pts<'a>(reference: &'a Tier, f: &Frame) -> Option<&'a Frame> {
    reference
        .frames
        .iter()
        .filter(|g| !g.burst)
        .min_by_key(|g| (g.pts - f.pts).abs())
}

#[cfg(test)]
mod tests {
    use super::*;
    use bc_protocol::media::Codec;
    use bc_protocol::recording::TierWriter;

    /// Predicted frames' extra headers hold this (not seconds) where
    /// keyframes hold the wall-clock seconds, as observed.
    const P_FRAME_WORD: u32 = 0x0900_0006;

    fn pic(keyframe: bool, pts_us: u64, wall: Option<u32>) -> Record {
        let extra_header = wall.map_or(Vec::new(), |w| {
            let word = if keyframe { w } else { P_FRAME_WORD };
            let mut e = word.to_le_bytes().to_vec();
            e.extend([0u8; 4]);
            e
        });
        Record::Video {
            codec: Codec::H265,
            keyframe,
            pts_us,
            data: vec![7; if keyframe { 4000 } else { 500 }],
            extra_header,
        }
    }

    /// Sub at 10 fps (GOP 20), main at 20 fps (GOP 40), on one picture
    /// clock that is `clock0` µs ahead of capture time 0; main's pts are
    /// shifted by `main_shift` (0 for one clock). Sub arrives 30 ms after
    /// capture, main 150 ms (plus a little jitter). Main starts at 2 s and
    /// restarts every 10 s with a burst of `burst` buffered pictures.
    fn synthetic(main_shift: i64, burst: u64) -> Vec<u8> {
        build(Synth {
            main_shift,
            burst,
            ..Synth::default()
        })
    }

    #[derive(Clone, Copy)]
    struct Synth {
        main_shift: i64,
        burst: u64,
        /// Arrival jitter added to both tiers, ± µs (deterministic).
        jitter: u64,
        /// Main restarts every `period` µs and runs `pictures` pictures.
        period: u64,
        pictures: u64,
        seconds: u64,
    }

    impl Default for Synth {
        fn default() -> Synth {
            Synth {
                main_shift: 0,
                burst: 0,
                jitter: 0,
                period: 10_000_000,
                pictures: 140,
                seconds: 40,
            }
        }
    }

    /// Deterministic jitter in `[-amp, amp]` µs for picture `i` of `salt`.
    fn jitter(amp: u64, salt: u64, i: u64) -> i64 {
        if amp == 0 {
            return 0;
        }
        let mut x = (i ^ (salt << 32)).wrapping_mul(0x9E37_79B9_7F4A_7C15);
        x ^= x >> 29;
        x = x.wrapping_mul(0xBF58_476D_1CE4_E5B9);
        x ^= x >> 32;
        (x % (2 * amp + 1)) as i64 - amp as i64
    }

    fn build(p: Synth) -> Vec<u8> {
        let clock0 = 5_000_000_000i64;
        let wall0 = 1_000_000u32;
        let mut items: Vec<(u64, u8, StreamTier, Option<Record>)> = Vec::new();
        for i in 0..p.seconds * 10 {
            let c = i * 100_000;
            let pts = (clock0 + c as i64) as u64;
            let wall = wall0 + ((clock0 + c as i64 - clock0) / 1_000_000) as u32;
            let at = (c as i64 + 30_000 + p.jitter as i64 + jitter(p.jitter, 1, i)) as u64;
            items.push((
                at,
                0,
                StreamTier::Sub,
                Some(pic(i % 20 == 0, pts, Some(wall))),
            ));
        }
        let mut start = 2_000_000u64;
        while start + p.period - 2_000_000 < p.seconds * 1_000_000 {
            items.push((start, 1, StreamTier::Main, None));
            // The device answers 300 ms later with pictures captured from
            // `burst` frames before then, at once, then live.
            let first_c = start + 300_000 - 150_000 - p.burst * 50_000;
            let first_c = first_c / 50_000 * 50_000;
            for k in 0..p.pictures {
                let c = first_c + k * 50_000;
                let live_at = (c as i64
                    + 150_000
                    + ((k * 7919 % 11) * 1_000) as i64
                    + jitter(p.jitter, 2 + start, k)) as u64;
                let at = live_at.max(start + 300_000 + k * 1_000);
                let pts = (clock0 + c as i64 + p.main_shift) as u64;
                let wall = wall0 + (c / 1_000_000) as u32;
                items.push((
                    at,
                    2,
                    StreamTier::Main,
                    Some(pic(k % 40 == 0, pts, Some(wall))),
                ));
            }
            items.push((start + p.period - 2_500_000, 3, StreamTier::Main, None));
            start += p.period;
        }
        items.sort_by_key(|x| (x.0, x.1));
        let mut w = TierWriter::new(Vec::new()).unwrap();
        w.start_requested(StreamTier::Sub, 0).unwrap();
        for (at, kind, tier, r) in items {
            match (kind, r) {
                (1, None) => w.start_requested(tier, at).unwrap(),
                (3, None) => w.stop_requested(tier, at).unwrap(),
                (_, Some(r)) => {
                    // Main records after its stop are not delivered.
                    w.record(tier, at, &r).unwrap();
                }
                _ => unreachable!(),
            }
        }
        w.finish().unwrap()
    }

    #[test]
    fn one_clock_is_recognized_with_the_latency_difference() {
        let a = Analysis::read(&synthetic(0, 6)[..]).unwrap();
        let md = a.to_markdown();
        assert!(md.contains("shared clock (main/sub): yes"), "{md}");
        assert!(md.contains("| fps (timestamps) | 10.00 | 20.00 |"), "{md}");
        assert!(
            md.contains("| GOP frames min / median / max | 20 / 20 / 20 | 40 / 40 / 40 |"),
            "{md}"
        );
        // 150 − 30 ms, plus 0–10 ms of jitter.
        let line = md.lines().find(|l| l.starts_with("| median |")).unwrap();
        let v: f64 = line.split('|').nth(2).unwrap().trim().parse().unwrap();
        assert!((120.0..=131.0).contains(&v), "{line}");
        assert!(md.contains("wall-clock windows overlap"), "{md}");
        // Every start: first record 300 ms after the request, a keyframe.
        assert!(
            md.contains("| main | 1 | 300.0 | 300.0 | 300.0 | yes |"),
            "{md}"
        );
        // Starting six pictures early: a burst of buffered pictures.
        let row = md.lines().find(|l| l.starts_with("| main | 2 |")).unwrap();
        let burst: usize = row.split('|').nth(8).unwrap().trim().parse().unwrap();
        assert!((4..=7).contains(&burst), "{row}");
        assert!(!md.contains("Future work"));
    }

    #[test]
    fn separate_clocks_are_rejected() {
        // Main's timestamps 37 s off the sub-stream's.
        let a = Analysis::read(&synthetic(37_000_000, 0)[..]).unwrap();
        let md = a.to_markdown();
        assert!(md.contains("shared clock (main/sub): no"), "{md}");
        assert!(md.contains("wall-clock windows disagree"), "{md}");
        assert!(md.contains("Future work"), "{md}");
    }

    /// Bug: over runs of tens of seconds, arrival jitter of a couple of
    /// hundred milliseconds made the per-run drift swing by hundreds of
    /// ppm, and that noise vetoed a shared clock although every picture of
    /// the same moment had the identical timestamp in both tiers.
    #[test]
    fn arrival_jitter_does_not_veto_identical_timestamps() {
        let p = Synth {
            jitter: 110_000,
            period: 15_000_000,
            pictures: 240,
            seconds: 62,
            ..Synth::default()
        };
        let a = Analysis::read(&build(p)[..]).unwrap();
        let md = a.to_markdown();
        // The per-run drift really is noisy: at least one run's estimate
        // exceeds the old 200 ppm limit.
        let noisy = md
            .lines()
            .filter(|l| l.contains(" ± ") && l.starts_with("| "))
            .filter_map(|l| {
                l.split('|')
                    .nth(5)?
                    .split('±')
                    .next()?
                    .trim()
                    .parse::<f64>()
                    .ok()
            })
            .any(|d| d.abs() > 200.0);
        assert!(noisy, "{md}");
        assert!(md.contains("shared clock (main/sub): yes"), "{md}");
        assert!(md.contains("identical timestamps (decisive)"), "{md}");
        assert!(!md.contains("Future work"), "{md}");
    }

    /// Bug: only keyframes carry the wall-clock seconds, but every frame's
    /// extra header was read as seconds, so each tier's window came out
    /// empty and the tiers were reported as disagreeing (with an offset
    /// range that contained 0).
    #[test]
    fn wall_clock_seconds_come_from_keyframes() {
        let a = Analysis::read(&synthetic(0, 0)[..]).unwrap();
        let md = a.to_markdown();
        assert!(
            md.contains("| keyframes with wall-clock seconds | 20 |"),
            "{md}"
        );
        assert!(md.contains("; one constant fits both tiers."), "{md}");
        assert!(md.contains("wall-clock windows overlap"), "{md}");
        let row = md
            .lines()
            .find(|l| l.starts_with("| pts within one wall-clock second |"))
            .unwrap();
        assert_eq!(row.matches("yes (spread").count(), 2, "{row}");
    }

    #[test]
    fn spreads_overlap_whatever_their_order() {
        let w = |min, max| WallSpread {
            n: 2,
            min,
            median: (min + max) / 2,
            max,
        };
        // Main's spread lies from 159.9 ms above to 52.7 ms below sub's
        // (the range contains 0): that is overlap.
        let sub = w(0, 900_000);
        let main = w(159_900, 900_000 - 52_700);
        assert!(sub.overlaps(&main) && main.overlaps(&sub));
        let main = w(-300_000, 100_000);
        assert!(sub.overlaps(&main) && main.overlaps(&sub));
        let far = w(37_000_000, 37_900_000);
        assert!(!sub.overlaps(&far) && !far.overlaps(&sub));
        assert!(sub.within_one_second() && !w(0, 1_200_000).within_one_second());
    }

    #[test]
    fn pooled_slope_fits_one_slope_across_runs_with_its_error() {
        // Two runs, slope 3, different intercepts, no noise.
        let a: Vec<(f64, f64)> = (0..10)
            .map(|i| (i as f64, 3.0 * i as f64 + 100.0))
            .collect();
        let b: Vec<(f64, f64)> = (20..30)
            .map(|i| (i as f64, 3.0 * i as f64 - 500.0))
            .collect();
        let f = pooled_slope(&[a, b]).unwrap();
        assert!((f.slope - 3.0).abs() < 1e-9 && f.se < 1e-6, "{f:?}");
        assert!(!drift_significant(Fit {
            slope: 400.0,
            se: 300.0
        }));
        assert!(drift_significant(Fit {
            slope: 400.0,
            se: 100.0
        }));
        assert!(!drift_significant(Fit {
            slope: 150.0,
            se: 1.0
        }));
    }

    #[test]
    fn a_one_stream_recording_gets_the_per_stream_part() {
        let mut w = bc_protocol::recording::Writer::new(Vec::new()).unwrap();
        for i in 0..50u64 {
            w.record(i * 40_000 + 5_000, &pic(i % 25 == 0, i * 40_000, None))
                .unwrap();
        }
        let a = Analysis::read(&w.finish().unwrap()[..]).unwrap();
        let md = a.to_markdown();
        assert!(md.contains("NVRREC01"));
        assert!(md.contains("| fps (timestamps) | 25.00 |"), "{md}");
        assert!(
            md.contains("shared clock: unknown (only one tier recorded)"),
            "{md}"
        );
    }
}
