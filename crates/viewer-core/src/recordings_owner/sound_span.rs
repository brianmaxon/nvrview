//! What a span-limited analysis needs (split from [`super::sound_work`]):
//! where measuring found no sound, and cutting a source's chunks to a span.

use super::sound_work::AudioChunk;
use recordings::Secs;
use std::collections::HashMap;

/// Most no-audio spans kept per camera.
const MAX_NO_AUDIO_SPANS: usize = 256;

/// Where the core has measured that a camera's recordings carry no sound,
/// so a gap in the loudness lane can say "no audio" instead of guessing.
/// In memory only: it is re-learned by the next listen or analysis.
#[derive(Default)]
pub(super) struct NoAudio {
    /// Cameras whose listener was refused with `no_audio` (30 s of video
    /// with no audio on the live stream), by (device key, channel).
    cameras: std::collections::HashSet<(String, u8)>,
    /// Spans of a camera's finished downloads that an analysis read and
    /// found without any audio, device-local seconds, by (device key, channel).
    spans: HashMap<(String, u8), Vec<(Secs, Secs)>>,
}

impl NoAudio {
    /// The live stream of the camera had video without audio.
    pub(super) fn camera_lacks(&mut self, key: &str, channel: u8) {
        self.cameras.insert((key.to_owned(), channel));
    }

    /// Audio of the camera was heard: it has a microphone after all.
    pub(super) fn camera_heard(&mut self, key: &str, channel: u8) {
        if !self.cameras.is_empty() {
            self.cameras.remove(&(key.to_owned(), channel));
        }
    }

    /// A downloaded span of the camera was read and has no audio.
    pub(super) fn span_lacks(&mut self, key: &str, channel: u8, span: (Secs, Secs)) {
        let v = self.spans.entry((key.to_owned(), channel)).or_default();
        if !v.contains(&span) {
            if v.len() >= MAX_NO_AUDIO_SPANS {
                v.remove(0);
            }
            v.push(span);
        }
    }

    /// Whether the whole camera is known to have no sound, and the spans
    /// known to have none.
    pub(super) fn of(&self, key: &str, channel: u8) -> (bool, Vec<(Secs, Secs)>) {
        let k = (key.to_owned(), channel);
        (
            self.cameras.contains(&k),
            self.spans.get(&k).cloned().unwrap_or_default(),
        )
    }
}

/// The part of `chunks` inside the samples `lo..hi`, cut at the edges; the
/// source is not read past `hi`.
pub(super) fn clip_chunks(
    chunks: impl Iterator<Item = AudioChunk> + Send,
    lo: u64,
    hi: u64,
) -> impl Iterator<Item = AudioChunk> + Send {
    chunks
        .take_while(move |c| c.first_sample < hi)
        .filter_map(move |c| {
            let end = c.first_sample + c.samples.len() as u64;
            if end <= lo {
                return None;
            }
            let from = lo.saturating_sub(c.first_sample) as usize;
            let to = (hi.min(end) - c.first_sample) as usize;
            Some(AudioChunk {
                first_sample: c.first_sample + from as u64,
                samples: c.samples[from..to].to_vec(),
            })
        })
}
