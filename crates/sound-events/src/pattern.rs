//! Temporal patterns and the matcher that finds them in a stream of pulses.

use crate::SAMPLE_RATE;

/// A temporal alarm pattern: groups of equal pulses, then a long pause.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Pattern {
    /// Duration of each pulse, seconds.
    pub on_s: f32,
    /// Pause between the pulses of a group, seconds.
    pub short_off_s: f32,
    /// Pulses per group.
    pub pulses: u8,
    /// Pause after a group, seconds.
    pub long_off_s: f32,
}

impl Pattern {
    /// Duration of one cycle (a group and the pause after it), seconds.
    pub fn cycle_s(&self) -> f32 {
        f32::from(self.pulses) * self.on_s
            + f32::from(self.pulses.saturating_sub(1)) * self.short_off_s
            + self.long_off_s
    }
}

/// Temporal-three (ISO 8201, ANSI/ASA S3.41, NFPA 72): smoke and fire.
pub const T3: Pattern = Pattern {
    on_s: 0.5,
    short_off_s: 0.5,
    pulses: 3,
    long_off_s: 1.5,
};

/// Temporal-four (NFPA 720, now NFPA 72; UL 2034): carbon monoxide.
pub const T4: Pattern = Pattern {
    on_s: 0.1,
    short_off_s: 0.1,
    pulses: 4,
    long_off_s: 5.0,
};

/// A range of durations in samples, inclusive.
#[derive(Clone, Copy, Debug)]
struct Range {
    min: u64,
    max: u64,
}

impl Range {
    fn new(nominal_s: f32, rel: f32, abs_s: f32) -> Range {
        let slack = nominal_s * rel + abs_s;
        let rate = SAMPLE_RATE as f32;
        Range {
            min: ((nominal_s - slack).max(0.0) * rate) as u64,
            max: ((nominal_s + slack) * rate).ceil() as u64,
        }
    }

    fn contains(&self, d: u64) -> bool {
        d >= self.min && d <= self.max
    }
}

/// What a matcher reports.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum MatchEvent {
    Started { start: u64, cycles: u32 },
    Ended { start: u64, end: u64, cycles: u32 },
}

#[derive(Clone, Copy, Debug)]
struct Episode {
    start: u64,
    last_end: u64,
    cycles: u32,
}

/// Finds one pattern in a stream of pulse edges, given in sample numbers.
#[derive(Clone, Debug)]
pub(crate) struct Matcher {
    pulses: u8,
    on: Range,
    short_off: Range,
    long_off: Range,
    hangover: u64,
    min_cycles: u32,

    /// Start of the pulse in progress.
    in_pulse: Option<u64>,
    /// End of the last pulse.
    last_end: Option<u64>,
    /// Pulses in the current group, the one in progress included.
    group_n: u8,
    group_start: u64,
    /// The current group follows a confirmed cycle within a long pause.
    group_linked: bool,
    /// Consecutive linked cycles so far.
    chain: u32,
    chain_start: u64,
    /// End of the last confirmed group, while the next one may link to it.
    linkable_end: Option<u64>,
    episode: Option<Episode>,
}

impl Matcher {
    pub fn new(p: &Pattern, rel: f32, abs_s: f32, min_cycles: u32, missed_cycles: u32) -> Matcher {
        let long_off = Range::new(p.long_off_s, rel, abs_s);
        let hangover =
            long_off.max + (missed_cycles as f32 * p.cycle_s() * SAMPLE_RATE as f32) as u64;
        Matcher {
            pulses: p.pulses.max(1),
            on: Range::new(p.on_s, rel, abs_s),
            short_off: Range::new(p.short_off_s, rel, abs_s),
            long_off,
            hangover,
            min_cycles: min_cycles.max(1),
            in_pulse: None,
            last_end: None,
            group_n: 0,
            group_start: 0,
            group_linked: false,
            chain: 0,
            chain_start: 0,
            linkable_end: None,
            episode: None,
        }
    }

    fn break_group(&mut self) {
        self.group_n = 0;
        self.chain = 0;
        self.linkable_end = None;
    }

    fn begin_group(&mut self, t: u64) {
        self.group_linked = self
            .linkable_end
            .is_some_and(|e| self.long_off.contains(t.saturating_sub(e)));
        if !self.group_linked {
            self.chain = 0;
        }
        self.linkable_end = None;
        self.group_n = 1;
        self.group_start = t;
    }

    fn group_complete(&self) -> bool {
        self.group_n == self.pulses && self.in_pulse.is_none()
    }

    fn confirm(&mut self, emit: &mut impl FnMut(MatchEvent)) {
        let end = self.last_end.unwrap_or(self.group_start);
        self.group_n = 0;
        if self.group_linked {
            self.chain += 1;
        } else {
            self.chain = 1;
            self.chain_start = self.group_start;
        }
        self.linkable_end = Some(end);
        match &mut self.episode {
            Some(ep) => {
                ep.cycles += 1;
                ep.last_end = end;
            }
            None if self.chain >= self.min_cycles => {
                self.episode = Some(Episode {
                    start: self.chain_start,
                    last_end: end,
                    cycles: self.chain,
                });
                emit(MatchEvent::Started {
                    start: self.chain_start,
                    cycles: self.chain,
                });
            }
            None => {}
        }
    }

    pub fn pulse_start(&mut self, t: u64, emit: &mut impl FnMut(MatchEvent)) {
        // Settle what the pause before this pulse decided.
        self.tick(t, emit);
        let gap = self.last_end.map(|e| t.saturating_sub(e));
        if self.group_n > 0 {
            // Mid-group (a complete group would have been confirmed by the
            // tick if the pause were long enough; a short pause means one
            // pulse too many).
            if self.group_n < self.pulses && gap.is_some_and(|g| self.short_off.contains(g)) {
                self.group_n += 1;
            } else {
                self.break_group();
                self.begin_group(t);
            }
        } else {
            self.begin_group(t);
        }
        self.in_pulse = Some(t);
    }

    pub fn pulse_end(&mut self, t: u64) {
        let Some(start) = self.in_pulse.take() else {
            // Already rejected as too long.
            self.last_end = Some(t);
            return;
        };
        self.last_end = Some(t);
        if self.group_n > 0 && !self.on.contains(t.saturating_sub(start)) {
            self.break_group();
        }
    }

    /// A discontinuity: forgets the pulse in progress and the current group.
    /// An episode in progress ends by its hangover as usual.
    pub fn reset_pulses(&mut self, t: u64) {
        self.in_pulse = None;
        self.last_end = Some(t);
        self.break_group();
    }

    pub fn tick(&mut self, now: u64, emit: &mut impl FnMut(MatchEvent)) {
        if let Some(start) = self.in_pulse {
            if now.saturating_sub(start) > self.on.max && self.group_n > 0 {
                // Too long to be a pulse of this pattern (a steady tone).
                self.break_group();
            }
        } else if let Some(end) = self.last_end {
            let quiet = now.saturating_sub(end);
            if self.group_complete() && quiet >= self.long_off.min {
                self.confirm(emit);
            } else if self.group_n > 0 && self.group_n < self.pulses && quiet > self.short_off.max {
                self.break_group();
            }
        }
        if self
            .linkable_end
            .is_some_and(|e| now.saturating_sub(e) > self.long_off.max)
        {
            self.linkable_end = None;
            self.chain = 0;
        }
        if let Some(ep) = self.episode
            && self.group_n == 0
            && now.saturating_sub(ep.last_end) > self.hangover
        {
            self.episode = None;
            emit(MatchEvent::Ended {
                start: ep.start,
                end: ep.last_end,
                cycles: ep.cycles,
            });
        }
    }

    /// Ends an episode in progress at the end of the stream.
    pub fn finish(&mut self, emit: &mut impl FnMut(MatchEvent)) {
        if let Some(ep) = self.episode.take() {
            emit(MatchEvent::Ended {
                start: ep.start,
                end: ep.last_end,
                cycles: ep.cycles,
            });
        }
        self.in_pulse = None;
        self.break_group();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(x: f32) -> u64 {
        (x * SAMPLE_RATE as f32) as u64
    }

    /// Feeds pulses given as (start, end) seconds, ticking every 16 ms up
    /// to `until` seconds.
    fn run(p: &Pattern, pulses: &[(f32, f32)], until: f32) -> Vec<MatchEvent> {
        let mut m = Matcher::new(p, 0.3, 0.04, 2, 1);
        let mut out = Vec::new();
        let mut emit = |e| out.push(e);
        let mut edges: Vec<(u64, bool)> = Vec::new();
        for &(a, b) in pulses {
            edges.push((s(a), true));
            edges.push((s(b), false));
        }
        let mut i = 0;
        let mut t = 0;
        while t <= s(until) {
            while i < edges.len() && edges[i].0 <= t {
                if edges[i].1 {
                    m.pulse_start(edges[i].0, &mut emit);
                } else {
                    m.pulse_end(edges[i].0);
                }
                i += 1;
            }
            m.tick(t, &mut emit);
            t += 256;
        }
        m.finish(&mut emit);
        out
    }

    fn t3(cycles: usize, at: f32) -> Vec<(f32, f32)> {
        let mut v = Vec::new();
        for c in 0..cycles {
            let base = at + c as f32 * 4.0;
            for k in 0..3 {
                let a = base + k as f32;
                v.push((a, a + 0.5));
            }
        }
        v
    }

    #[test]
    fn two_t3_cycles_start_an_episode() {
        let ev = run(&T3, &t3(2, 1.0), 30.0);
        assert_eq!(
            ev,
            vec![
                MatchEvent::Started {
                    start: s(1.0),
                    cycles: 2
                },
                MatchEvent::Ended {
                    start: s(1.0),
                    end: s(7.5),
                    cycles: 2
                }
            ]
        );
    }

    #[test]
    fn one_cycle_is_not_enough() {
        assert!(run(&T3, &t3(1, 1.0), 30.0).is_empty());
    }

    #[test]
    fn a_fourth_pulse_breaks_the_group() {
        let mut p = t3(4, 0.0);
        p.insert(3, (3.0, 3.5)); // a fourth pulse after the first group
        // It runs the first two groups together; the third and fourth still
        // make an episode.
        let ev = run(&T3, &p, 30.0);
        assert_eq!(
            ev[0],
            MatchEvent::Started {
                start: s(8.0),
                cycles: 2
            }
        );
    }

    #[test]
    fn one_missed_group_is_bridged() {
        let mut p = t3(6, 0.0);
        p.drain(9..12); // the fourth group is missing
        let ev = run(&T3, &p, 40.0);
        assert_eq!(ev.len(), 2);
        assert_eq!(
            ev[1],
            MatchEvent::Ended {
                start: 0,
                end: s(22.5),
                cycles: 5
            }
        );
    }

    #[test]
    fn continuous_beeping_never_matches() {
        let p: Vec<(f32, f32)> = (0..40).map(|k| (k as f32, k as f32 + 0.5)).collect();
        assert!(run(&T3, &p, 45.0).is_empty());
    }

    #[test]
    fn t4_cycles() {
        let mut p = Vec::new();
        for c in 0..3 {
            for k in 0..4 {
                let a = 0.5 + c as f32 * 5.7 + k as f32 * 0.2;
                p.push((a, a + 0.1));
            }
        }
        let ev = run(&T4, &p, 40.0);
        assert_eq!(
            ev[0],
            MatchEvent::Started {
                start: s(0.5),
                cycles: 2
            }
        );
        assert!(matches!(ev[1], MatchEvent::Ended { cycles: 3, .. }));
        assert!(run(&T3, &p, 40.0).is_empty());
    }
}
