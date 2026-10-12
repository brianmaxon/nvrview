//! Reason and limit codes, spelled as the C interface reports them
//! (docs/FFI.md, "Capture").

/// Why a recording or a segment ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Reason {
    /// The user stopped it.
    Stopped,
    /// The duration cap was reached.
    MaxDuration,
    /// The size cap was reached.
    MaxSize,
    /// Free space fell under the stop threshold.
    DiskLow,
    /// The device was removed.
    DeviceRemoved,
    /// The device refused the login.
    LoginFailed,
    /// The core is being freed.
    CoreShutdown,
    /// A write failed.
    WriteFailed,
    /// The audio encoder failed.
    EncoderFailed,
    /// No keyframe arrived in time (nothing to record).
    NoVideo,
    /// New parameter sets, codec, or resolution: a new segment starts.
    FormatChanged,
    /// A gap over the continue limit: a new segment starts.
    LongGap,
}

impl Reason {
    /// Every reason.
    pub const ALL: [Reason; 12] = [
        Reason::Stopped,
        Reason::MaxDuration,
        Reason::MaxSize,
        Reason::DiskLow,
        Reason::DeviceRemoved,
        Reason::LoginFailed,
        Reason::CoreShutdown,
        Reason::WriteFailed,
        Reason::EncoderFailed,
        Reason::NoVideo,
        Reason::FormatChanged,
        Reason::LongGap,
    ];

    /// The code as the `reason` field of the recording event.
    pub const fn as_str(self) -> &'static str {
        match self {
            Reason::Stopped => "stopped",
            Reason::MaxDuration => "max_duration",
            Reason::MaxSize => "max_size",
            Reason::DiskLow => "disk_low",
            Reason::DeviceRemoved => "device_removed",
            Reason::LoginFailed => "login_failed",
            Reason::CoreShutdown => "core_shutdown",
            Reason::WriteFailed => "write_failed",
            Reason::EncoderFailed => "encoder_failed",
            Reason::NoVideo => "no_video",
            Reason::FormatChanged => "format_changed",
            Reason::LongGap => "long_gap",
        }
    }

    /// True for the reasons that end a segment but not the recording.
    pub const fn continues_recording(self) -> bool {
        matches!(self, Reason::FormatChanged | Reason::LongGap)
    }

    /// True when the state is `failed` rather than `finished`.
    pub const fn is_failure(self) -> bool {
        matches!(
            self,
            Reason::LoginFailed | Reason::WriteFailed | Reason::EncoderFailed | Reason::NoVideo
        )
    }
}

/// Which limit refused a start (the `limit` field of the answer).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LimitKind {
    /// Too many live recordings.
    Live,
    /// Too many call recordings.
    Calls,
    /// Too many main-stream recorders on one device.
    MainStreams,
    /// Under the free-space threshold.
    DiskLow,
}

impl LimitKind {
    /// The code as the C interface spells it.
    pub const fn as_str(self) -> &'static str {
        match self {
            LimitKind::Live => "live",
            LimitKind::Calls => "calls",
            LimitKind::MainStreams => "main_streams",
            LimitKind::DiskLow => "disk_low",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codes_match_the_interface() {
        let codes: Vec<&str> = Reason::ALL.iter().map(|r| r.as_str()).collect();
        assert_eq!(
            codes,
            [
                "stopped",
                "max_duration",
                "max_size",
                "disk_low",
                "device_removed",
                "login_failed",
                "core_shutdown",
                "write_failed",
                "encoder_failed",
                "no_video",
                "format_changed",
                "long_gap"
            ]
        );
        let limits: Vec<&str> = [
            LimitKind::Live,
            LimitKind::Calls,
            LimitKind::MainStreams,
            LimitKind::DiskLow,
        ]
        .iter()
        .map(|l| l.as_str())
        .collect();
        assert_eq!(limits, ["live", "calls", "main_streams", "disk_low"]);
    }

    #[test]
    fn only_two_reasons_continue_the_recording() {
        let cont: Vec<Reason> = Reason::ALL
            .into_iter()
            .filter(|r| r.continues_recording())
            .collect();
        assert_eq!(cont, [Reason::FormatChanged, Reason::LongGap]);
        assert!(Reason::WriteFailed.is_failure());
        assert!(!Reason::MaxSize.is_failure());
    }
}
