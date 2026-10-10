//! Synthetic audio for the sound-events tests and bench: alarm patterns,
//! noise, degradations, and distractors, all generated from fixed seeds.
#![allow(dead_code)]

use sound_events::{AlarmConfig, AlarmDetector, AlarmEvent, AlarmKind, SAMPLE_RATE};
use std::f32::consts::PI;

pub const RATE: f32 = SAMPLE_RATE as f32;

/// A small deterministic generator (xorshift64*).
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Rng {
        Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1)
    }
    pub fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
    /// Uniform in [0, 1).
    pub fn uniform(&mut self) -> f32 {
        (self.next_u64() >> 40) as f32 / (1u64 << 24) as f32
    }
    /// Uniform in [-a, a].
    pub fn sym(&mut self, a: f32) -> f32 {
        (self.uniform() * 2.0 - 1.0) * a
    }
    /// Standard normal (Box-Muller).
    pub fn gauss(&mut self) -> f32 {
        let u1 = self.uniform().max(1e-7);
        let u2 = self.uniform();
        (-2.0 * u1.ln()).sqrt() * (2.0 * PI * u2).cos()
    }
}

pub fn secs(n: usize) -> f32 {
    n as f32 / RATE
}

pub fn samples(s: f32) -> usize {
    (s * RATE).round() as usize
}

/// An on/off rhythm: (on seconds, off seconds after it).
pub type Rhythm = Vec<(f32, f32)>;

/// `cycles` cycles of T3, each phase scaled by a random factor within
/// ±`jitter`.
pub fn t3(cycles: usize, jitter: f32, rng: &mut Rng) -> Rhythm {
    let mut r = Vec::new();
    for _ in 0..cycles {
        for k in 0..3 {
            let off = if k == 2 { 1.5 } else { 0.5 };
            r.push((0.5 * (1.0 + rng.sym(jitter)), off * (1.0 + rng.sym(jitter))));
        }
    }
    r
}

/// `cycles` cycles of T4, jittered like [`t3`].
pub fn t4(cycles: usize, jitter: f32, rng: &mut Rng) -> Rhythm {
    let mut r = Vec::new();
    for _ in 0..cycles {
        for k in 0..4 {
            let off = if k == 3 { 5.0 } else { 0.1 };
            r.push((0.1 * (1.0 + rng.sym(jitter)), off * (1.0 + rng.sym(jitter))));
        }
    }
    r
}

/// How a tone sounds.
#[derive(Clone, Copy, Debug)]
pub struct Voice {
    pub freq: f32,
    /// Peak amplitude of the fundamental (1.0 = full scale).
    pub amp: f32,
    /// Second harmonic's amplitude relative to the fundamental (a piezo
    /// horn is not a pure sine).
    pub harmonic: f32,
    /// Reverberation time (60 dB decay) of the tail after each pulse, s.
    pub rt60: f32,
    /// Slow frequency wobble, Hz peak.
    pub wobble_hz: f32,
}

impl Voice {
    pub fn pure(freq: f32, amp: f32) -> Voice {
        Voice {
            freq,
            amp,
            harmonic: 0.0,
            rt60: 0.0,
            wobble_hz: 0.0,
        }
    }
}

pub fn dbfs_amp(db: f32) -> f32 {
    10f32.powf(db / 20.0)
}

/// Renders a rhythm into `out` starting at `at` seconds (added to what is
/// there). Edges ramp over 3 ms.
pub fn render(out: &mut [f32], at: f32, rhythm: &Rhythm, v: Voice) {
    let ramp = 0.003;
    let mut t0 = at;
    let mut phase = 0.0f32;
    for &(on, off) in rhythm {
        let start = samples(t0);
        let tail = if v.rt60 > 0.0 { v.rt60 } else { ramp };
        let end = samples(t0 + on + tail).min(out.len());
        for (i, o) in out.iter_mut().enumerate().take(end).skip(start) {
            let t = secs(i) - t0;
            let env = if t < ramp {
                t / ramp
            } else if t < on {
                1.0
            } else if v.rt60 > 0.0 {
                // Direct sound stops; the room's tail decays from about 6 dB
                // below it.
                0.5 * (-6.9 * (t - on) / v.rt60).exp()
            } else {
                (1.0 - (t - on) / ramp).max(0.0)
            };
            let f = v.freq + v.wobble_hz * (2.0 * PI * 0.7 * secs(i)).sin();
            phase += 2.0 * PI * f / RATE;
            if phase > 2.0 * PI {
                phase -= 2.0 * PI;
            }
            let s = phase.sin() + v.harmonic * (2.0 * phase).sin();
            *o += v.amp * env * s;
        }
        t0 += on + off;
    }
}

/// Total length of a rhythm, seconds.
pub fn length(r: &Rhythm) -> f32 {
    r.iter().map(|(a, b)| a + b).sum()
}

pub fn white(n: usize, rms: f32, rng: &mut Rng) -> Vec<f32> {
    (0..n).map(|_| rng.gauss() * rms).collect()
}

/// Pink noise (Paul Kellet's filter on white noise), scaled to `rms`.
pub fn pink(n: usize, rms: f32, rng: &mut Rng) -> Vec<f32> {
    let (mut b0, mut b1, mut b2, mut b3, mut b4, mut b5, mut b6) =
        (0f32, 0f32, 0f32, 0f32, 0f32, 0f32, 0f32);
    let mut v: Vec<f32> = (0..n)
        .map(|_| {
            let w = rng.gauss();
            b0 = 0.99886 * b0 + w * 0.0555179;
            b1 = 0.99332 * b1 + w * 0.0750759;
            b2 = 0.96900 * b2 + w * 0.153_852;
            b3 = 0.86650 * b3 + w * 0.3104856;
            b4 = 0.55000 * b4 + w * 0.5329522;
            b5 = -0.7616 * b5 - w * 0.0168980;
            let out = b0 + b1 + b2 + b3 + b4 + b5 + b6 + w * 0.5362;
            b6 = w * 0.115926;
            out
        })
        .collect();
    scale_to_rms(&mut v, rms);
    v
}

pub fn rms(x: &[f32]) -> f32 {
    (x.iter().map(|s| s * s).sum::<f32>() / x.len().max(1) as f32).sqrt()
}

pub fn scale_to_rms(x: &mut [f32], target: f32) {
    let r = rms(x);
    if r > 0.0 {
        x.iter_mut().for_each(|s| *s *= target / r);
    }
}

pub fn add(into: &mut [f32], x: &[f32]) {
    into.iter_mut().zip(x).for_each(|(a, b)| *a += b);
}

/// An RBJ biquad.
pub struct Biquad {
    b: [f32; 3],
    a: [f32; 2],
    z: [f32; 2],
}

impl Biquad {
    pub fn lowpass(fc: f32, q: f32, rate: f32) -> Biquad {
        let w = 2.0 * PI * fc / rate;
        let alpha = w.sin() / (2.0 * q);
        let c = w.cos();
        let a0 = 1.0 + alpha;
        Biquad {
            b: [(1.0 - c) / 2.0 / a0, (1.0 - c) / a0, (1.0 - c) / 2.0 / a0],
            a: [-2.0 * c / a0, (1.0 - alpha) / a0],
            z: [0.0; 2],
        }
    }
    pub fn bandpass(fc: f32, q: f32, rate: f32) -> Biquad {
        let w = 2.0 * PI * fc / rate;
        let alpha = w.sin() / (2.0 * q);
        let c = w.cos();
        let a0 = 1.0 + alpha;
        Biquad {
            b: [alpha / a0, 0.0, -alpha / a0],
            a: [-2.0 * c / a0, (1.0 - alpha) / a0],
            z: [0.0; 2],
        }
    }
    pub fn run(&mut self, x: f32) -> f32 {
        let y = self.b[0] * x + self.z[0];
        self.z[0] = self.b[1] * x - self.a[0] * y + self.z[1];
        self.z[1] = self.b[2] * x - self.a[1] * y;
        y
    }
    pub fn apply(&mut self, x: &mut [f32]) {
        x.iter_mut().for_each(|s| *s = self.run(*s));
    }
}

/// Rounds to `bits`-bit integers and back, clipping at full scale.
pub fn quantize(x: &mut [f32], bits: u32) {
    let q = (1u32 << (bits - 1)) as f32;
    x.iter_mut()
        .for_each(|s| *s = (*s * q).round().clamp(-q, q - 1.0) / q);
}

/// A rough stand-in for low-bitrate AAC at 16 kHz: band-limited to about
/// 6 kHz, plus coding noise that follows the signal's level in 64 ms blocks
/// (`noise_db` below it), then 16-bit rounding.
pub fn aac_like(x: &mut [f32], noise_db: f32, rng: &mut Rng) {
    Biquad::lowpass(6000.0, 0.707, RATE).apply(x);
    Biquad::lowpass(6000.0, 0.707, RATE).apply(x);
    let k = dbfs_amp(noise_db);
    for block in x.chunks_mut(1024) {
        let r = rms(block);
        block.iter_mut().for_each(|s| *s += rng.gauss() * r * k);
    }
    quantize(x, 16);
}

/// Renders at 44.1 kHz and resamples to 16 kHz (two low-pass biquads at
/// 7 kHz, then linear interpolation), as a recording path might.
pub fn resampled_from_44k(total_s: f32, at: f32, rhythm: &Rhythm, v: Voice) -> Vec<f32> {
    let hi = 44_100.0f32;
    let n_hi = (total_s * hi) as usize;
    let mut x = vec![0.0f32; n_hi];
    // Render at the high rate by hand (render() assumes 16 kHz).
    let mut t0 = at;
    let mut phase = 0.0f32;
    for &(on, off) in rhythm {
        let a = (t0 * hi) as usize;
        let b = (((t0 + on) * hi) as usize).min(n_hi);
        for (i, o) in x.iter_mut().enumerate().take(b).skip(a) {
            let t = i as f32 / hi - t0;
            let env = (t / 0.003).min(1.0).min((on - t) / 0.003).max(0.0);
            phase += 2.0 * PI * v.freq / hi;
            if phase > 2.0 * PI {
                phase -= 2.0 * PI;
            }
            *o = v.amp * env * (phase.sin() + v.harmonic * (2.0 * phase).sin());
        }
        t0 += on + off;
    }
    Biquad::lowpass(7000.0, 0.707, hi).apply(&mut x);
    Biquad::lowpass(7000.0, 0.707, hi).apply(&mut x);
    let n = (total_s * RATE) as usize;
    (0..n)
        .map(|i| {
            let p = i as f32 * hi / RATE;
            let j = p as usize;
            let f = p - j as f32;
            let a = x.get(j).copied().unwrap_or(0.0);
            let b = x.get(j + 1).copied().unwrap_or(0.0);
            a + (b - a) * f
        })
        .collect()
}

/// Noise shaped like speech: band-limited to 300-3400 Hz, with a syllable
/// envelope (3 to 6 per second) and pauses.
pub fn speech_like(n: usize, rms_level: f32, rng: &mut Rng) -> Vec<f32> {
    let mut x = white(n, 1.0, rng);
    Biquad::bandpass(700.0, 0.6, RATE).apply(&mut x);
    let mut lp = Biquad::lowpass(3400.0, 0.707, RATE);
    lp.apply(&mut x);
    let mut i = 0;
    while i < n {
        let syl = samples(0.08 + rng.uniform() * 0.25);
        let gain = if rng.uniform() < 0.2 {
            0.0
        } else {
            0.3 + rng.uniform()
        };
        for k in 0..syl.min(n - i) {
            let e = (PI * k as f32 / syl as f32).sin();
            x[i + k] *= gain * e;
        }
        i += syl;
    }
    scale_to_rms(&mut x, rms_level);
    x
}

/// Music-like chords: three notes with six harmonics each, changing every
/// half second, in a steady rhythm.
pub fn chords(n: usize, amp: f32, rng: &mut Rng) -> Vec<f32> {
    let roots = [
        220.0f32, 246.9, 261.6, 293.7, 329.6, 349.2, 392.0, 440.0, 523.3,
    ];
    let mut x = vec![0.0f32; n];
    let step = samples(0.5);
    let mut i = 0;
    while i < n {
        let r = roots[(rng.next_u64() % roots.len() as u64) as usize];
        let notes = [r, r * 1.26, r * 1.498, r * 2.0];
        for k in 0..step.min(n - i) {
            let t = secs(i + k);
            let env = (-3.0 * secs(k)).exp();
            let mut s = 0.0;
            for f in notes {
                for h in 1..=6 {
                    let fh = f * h as f32;
                    if fh < 7500.0 {
                        s += (2.0 * PI * fh * t).sin() / h as f32;
                    }
                }
            }
            x[i + k] = amp * env * s / 8.0;
        }
        i += step;
    }
    x
}

/// Runs the detector over `x` in blocks of `block` samples and returns all
/// events, the stream's end included.
pub fn detect_with(cfg: AlarmConfig, x: &[f32], block: usize) -> Vec<AlarmEvent> {
    let mut d = AlarmDetector::new(cfg);
    let mut out = Vec::new();
    for (k, chunk) in x.chunks(block).enumerate() {
        d.push((k * block) as u64, chunk, |e| out.push(e));
    }
    d.flush(|e| out.push(e));
    out
}

pub fn detect(x: &[f32]) -> Vec<AlarmEvent> {
    detect_with(AlarmConfig::default(), x, 1024)
}

/// Kinds of the episodes that started.
pub fn started(ev: &[AlarmEvent]) -> Vec<AlarmKind> {
    ev.iter()
        .filter_map(|e| match e {
            AlarmEvent::Started { kind, .. } => Some(*kind),
            _ => None,
        })
        .collect()
}

/// A labeled test signal.
pub struct Case {
    pub set: &'static str,
    pub name: String,
    pub signal: Vec<f32>,
    pub expect: Option<AlarmKind>,
}

/// Outcome of one case.
pub struct Outcome {
    pub detected: bool,
    pub false_alarms: usize,
}

pub fn evaluate(c: &Case) -> Outcome {
    let kinds = started(&detect(&c.signal));
    let detected = c.expect.is_some_and(|k| kinds.contains(&k));
    let false_alarms = kinds.iter().filter(|&&k| Some(k) != c.expect).count();
    Outcome {
        detected,
        false_alarms,
    }
}

fn alarm(kind: AlarmKind, cycles: usize, jitter: f32, seed: u64) -> (Rhythm, f32) {
    let mut rng = Rng::new(seed);
    let r = match kind {
        AlarmKind::Smoke => t3(cycles, jitter, &mut rng),
        AlarmKind::Co => t4(cycles, jitter, &mut rng),
    };
    let len = length(&r);
    (r, len)
}

fn kind_name(k: AlarmKind) -> &'static str {
    match k {
        AlarmKind::Smoke => "T3",
        AlarmKind::Co => "T4",
    }
}

/// Clean alarms at several levels and tone frequencies.
pub fn clean_set() -> Vec<Case> {
    let mut v = Vec::new();
    for kind in [AlarmKind::Smoke, AlarmKind::Co] {
        for (i, db) in [-6.0f32, -30.0, -50.0, -60.0].into_iter().enumerate() {
            for (j, freq) in [2950.0f32, 3100.0, 3250.0, 3450.0].into_iter().enumerate() {
                let (r, len) = alarm(kind, 4, 0.0, (i * 10 + j) as u64);
                let mut x = vec![0.0; samples(len + 3.0)];
                render(&mut x, 1.0, &r, Voice::pure(freq, dbfs_amp(db)));
                v.push(Case {
                    set: "clean",
                    name: format!("{} {db} dBFS {freq} Hz", kind_name(kind)),
                    signal: x,
                    expect: Some(kind),
                });
            }
        }
    }
    v
}

/// Alarms at -30 dBFS with white or pink noise at a tone-to-noise ratio
/// (tone RMS during a pulse over noise RMS, broadband 0-8 kHz).
pub fn noise_set(snrs: &[f32]) -> Vec<Case> {
    let mut v = Vec::new();
    for kind in [AlarmKind::Smoke, AlarmKind::Co] {
        for &snr in snrs {
            for pinkish in [false, true] {
                for seed in 0..3u64 {
                    let (r, len) = alarm(kind, 4, 0.05, 100 + seed);
                    let amp = dbfs_amp(-30.0);
                    let n = samples(len + 3.0);
                    let mut x = vec![0.0; n];
                    let voice = Voice {
                        harmonic: 0.2,
                        ..Voice::pure(3100.0 + 50.0 * seed as f32, amp)
                    };
                    render(&mut x, 1.0, &r, voice);
                    let noise_rms = amp / 2f32.sqrt() / dbfs_amp(snr);
                    let mut rng = Rng::new(7 + seed);
                    let noise = if pinkish {
                        pink(n, noise_rms, &mut rng)
                    } else {
                        white(n, noise_rms, &mut rng)
                    };
                    add(&mut x, &noise);
                    v.push(Case {
                        set: if pinkish { "pink noise" } else { "white noise" },
                        name: format!("{} SNR {snr} dB seed {seed}", kind_name(kind)),
                        signal: x,
                        expect: Some(kind),
                    });
                }
            }
        }
    }
    v
}

/// Degradations: AAC-like coding, resampling, reverberation, timing jitter.
pub fn degraded_set() -> Vec<Case> {
    let mut v = Vec::new();
    for kind in [AlarmKind::Smoke, AlarmKind::Co] {
        let k = kind_name(kind);
        for seed in 0..3u64 {
            let (r, len) = alarm(kind, 4, 0.05, 200 + seed);
            let n = samples(len + 3.0);
            let mut rng = Rng::new(300 + seed);
            // AAC-like: coding noise 20 dB under the signal, with room noise.
            let mut x = vec![0.0; n];
            render(
                &mut x,
                1.0,
                &r,
                Voice {
                    harmonic: 0.3,
                    ..Voice::pure(3200.0, dbfs_amp(-25.0))
                },
            );
            add(&mut x, &pink(n, dbfs_amp(-55.0), &mut rng));
            aac_like(&mut x, -20.0, &mut rng);
            v.push(Case {
                set: "aac-like",
                name: format!("{k} seed {seed}"),
                signal: x,
                expect: Some(kind),
            });

            let x = resampled_from_44k(
                len + 3.0,
                1.0,
                &r,
                Voice::pure(3050.0 + 100.0 * seed as f32, dbfs_amp(-20.0)),
            );
            v.push(Case {
                set: "resampled 44.1k",
                name: format!("{k} seed {seed}"),
                signal: x,
                expect: Some(kind),
            });

            let rt60 = [0.2f32, 0.4, 0.6][seed as usize];
            let mut x = vec![0.0; n + samples(1.0)];
            render(
                &mut x,
                1.0,
                &r,
                Voice {
                    rt60,
                    wobble_hz: 20.0,
                    ..Voice::pure(3300.0, dbfs_amp(-30.0))
                },
            );
            let room = white(x.len(), dbfs_amp(-70.0), &mut rng);
            add(&mut x, &room);
            v.push(Case {
                set: "reverb",
                name: format!("{k} RT60 {rt60} s"),
                signal: x,
                expect: Some(kind),
            });

            for jitter in [0.1f32, 0.2] {
                let (r, len) = alarm(kind, 4, jitter, 400 + seed);
                let mut x = vec![0.0; samples(len + 3.0)];
                render(&mut x, 1.0, &r, Voice::pure(3150.0, dbfs_amp(-30.0)));
                v.push(Case {
                    set: "jitter",
                    name: format!("{k} ±{}% seed {seed}", jitter * 100.0),
                    signal: x,
                    expect: Some(kind),
                });
            }
        }
    }
    v
}

fn repeat(pattern: &[(f32, f32)], times: usize) -> Rhythm {
    pattern
        .iter()
        .copied()
        .cycle()
        .take(pattern.len() * times)
        .collect()
}

/// Sounds that must not be reported.
pub fn distractor_set() -> Vec<Case> {
    let mut v = Vec::new();
    let amp = dbfs_amp(-20.0);
    let mut push = |name: &str, signal: Vec<f32>| {
        v.push(Case {
            set: "distractors",
            name: name.to_string(),
            signal,
            expect: None,
        });
    };
    let n = samples(40.0);
    let tone = |rhythm: &Rhythm, freq: f32| {
        let mut x = vec![0.0; samples(length(rhythm) + 2.0)];
        render(&mut x, 1.0, rhythm, Voice::pure(freq, amp));
        x
    };
    push("continuous 3.1 kHz tone", tone(&vec![(35.0, 0.0)], 3100.0));
    push("single beep", tone(&vec![(0.5, 0.0)], 3100.0));
    let mut rng = Rng::new(1);
    push("one T3 group", tone(&t3(1, 0.0, &mut rng), 3100.0));
    push("one T4 group", tone(&t4(1, 0.0, &mut rng), 3100.0));
    push(
        "ring, 0.4/0.2/0.4/2.0 cadence",
        tone(&repeat(&[(0.4, 0.2), (0.4, 2.0)], 12), 3100.0),
    );
    push(
        "ring, 2 s on 4 s off",
        tone(&repeat(&[(2.0, 4.0)], 6), 3100.0),
    );
    push(
        "ring, 1 s on 2 s off",
        tone(&repeat(&[(1.0, 2.0)], 12), 3100.0),
    );
    push(
        "microwave, 5 beeps",
        tone(&repeat(&[(0.3, 0.3)], 5), 2950.0),
    );
    push(
        "microwave, 3 beeps every 20 s",
        tone(&repeat(&[(0.5, 0.5), (0.5, 0.5), (0.5, 18.5)], 3), 3000.0),
    );
    push(
        "backup beeper 0.5/0.5",
        tone(&repeat(&[(0.5, 0.5)], 40), 3100.0),
    );
    push(
        "fast beeper 0.1/0.1",
        tone(&repeat(&[(0.1, 0.1)], 150), 3100.0),
    );
    push("T3 at 520 Hz", tone(&t3(6, 0.0, &mut rng), 520.0));
    push("T3 at 1 kHz", tone(&t3(6, 0.0, &mut rng), 1000.0));
    push("T3 at 4.5 kHz", tone(&t3(6, 0.0, &mut rng), 4500.0));
    push("T4 at 1 kHz", tone(&t4(5, 0.0, &mut rng), 1000.0));
    // Broadband noise bursts in the T3 rhythm.
    let r = t3(6, 0.0, &mut rng);
    let mut x = vec![0.0; samples(length(&r) + 2.0)];
    let mut t0 = 1.0;
    for &(on, off) in &r {
        for s in &mut x[samples(t0)..samples(t0 + on)] {
            *s = rng.gauss() * 0.1;
        }
        t0 += on + off;
    }
    push("white noise bursts in T3 rhythm", x);
    for q in [1.0f32, 2.0, 4.0] {
        let r = t3(6, 0.0, &mut rng);
        let mut x = vec![0.0; samples(length(&r) + 2.0)];
        let mut t0 = 1.0;
        let mut noise = white(x.len(), 1.0, &mut rng);
        Biquad::bandpass(3100.0, q, RATE).apply(&mut noise);
        scale_to_rms(&mut noise, 0.1);
        for &(on, off) in &r {
            let (a, b) = (samples(t0), samples(t0 + on));
            x[a..b].copy_from_slice(&noise[a..b]);
            t0 += on + off;
        }
        push(
            &format!("band-limited noise (3.1 kHz, Q {q}) bursts in T3 rhythm"),
            x,
        );
    }
    push("loud pink noise", pink(n, 0.2, &mut rng));
    push("loud white noise", white(n, 0.2, &mut rng));
    push("speech-like", speech_like(n, 0.1, &mut rng));
    push("music-like chords", chords(n, 0.3, &mut rng));
    let mut x = speech_like(n, 0.05, &mut rng);
    add(&mut x, &chords(n, 0.2, &mut rng));
    push("speech over chords", x);
    push("silence", vec![0.0; n]);
    v
}
