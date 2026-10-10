//! Stream tiers and handle encoding.

/// A stream tier of a channel.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum StreamTier {
    /// `mainStream`, tier 0: the usual high-quality stream.
    Main,
    /// `subStream`, tier 1: the usual low-quality stream.
    Sub,
    /// `mobileStream`, tier 2: low tier of a second lens.
    Mobile,
    /// `externStream`, tier 4: high tier of a second lens.
    Extern,
}

impl StreamTier {
    /// Tier number used in the handle.
    pub const fn number(self) -> u32 {
        match self {
            StreamTier::Main => 0,
            StreamTier::Sub => 1,
            StreamTier::Mobile => 2,
            StreamTier::Extern => 4,
        }
    }

    /// Parses a tier number.
    pub const fn from_number(n: u32) -> Option<StreamTier> {
        match n {
            0 => Some(StreamTier::Main),
            1 => Some(StreamTier::Sub),
            2 => Some(StreamTier::Mobile),
            4 => Some(StreamTier::Extern),
            _ => None,
        }
    }

    /// Name used in the `streamType` XML element.
    pub const fn name(self) -> &'static str {
        match self {
            StreamTier::Main => "mainStream",
            StreamTier::Sub => "subStream",
            StreamTier::Mobile => "mobileStream",
            StreamTier::Extern => "externStream",
        }
    }

    /// Parses a `streamType` name.
    pub fn from_name(name: &str) -> Option<StreamTier> {
        [
            StreamTier::Main,
            StreamTier::Sub,
            StreamTier::Mobile,
            StreamTier::Extern,
        ]
        .into_iter()
        .find(|t| t.name() == name)
    }

    /// Handle for a channel on this tier: `(tier << 8) | channel`.
    pub const fn handle(self, channel: u8) -> u32 {
        (self.number() << 8) | channel as u32
    }

    /// Splits a handle into tier and channel; `None` for unknown tiers.
    pub const fn from_handle(handle: u32) -> Option<(StreamTier, u8)> {
        match StreamTier::from_number(handle >> 8) {
            Some(t) => Some((t, (handle & 0xff) as u8)),
            None => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn handles() {
        assert_eq!(StreamTier::Main.handle(3), 3);
        assert_eq!(StreamTier::Sub.handle(0), 256);
        assert_eq!(StreamTier::Mobile.handle(1), 513);
        assert_eq!(StreamTier::Extern.handle(2), 1026);
        for t in [
            StreamTier::Main,
            StreamTier::Sub,
            StreamTier::Mobile,
            StreamTier::Extern,
        ] {
            assert_eq!(StreamTier::from_handle(t.handle(15)), Some((t, 15)));
            assert_eq!(StreamTier::from_name(t.name()), Some(t));
        }
        assert_eq!(StreamTier::from_handle(3 << 8), None);
    }
}
