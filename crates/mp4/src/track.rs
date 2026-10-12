//! Audio track declarations shared by the fragmenter, the fragmented file,
//! and the writer.

use crate::adts::AacConfig;

/// Most audio tracks one file or stream may declare.
pub const MAX_AUDIO_TRACKS: usize = 8;

/// Name in the video track's handler box.
pub const VIDEO_HANDLER_NAME: &str = "VideoHandler";

/// Every handler name this crate ever writes: generic words that name no
/// product, device, person, or installation. Anything that checks a file
/// for identifying metadata can rely on this list.
pub const HANDLER_NAMES: [&str; 5] = [
    VIDEO_HANDLER_NAME,
    AudioHandler::Sound.name(),
    AudioHandler::Conversation.name(),
    AudioHandler::Visitor.name(),
    AudioHandler::You.name(),
];

/// The name in an audio track's handler box, from a fixed set so no free
/// text reaches the file. Players show it as the track's title (QuickTime
/// and VLC list it in their audio track menus).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum AudioHandler {
    /// `SoundHandler`, the generic name most muxers write.
    #[default]
    Sound,
    /// `Conversation`: a mix of both sides of a call.
    Conversation,
    /// `Visitor`: the far side of a call (the camera's microphone).
    Visitor,
    /// `You`: the near side of a call (what this computer sent).
    You,
}

impl AudioHandler {
    /// Every audio handler, in [`HANDLER_NAMES`] order.
    pub const ALL: [AudioHandler; 4] = [
        AudioHandler::Sound,
        AudioHandler::Conversation,
        AudioHandler::Visitor,
        AudioHandler::You,
    ];

    /// The handler name as written.
    pub const fn name(self) -> &'static str {
        match self {
            AudioHandler::Sound => "SoundHandler",
            AudioHandler::Conversation => "Conversation",
            AudioHandler::Visitor => "Visitor",
            AudioHandler::You => "You",
        }
    }

    /// The handler for a name as written, if it is one of ours.
    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|h| h.name() == name)
    }
}

/// One audio track, declared before any media so an init segment can carry
/// it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AudioTrackSpec {
    /// The AAC format: from an ADTS header ([`parse_adts`](crate::parse_adts))
    /// or from an encoder's AudioSpecificConfig
    /// ([`AacConfig::from_raw_audio_specific_config`]).
    pub config: AacConfig,
    pub handler: AudioHandler,
    /// The track header's enabled flag. Players play enabled tracks; a
    /// disabled track is kept in the file for players that let the user pick
    /// it.
    pub enabled: bool,
    /// The track header's `alternate_group`: tracks with the same nonzero
    /// value are alternatives of each other, and a player plays one of them
    /// (normally the enabled one). 0 means no group.
    pub alternate_group: u16,
    /// Encoder delay: decoded samples at the start of the track that are
    /// not part of the audio (an AAC encoder's priming, typically 2112 for
    /// Apple's encoder). An edit list skips them, in the fragmented init's
    /// `moov` and in the finished file. 0 writes no edit for it.
    ///
    /// With priming, give each access unit the time of the block of 1024
    /// input samples it was made from (the first unit: the time of the first
    /// sample given to the encoder); the edit then moves the decoded audio
    /// back into line.
    pub priming_samples: u32,
}

impl AudioTrackSpec {
    /// A track as the single-track API writes it: `SoundHandler`, enabled,
    /// no alternate group, no priming.
    pub fn new(config: AacConfig) -> Self {
        Self {
            config,
            handler: AudioHandler::Sound,
            enabled: true,
            alternate_group: 0,
            priming_samples: 0,
        }
    }

    /// The track header flags: in movie (2), plus enabled (1).
    pub(crate) fn tkhd_flags(&self) -> u32 {
        if self.enabled { 3 } else { 2 }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn handler_names_are_fixed_generic_words() {
        assert_eq!(
            HANDLER_NAMES,
            [
                "VideoHandler",
                "SoundHandler",
                "Conversation",
                "Visitor",
                "You"
            ]
        );
        for h in AudioHandler::ALL {
            assert_eq!(AudioHandler::from_name(h.name()), Some(h));
        }
        assert_eq!(AudioHandler::from_name("VideoHandler"), None);
        assert_eq!(AudioHandler::from_name("anything else"), None);
        assert!(
            HANDLER_NAMES
                .iter()
                .all(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_alphabetic()))
        );
    }
}
