//! Which tracks a recording holds and how its audio is produced
//! (docs/CAPTURE.md, "Tracks").
//!
//! - A **live** recording has the video and, when the camera sends audio,
//!   one `SoundHandler` audio track (enabled): the camera's AAC passed
//!   through, or its other codec decoded and encoded by the app.
//! - A **call** recording has the video and three audio tracks in one
//!   alternate group: `Conversation` (enabled, first), `Visitor` and `You`
//!   (disabled), so a player plays Conversation and the other two are
//!   there to listen to one side. All three are kept ("Both").

use crate::caps::RecordingKind;
use mp4_remux::{AacConfig, AudioHandler, AudioTrackSpec};

/// Alternate group of the call tracks.
pub const CALL_ALTERNATE_GROUP: u16 = 1;

/// One audio track of a recording.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AudioTrackPlan {
    /// The handler name (and so the name players show).
    pub handler: AudioHandler,
    /// The track header's enabled flag.
    pub enabled: bool,
    /// The track header's alternate group (0: none).
    pub alternate_group: u16,
}

impl AudioTrackPlan {
    /// The muxer's declaration of this track for an encoder's AAC
    /// configuration and priming (0 for passthrough).
    pub fn spec(&self, config: AacConfig, priming_samples: u32) -> AudioTrackSpec {
        AudioTrackSpec {
            config,
            handler: self.handler,
            enabled: self.enabled,
            alternate_group: self.alternate_group,
            priming_samples,
        }
    }
}

/// The audio tracks of a kind of recording, in file order.
pub fn audio_tracks(kind: RecordingKind) -> Vec<AudioTrackPlan> {
    match kind {
        RecordingKind::Live => vec![AudioTrackPlan {
            handler: AudioHandler::Sound,
            enabled: true,
            alternate_group: 0,
        }],
        RecordingKind::Call => [
            (AudioHandler::Conversation, true),
            (AudioHandler::Visitor, false),
            (AudioHandler::You, false),
        ]
        .into_iter()
        .map(|(handler, enabled)| AudioTrackPlan {
            handler,
            enabled,
            alternate_group: CALL_ALTERNATE_GROUP,
        })
        .collect(),
    }
}

/// The track names the recording event lists (`tracks`).
pub fn track_names(kind: RecordingKind) -> Vec<&'static str> {
    match kind {
        RecordingKind::Live => vec!["video", "audio"],
        RecordingKind::Call => vec!["video", "Conversation", "Visitor", "You"],
    }
}

/// The camera's audio codec, as far as the recorder cares.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CameraAudio {
    /// The camera sends no audio.
    None,
    /// AAC (ADTS), which can pass through.
    Aac,
    /// Another codec (reported only), which needs decode and encode.
    Other,
}

/// The `audio` field of the recording event.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AudioMode {
    /// Camera AAC written as received.
    Passthrough,
    /// Decoded and encoded by the app.
    Encoded,
    /// The camera sends no audio.
    None,
    /// Not AAC and no encoder: video only.
    Unsupported,
}

impl AudioMode {
    /// The code as the event spells it.
    pub const fn as_str(self) -> &'static str {
        match self {
            AudioMode::Passthrough => "passthrough",
            AudioMode::Encoded => "encoded",
            AudioMode::None => "none",
            AudioMode::Unsupported => "unsupported",
        }
    }
}

/// How a **live** recording gets its audio.
pub fn live_audio_mode(camera: CameraAudio, encoder_available: bool) -> AudioMode {
    match camera {
        CameraAudio::None => AudioMode::None,
        CameraAudio::Aac => AudioMode::Passthrough,
        CameraAudio::Other if encoder_available => AudioMode::Encoded,
        CameraAudio::Other => AudioMode::Unsupported,
    }
}

/// How the **Visitor** track of a call is produced (AAC passes through,
/// else it is encoded); `None` when the call cannot be recorded because
/// an encoder is needed for Conversation and You anyway.
pub fn call_visitor_mode(camera: CameraAudio, encoder_available: bool) -> Option<AudioMode> {
    if !encoder_available {
        return None;
    }
    Some(match camera {
        CameraAudio::Aac => AudioMode::Passthrough,
        CameraAudio::None => AudioMode::None,
        CameraAudio::Other => AudioMode::Encoded,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn call_tracks_are_one_alternate_group_with_conversation_first_and_enabled() {
        let t = audio_tracks(RecordingKind::Call);
        let got: Vec<(&str, bool, u16)> = t
            .iter()
            .map(|p| (p.handler.name(), p.enabled, p.alternate_group))
            .collect();
        assert_eq!(
            got,
            [
                ("Conversation", true, 1),
                ("Visitor", false, 1),
                ("You", false, 1)
            ]
        );
        assert_eq!(
            track_names(RecordingKind::Call),
            ["video", "Conversation", "Visitor", "You"]
        );
    }

    #[test]
    fn a_live_recording_has_one_enabled_sound_track() {
        let t = audio_tracks(RecordingKind::Live);
        assert_eq!(t.len(), 1);
        assert_eq!(t[0].handler, AudioHandler::Sound);
        assert!(t[0].enabled);
        assert_eq!(t[0].alternate_group, 0);
        assert_eq!(track_names(RecordingKind::Live), ["video", "audio"]);
    }

    #[test]
    fn every_handler_is_an_allowed_generic_name() {
        for kind in [RecordingKind::Live, RecordingKind::Call] {
            for p in audio_tracks(kind) {
                assert!(recordings::export::handler_name_allowed(p.handler.name()));
            }
        }
    }

    #[test]
    fn specs_carry_the_flags_and_priming() {
        let plan = audio_tracks(RecordingKind::Call)[1];
        let cfg = AacConfig::from_raw_audio_specific_config(&[0x11, 0x88]).unwrap();
        let s = plan.spec(cfg, 2112);
        assert_eq!(s.handler, AudioHandler::Visitor);
        assert!(!s.enabled);
        assert_eq!(s.alternate_group, 1);
        assert_eq!(s.priming_samples, 2112);
    }

    #[test]
    fn live_audio_mode_table() {
        use AudioMode::*;
        use CameraAudio as C;
        let table = [
            (C::None, false, None),
            (C::None, true, None),
            (C::Aac, false, Passthrough),
            (C::Aac, true, Passthrough),
            (C::Other, true, Encoded),
            (C::Other, false, Unsupported),
        ];
        for (cam, enc, want) in table {
            assert_eq!(live_audio_mode(cam, enc), want, "{cam:?} {enc}");
        }
        assert_eq!(Unsupported.as_str(), "unsupported");
    }

    #[test]
    fn a_call_needs_an_encoder() {
        assert_eq!(call_visitor_mode(CameraAudio::Aac, false), None);
        assert_eq!(
            call_visitor_mode(CameraAudio::Aac, true),
            Some(AudioMode::Passthrough)
        );
        assert_eq!(
            call_visitor_mode(CameraAudio::Other, true),
            Some(AudioMode::Encoded)
        );
    }
}
