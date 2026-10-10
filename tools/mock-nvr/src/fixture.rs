//! Annex-B fixture loading: split an elementary stream into access units.

use bc_protocol::media::Codec;
use std::fmt;
use std::path::Path;

/// One access unit (a whole picture) of the fixture.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Frame {
    /// True when the picture is a keyframe (IDR for H.264; IRAP for H.265).
    pub keyframe: bool,
    /// Annex-B bytes, including parameter sets that precede the picture.
    pub data: Vec<u8>,
}

/// Why a fixture could not be used.
#[derive(Debug)]
pub enum FixtureError {
    /// The file could not be read.
    Io(std::io::Error),
    /// The file extension does not name a codec.
    UnknownCodec,
    /// The stream holds no complete keyframe.
    NoKeyframe,
}

impl fmt::Display for FixtureError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            FixtureError::Io(e) => write!(f, "cannot read fixture: {e}"),
            FixtureError::UnknownCodec => {
                write!(f, "cannot tell the codec; use .h264, .h265, or .hevc")
            }
            FixtureError::NoKeyframe => write!(f, "fixture contains no keyframe"),
        }
    }
}

impl std::error::Error for FixtureError {}

/// A looping video source.
///
/// Playback starts at the first keyframe, so frames before it (such as the
/// undecodable leading predicted frames of some fixtures) are never sent, and
/// loops back to that keyframe at the end.
#[derive(Clone, Debug)]
pub struct Fixture {
    codec: Codec,
    frames: Vec<Frame>,
    first_key: usize,
}

impl Fixture {
    /// Loads a `.h264`, `.h265`, or `.hevc` Annex-B file.
    pub fn load(path: &Path) -> Result<Fixture, FixtureError> {
        let codec = match path.extension().and_then(|e| e.to_str()) {
            Some("h264") => Codec::H264,
            Some("h265" | "hevc") => Codec::H265,
            _ => return Err(FixtureError::UnknownCodec),
        };
        let bytes = std::fs::read(path).map_err(FixtureError::Io)?;
        Fixture::from_annex_b(codec, &bytes)
    }

    /// Splits `bytes` (an Annex-B stream of `codec`) into access units.
    pub fn from_annex_b(codec: Codec, bytes: &[u8]) -> Result<Fixture, FixtureError> {
        let frames = split_access_units(codec, bytes);
        let first_key = frames
            .iter()
            .position(|f| f.keyframe)
            .ok_or(FixtureError::NoKeyframe)?;
        Ok(Fixture {
            codec,
            frames,
            first_key,
        })
    }

    /// The codec of the stream.
    pub fn codec(&self) -> Codec {
        self.codec
    }

    /// All access units, including any before the first keyframe.
    pub fn frames(&self) -> &[Frame] {
        &self.frames
    }

    /// Index of the first keyframe, where playback starts and loops to.
    pub fn first_keyframe(&self) -> usize {
        self.first_key
    }

    /// Index of the frame after `index` in playback order.
    pub(crate) fn next_index(&self, index: usize) -> usize {
        if index + 1 >= self.frames.len() {
            self.first_key
        } else {
            index + 1
        }
    }
}

struct Nal {
    start: usize,
    end: usize,
    vcl: bool,
    first_slice: bool,
    keyframe: bool,
}

fn split_nals(codec: Codec, b: &[u8]) -> Vec<Nal> {
    // Positions of every 00 00 01 start code; a preceding zero byte belongs to
    // the start code.
    let mut codes = Vec::new();
    let mut i = 0;
    while i + 3 <= b.len() {
        if b[i] == 0 && b[i + 1] == 0 && b[i + 2] == 1 {
            let begin = if i > 0 && b[i - 1] == 0 { i - 1 } else { i };
            codes.push((begin, i + 3));
            i += 3;
        } else {
            i += 1;
        }
    }
    let mut nals = Vec::new();
    for (n, &(begin, payload)) in codes.iter().enumerate() {
        let end = codes.get(n + 1).map_or(b.len(), |c| c.0);
        let hdr = &b[payload..end];
        let (vcl, first_slice, keyframe) = match codec {
            Codec::H264 => match hdr.first() {
                Some(&h) => {
                    let t = h & 0x1f;
                    // first_mb_in_slice == 0 is the single bit '1'.
                    (
                        (1..=5).contains(&t),
                        hdr.get(1).is_some_and(|x| x & 0x80 != 0),
                        t == 5,
                    )
                }
                None => continue,
            },
            Codec::H265 => match hdr.first() {
                Some(&h) => {
                    let t = (h >> 1) & 0x3f;
                    (
                        t < 32,
                        hdr.get(2).is_some_and(|x| x & 0x80 != 0),
                        (16..=21).contains(&t),
                    )
                }
                None => continue,
            },
        };
        nals.push(Nal {
            start: begin,
            end,
            vcl,
            first_slice,
            keyframe,
        });
    }
    nals
}

fn split_access_units(codec: Codec, b: &[u8]) -> Vec<Frame> {
    let nals = split_nals(codec, b);
    let mut frames = Vec::new();
    // Current access unit: NAL index range, whether it has a picture, where
    // its last picture NAL ends (trailing parameter sets belong to the next).
    let mut begin = 0usize;
    let mut has_vcl = false;
    let mut last_vcl = 0usize;
    let mut key = false;
    let emit = |from: usize, to: usize, key: bool, frames: &mut Vec<Frame>| {
        if from < to {
            frames.push(Frame {
                keyframe: key,
                data: b[nals[from].start..nals[to - 1].end].to_vec(),
            });
        }
    };
    for (i, n) in nals.iter().enumerate() {
        if n.vcl && n.first_slice && has_vcl {
            emit(begin, last_vcl + 1, key, &mut frames);
            begin = last_vcl + 1;
            has_vcl = false;
            key = false;
        }
        if n.vcl {
            has_vcl = true;
            last_vcl = i;
            key |= n.keyframe;
        }
    }
    if has_vcl {
        emit(begin, last_vcl + 1, key, &mut frames);
    }
    frames
}

#[cfg(test)]
mod tests {
    use super::*;

    fn h264_nal(t: u8, first: bool) -> Vec<u8> {
        vec![0, 0, 0, 1, 0x60 | t, if first { 0x80 } else { 0x00 }, 0xaa]
    }

    #[test]
    fn splits_h264_and_loops_from_first_keyframe() {
        let mut s = Vec::new();
        s.extend(h264_nal(1, true)); // leading P
        s.extend(h264_nal(7, true)); // SPS
        s.extend(h264_nal(8, true)); // PPS
        s.extend(h264_nal(5, true)); // IDR
        s.extend(h264_nal(5, false)); // second slice of the IDR
        s.extend(h264_nal(1, true)); // P
        let f = Fixture::from_annex_b(Codec::H264, &s).unwrap();
        assert_eq!(f.frames().len(), 3);
        assert_eq!(f.first_keyframe(), 1);
        assert!(f.frames()[1].keyframe && !f.frames()[2].keyframe);
        assert_eq!(f.frames()[1].data.len(), 4 * 7);
        assert_eq!(f.next_index(2), 1);
        assert_eq!(f.next_index(1), 2);
    }

    #[test]
    fn no_keyframe_is_an_error() {
        let s = h264_nal(1, true);
        assert!(matches!(
            Fixture::from_annex_b(Codec::H264, &s),
            Err(FixtureError::NoKeyframe)
        ));
    }
}
