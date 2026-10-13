//! Picture acquisition: a woken camera's main stream, or its low-quality one
//! for the re-checks of an alerted camera (shared with any tile
//! already showing it, since the session runs one preview per channel and
//! tier), the access units of the current GOP, and the pictures the
//! service asks for, handed over as frame bundles (a keyframe and the
//! access units up to the picture to check). Bounded: a GOP longer than
//! [`MAX_GOP_AUS`] or [`MAX_GOP_BYTES`] is dropped until the next keyframe.

use bc_protocol::media::{Codec as MediaCodec, Record};
use bc_protocol::stream::StreamTier;
use bc_session::{DeviceSession, Item};
use std::sync::Arc;
use video_pipeline::nal::analyze;
use video_pipeline::{Codec, ParameterSets, VideoFormat};

use super::{AlertFrames, Inner};
use alerts::model::CameraId;

/// Most access units kept from one keyframe.
pub(crate) const MAX_GOP_AUS: usize = 300;
/// Most bytes kept from one keyframe.
pub(crate) const MAX_GOP_BYTES: usize = 24 << 20;

/// One access unit.
#[derive(Clone, Debug)]
pub(crate) struct Au {
    pub(crate) pts_us: u64,
    pub(crate) keyframe: bool,
    pub(crate) data: Arc<[u8]>,
}

/// The access units of one picture to check: a keyframe first, the picture
/// last.
#[derive(Clone, Debug)]
pub(crate) struct Bundle {
    pub(crate) codec: Codec,
    pub(crate) width: u32,
    pub(crate) height: u32,
    /// Parameter sets (Annex-B) to put before the keyframe when it has
    /// none of its own; often empty.
    pub(crate) prefix: Arc<[u8]>,
    pub(crate) aus: Vec<Au>,
    /// A cover image from the recording, not the live stream.
    pub(crate) from_cover: bool,
}

impl Bundle {
    pub(crate) fn bytes(&self) -> usize {
        self.prefix.len() + self.aus.iter().map(|a| a.data.len()).sum::<usize>()
    }

    pub(crate) fn pts_us(&self) -> u64 {
        self.aus.last().map_or(0, |a| a.pts_us)
    }

    /// The bytes for the app: the prefix belongs to the first access unit.
    pub(crate) fn frames(&self) -> AlertFrames {
        let mut data = Vec::with_capacity(self.bytes());
        let mut sizes = Vec::with_capacity(self.aus.len());
        for (i, a) in self.aus.iter().enumerate() {
            let before = data.len();
            if i == 0 {
                data.extend_from_slice(&self.prefix);
            }
            data.extend_from_slice(&a.data);
            sizes.push(data.len() - before);
        }
        AlertFrames {
            codec: self.codec,
            width: self.width,
            height: self.height,
            pts_us: self.pts_us(),
            sizes,
            data,
        }
    }

    /// A bundle of one keyframe (a cover image).
    pub(crate) fn single(
        codec: Codec,
        width: u32,
        height: u32,
        pts_us: u64,
        data: Vec<u8>,
    ) -> Self {
        let (width, height) = size_or_sps(codec, width, height, &data);
        Bundle {
            codec,
            width,
            height,
            prefix: Arc::from(Vec::new()),
            aus: vec![Au {
                pts_us,
                keyframe: true,
                data: Arc::from(data),
            }],
            from_cover: true,
        }
    }
}

pub(crate) fn codec(c: MediaCodec) -> Codec {
    match c {
        MediaCodec::H264 => Codec::H264,
        MediaCodec::H265 => Codec::H265,
    }
}

/// The stream of a camera angle: lens 0 is main, a second lens is the
/// `extern` stream (its main quality).
pub(crate) fn tier_of(camera: CameraId) -> StreamTier {
    if camera.lens == 0 {
        StreamTier::Main
    } else {
        StreamTier::Extern
    }
}

/// The stream for the re-checks of an alerted camera: the low-quality one
/// (`sub`; `mobile` for a second lens), which is cheaper to decode and to
/// carry for a "still present" look.
pub(crate) fn recheck_tier_of(camera: CameraId) -> StreamTier {
    if camera.lens == 0 {
        StreamTier::Sub
    } else {
        StreamTier::Mobile
    }
}

/// The picture size, from the stream info, else from the SPS.
fn size_or_sps(codec: Codec, width: u32, height: u32, au: &[u8]) -> (u32, u32) {
    if width > 0 && height > 0 {
        return (width, height);
    }
    let sets = ParameterSets::from_access_unit(codec, au);
    VideoFormat::from_parameter_sets(sets).map_or((width, height), |f| {
        (f.visible_rect.width, f.visible_rect.height)
    })
}

fn annex_b(sets: &ParameterSets) -> Vec<u8> {
    let mut out = Vec::new();
    for s in sets {
        out.extend_from_slice(&[0, 0, 0, 1]);
        out.extend_from_slice(&s.data);
    }
    out
}

/// What the service wants from a woken stream next.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Want {
    Nothing,
    /// The next keyframe, alone.
    Keyframe,
    /// The first picture at or after `target` (stream time), decoded
    /// forward from its keyframe. `base` is the previous picture's time:
    /// a stream that jumped back before it (a reconnect) counts as there.
    AtOrAfter {
        target: u64,
        base: u64,
    },
}

/// What happened to a woken stream.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Note {
    /// The first video arrived.
    FirstVideo,
    /// The device refused the stream.
    Refused,
    /// The session ended for good.
    Ended,
}

/// Reads one woken camera's stream until the task is aborted (which drops
/// the subscription and so stops the preview, unless a tile shows it).
pub(crate) async fn run_wake(
    inner: Arc<Inner>,
    session: Arc<DeviceSession>,
    camera: CameraId,
    generation: u64,
    tier: StreamTier,
) {
    let Ok(channel) = u8::try_from(camera.channel) else {
        inner.wake_note(camera, generation, Note::Refused);
        return;
    };
    let Ok(mut sub) = session.subscribe(channel, tier).await else {
        inner.wake_note(camera, generation, Note::Ended);
        return;
    };
    let mut gop: Vec<Au> = Vec::new();
    let mut gop_bytes = 0usize;
    let mut gop_ok = false;
    let (mut width, mut height) = (0u32, 0u32);
    let mut sets: Option<ParameterSets> = None;
    let mut seen_video = false;
    while let Some(item) = sub.recv().await {
        match item {
            Item::Media(Record::Info {
                width: w,
                height: h,
                ..
            }) => {
                if w > 0 && h > 0 {
                    (width, height) = (w, h);
                }
            }
            Item::Media(Record::Video {
                codec: c,
                keyframe,
                pts_us,
                data,
                ..
            }) => {
                let c = codec(c);
                if keyframe {
                    gop.clear();
                    gop_bytes = 0;
                    gop_ok = true;
                    let found = ParameterSets::from_access_unit(c, &data);
                    if !found.is_empty() {
                        if width == 0 || height == 0 {
                            (width, height) = size_or_sps(c, width, height, &data);
                        }
                        sets = Some(found);
                    }
                }
                if !gop_ok {
                    continue;
                }
                if gop.len() >= MAX_GOP_AUS || gop_bytes + data.len() > MAX_GOP_BYTES {
                    gop.clear();
                    gop_bytes = 0;
                    gop_ok = false;
                    continue;
                }
                gop_bytes += data.len();
                gop.push(Au {
                    pts_us,
                    keyframe,
                    data: Arc::from(data),
                });
                if !seen_video {
                    seen_video = true;
                    inner.wake_note(camera, generation, Note::FirstVideo);
                }
                let take = match inner.wake_want(camera, generation) {
                    Want::Nothing => None,
                    Want::Keyframe => keyframe.then(|| gop.len() - 1..gop.len()),
                    Want::AtOrAfter { target, base } => {
                        (pts_us >= target || pts_us < base).then_some(0..gop.len())
                    }
                };
                if let Some(range) = take {
                    let aus = gop[range].to_vec();
                    let has_sets = aus
                        .first()
                        .is_some_and(|a| analyze(c, &a.data).parameter_sets > 0);
                    let prefix = match (&sets, has_sets) {
                        (Some(s), false) if s.codec() == c => annex_b(s),
                        _ => Vec::new(),
                    };
                    let bundle = Bundle {
                        codec: c,
                        width,
                        height,
                        prefix: Arc::from(prefix),
                        aus,
                        from_cover: false,
                    };
                    inner.wake_picture(camera, generation, bundle);
                }
            }
            Item::Media(Record::Audio { .. }) | Item::WaitingForDevice { .. } => {}
            Item::Discontinuity => {
                gop.clear();
                gop_bytes = 0;
                gop_ok = false;
            }
            Item::StartRejected { .. } => inner.wake_note(camera, generation, Note::Refused),
        }
    }
    inner.wake_note(camera, generation, Note::Ended);
}
