//! A short preview that tells whether a channel's camera has a second lens
//! its channel list does not reveal (docs/protocol.md, "Second lenses").

use crate::queue::Item;
use crate::supervisor::{ConnState, DeviceSession};
use bc_protocol::media::Record;
use bc_protocol::stream::StreamTier;
use std::time::Duration;
use tokio::time::{Instant, timeout_at};

/// What [`DeviceSession::probe_second_lens`] found.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LensProbe {
    /// Video arrived on `externStream`: the camera has a second lens.
    SecondLens {
        /// Time from the preview start to the first picture (a keyframe).
        after: Duration,
    },
    /// No video within the window: no second lens.
    NoVideo {
        /// A stream-info record arrived (cameras without a second lens send
        /// one, of size 0x0, and nothing more).
        info: bool,
    },
    /// The device refused the preview: no second lens. A 400 counts too
    /// (the probe does not wait for a stream the way a viewer does for a
    /// third-party camera).
    Refused {
        /// Reply status.
        status: u16,
    },
    /// No answer either way (the session was not logged in, the connection
    /// dropped, or the session ended); try again on a later connection.
    Inconclusive,
}

impl DeviceSession {
    /// Starts an `externStream` preview of `channel`, waits up to `window`
    /// for its first picture, and stops it again: on every path, the
    /// preview is stopped before this returns (dropping the subscription
    /// sends the stop, unless another subscriber shares the stream). Uses
    /// this session's connection; never a second login.
    ///
    /// Meant for channels whose channel list (145) does not name
    /// `externStream`: for those, video means a second lens.
    pub async fn probe_second_lens(&self, channel: u8, window: Duration) -> LensProbe {
        if self.state() != ConnState::Connected {
            return LensProbe::Inconclusive;
        }
        let Ok(mut sub) = self.subscribe(channel, StreamTier::Extern).await else {
            return LensProbe::Inconclusive;
        };
        let started = Instant::now();
        let deadline = started + window;
        let mut info = false;
        let result = loop {
            match timeout_at(deadline, sub.recv()).await {
                // The queue holds back pictures until a keyframe, so the
                // first video record is one.
                Ok(Some(Item::Media(Record::Video { .. }))) => {
                    break LensProbe::SecondLens {
                        after: started.elapsed(),
                    };
                }
                Ok(Some(Item::Media(Record::Info { .. }))) => info = true,
                Ok(Some(Item::Media(_))) => {}
                Ok(Some(Item::WaitingForDevice { status } | Item::StartRejected { status })) => {
                    break LensProbe::Refused { status };
                }
                Ok(Some(Item::Discontinuity)) | Ok(None) => break LensProbe::Inconclusive,
                Err(_) if self.state() == ConnState::Connected => {
                    break LensProbe::NoVideo { info };
                }
                Err(_) => break LensProbe::Inconclusive,
            }
        };
        drop(sub);
        result
    }
}
