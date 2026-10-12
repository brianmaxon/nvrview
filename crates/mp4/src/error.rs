//! Error type for the MP4 writer.

use std::fmt;

/// Everything that can go wrong while muxing.
#[derive(Debug)]
pub enum Error {
    /// The output sink failed.
    Io(std::io::Error),
    /// The access unit had no Annex-B start code.
    InvalidAnnexB,
    /// No SPS/PPS (and, for H.265, no usable parameter sets) was seen before
    /// the first video sample.
    MissingParameterSets,
    /// A parameter set could not be parsed.
    InvalidParameterSet(&'static str),
    /// A video decode timestamp did not increase, or a presentation time was
    /// earlier than its decode time.
    NonMonotonicTimestamp,
    /// An audio frame was not a single valid ADTS frame.
    InvalidAdts(&'static str),
    /// A raw AudioSpecificConfig this crate cannot carry.
    InvalidAudioConfig(&'static str),
    /// An audio track index that was not declared, more audio tracks than
    /// [`MAX_AUDIO_TRACKS`](crate::MAX_AUDIO_TRACKS), or an empty or
    /// oversized raw access unit.
    InvalidTrack(&'static str),
    /// The audio format (profile, rate, channels) changed mid-stream.
    AudioConfigChanged,
    /// An audio gap needs silence, which this writer can only generate for
    /// mono AAC-LC.
    UnsupportedAudioGap,
    /// `finish` was called before any video sample was written.
    NoVideo,
    /// The video parameter sets changed and the caller asked for that to be
    /// an error (see `ChangePolicy::Reject`). Nothing was consumed.
    ParameterSetsChanged,
    /// A fragmented file could not be read back (not written by this crate,
    /// or damaged before its first complete fragment).
    InvalidFragmentedFile(&'static str),
    /// A sample or table exceeded what the format can express.
    TooLarge,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Io(e) => write!(f, "i/o error: {e}"),
            Error::InvalidAnnexB => write!(f, "access unit has no Annex-B start code"),
            Error::MissingParameterSets => write!(f, "no parameter sets before first sample"),
            Error::InvalidParameterSet(why) => write!(f, "invalid parameter set: {why}"),
            Error::NonMonotonicTimestamp => write!(f, "video timestamps must increase"),
            Error::InvalidAdts(why) => write!(f, "invalid ADTS frame: {why}"),
            Error::InvalidAudioConfig(why) => write!(f, "invalid AudioSpecificConfig: {why}"),
            Error::InvalidTrack(why) => write!(f, "invalid audio track: {why}"),
            Error::AudioConfigChanged => write!(f, "audio configuration changed mid-stream"),
            Error::UnsupportedAudioGap => write!(f, "audio gap in a format without silence frames"),
            Error::NoVideo => write!(f, "no video samples were written"),
            Error::ParameterSetsChanged => write!(f, "video parameter sets changed"),
            Error::InvalidFragmentedFile(why) => write!(f, "invalid fragmented file: {why}"),
            Error::TooLarge => write!(f, "value too large for MP4"),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Error::Io(e) => Some(e),
            _ => None,
        }
    }
}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Error::Io(e)
    }
}

/// Result alias for this crate.
pub type Result<T> = std::result::Result<T, Error>;
