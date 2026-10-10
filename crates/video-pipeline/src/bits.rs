//! Bit reader over a NAL unit payload, removing emulation prevention bytes on
//! the fly so no RBSP copy is ever allocated.
//!
//! Inside a NAL unit, the encoder inserts `0x03` after any two zero bytes that
//! would otherwise be followed by a byte in `0x00..=0x03` (H.264 7.4.1,
//! H.265 7.4.2). The reader drops every `0x03` that follows two zero bytes.

/// Why a bitstream structure could not be parsed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ParseError {
    /// The data ended before the structure did.
    Truncated,
    /// A syntax element was outside the range this crate accepts; the name
    /// says which.
    OutOfRange(&'static str),
    /// Valid syntax this crate does not handle (for example a multi-layer
    /// extension); the name says which.
    Unsupported(&'static str),
    /// A required parameter set was not present.
    Missing(&'static str),
}

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ParseError::Truncated => write!(f, "truncated"),
            ParseError::OutOfRange(what) => write!(f, "{what} out of range"),
            ParseError::Unsupported(what) => write!(f, "unsupported {what}"),
            ParseError::Missing(what) => write!(f, "missing {what}"),
        }
    }
}

impl std::error::Error for ParseError {}

pub(crate) type Result<T> = std::result::Result<T, ParseError>;

pub(crate) struct BitReader<'a> {
    data: &'a [u8],
    /// Next raw byte to load.
    pos: usize,
    /// Zero bytes seen in a row, for emulation prevention.
    zeros: u8,
    cur: u8,
    /// Unread bits left in `cur`.
    left: u8,
}

impl<'a> BitReader<'a> {
    /// Reads `data`, which must start after the NAL unit header.
    pub(crate) fn new(data: &'a [u8]) -> BitReader<'a> {
        BitReader {
            data,
            pos: 0,
            zeros: 0,
            cur: 0,
            left: 0,
        }
    }

    fn load(&mut self) -> Result<()> {
        loop {
            let b = *self.data.get(self.pos).ok_or(ParseError::Truncated)?;
            self.pos += 1;
            if self.zeros >= 2 && b == 0x03 {
                self.zeros = 0;
                continue;
            }
            self.zeros = if b == 0 {
                self.zeros.saturating_add(1)
            } else {
                0
            };
            self.cur = b;
            self.left = 8;
            return Ok(());
        }
    }

    pub(crate) fn bit(&mut self) -> Result<bool> {
        if self.left == 0 {
            self.load()?;
        }
        self.left -= 1;
        Ok((self.cur >> self.left) & 1 == 1)
    }

    pub(crate) fn flag(&mut self) -> Result<bool> {
        self.bit()
    }

    /// `u(n)` for `n <= 32`.
    pub(crate) fn bits(&mut self, n: u32) -> Result<u32> {
        debug_assert!(n <= 32);
        let mut v: u32 = 0;
        for _ in 0..n {
            v = (v << 1) | u32::from(self.bit()?);
        }
        Ok(v)
    }

    pub(crate) fn u8(&mut self, n: u32) -> Result<u8> {
        debug_assert!(n <= 8);
        Ok(self.bits(n)? as u8)
    }

    /// Skips `n` bits.
    pub(crate) fn skip(&mut self, n: u32) -> Result<()> {
        for _ in 0..n {
            self.bit()?;
        }
        Ok(())
    }

    /// `ue(v)`: unsigned exponential-Golomb (H.264 9.1, H.265 9.2). Codes
    /// longer than 32 leading zeros cannot fit 32 bits and are rejected.
    pub(crate) fn ue(&mut self) -> Result<u32> {
        let mut zeros = 0u32;
        while !self.bit()? {
            zeros += 1;
            if zeros > 31 {
                return Err(ParseError::OutOfRange("exp-Golomb code"));
            }
        }
        let suffix = self.bits(zeros)?;
        // (2^zeros - 1) + suffix, at most 2^32 - 2.
        Ok(((1u64 << zeros) - 1 + u64::from(suffix)) as u32)
    }

    /// `ue(v)` that must not exceed `max`.
    pub(crate) fn ue_max(&mut self, max: u32, name: &'static str) -> Result<u32> {
        let v = self.ue()?;
        if v > max {
            return Err(ParseError::OutOfRange(name));
        }
        Ok(v)
    }

    /// `se(v)`: signed exponential-Golomb, mapping 1, 2, 3, 4 to 1, -1, 2, -2.
    pub(crate) fn se(&mut self) -> Result<i32> {
        let k = i64::from(self.ue()?);
        let v = if k % 2 == 1 { (k + 1) / 2 } else { -(k / 2) };
        Ok(v as i32)
    }
}

#[cfg(test)]
pub(crate) mod writer {
    //! Builds bitstreams in tests, inserting emulation prevention bytes.

    #[derive(Default)]
    pub(crate) struct BitWriter {
        bytes: Vec<u8>,
        cur: u8,
        n: u8,
    }

    impl BitWriter {
        pub(crate) fn bit(&mut self, b: bool) -> &mut Self {
            self.cur = (self.cur << 1) | u8::from(b);
            self.n += 1;
            if self.n == 8 {
                self.bytes.push(self.cur);
                self.cur = 0;
                self.n = 0;
            }
            self
        }
        pub(crate) fn bits(&mut self, n: u32, v: u64) -> &mut Self {
            for i in (0..n).rev() {
                self.bit(i < 64 && (v >> i) & 1 == 1);
            }
            self
        }
        pub(crate) fn ue(&mut self, v: u32) -> &mut Self {
            let x = u64::from(v) + 1;
            let len = 64 - x.leading_zeros();
            self.bits(len - 1, 0).bits(len, x)
        }
        pub(crate) fn se(&mut self, v: i32) -> &mut Self {
            let k = if v > 0 {
                (2 * i64::from(v) - 1) as u32
            } else {
                (-2 * i64::from(v)) as u32
            };
            self.ue(k)
        }
        /// Appends rbsp_trailing_bits and returns the RBSP with emulation
        /// prevention applied.
        pub(crate) fn finish(mut self) -> Vec<u8> {
            self.bit(true);
            while self.n != 0 {
                self.bit(false);
            }
            let mut out = Vec::with_capacity(self.bytes.len() + 8);
            let mut zeros = 0;
            for b in self.bytes {
                if zeros >= 2 && b <= 3 {
                    out.push(3);
                    zeros = 0;
                }
                zeros = if b == 0 { zeros + 1 } else { 0 };
                out.push(b);
            }
            out
        }
    }
}

#[cfg(test)]
mod tests {
    use super::writer::BitWriter;
    use super::*;

    #[test]
    fn exp_golomb_round_trip() {
        let mut w = BitWriter::default();
        let ues = [0u32, 1, 2, 3, 7, 8, 254, 255, 65_535, 1 << 20, u32::MAX - 1];
        let ses = [0i32, 1, -1, 2, -2, 127, -128, 1 << 20, -(1 << 20)];
        for v in ues {
            w.ue(v);
        }
        for v in ses {
            w.se(v);
        }
        w.bits(5, 0b10110);
        let data = w.finish();
        let mut r = BitReader::new(&data);
        for v in ues {
            assert_eq!(r.ue().unwrap(), v);
        }
        for v in ses {
            assert_eq!(r.se().unwrap(), v);
        }
        assert_eq!(r.bits(5).unwrap(), 0b10110);
    }

    #[test]
    fn emulation_prevention_removed() {
        // 00 00 03 01 reads as 00 00 01; a 03 not after two zeros is kept.
        let data = [
            0x00, 0x00, 0x03, 0x01, 0x03, 0x00, 0x00, 0x03, 0x00, 0x00, 0x03,
        ];
        let mut r = BitReader::new(&data);
        let got: Vec<u8> = (0..6).map(|_| r.u8(8).unwrap()).collect();
        assert_eq!(got, [0x00, 0x00, 0x01, 0x03, 0x00, 0x00]);
        // The next 00 00 03 sequence: 00 00 then the trailing 03 is dropped.
        assert_eq!(r.u8(8).unwrap(), 0x00);
        assert_eq!(r.u8(8).unwrap(), 0x00);
        assert_eq!(r.u8(8), Err(ParseError::Truncated));
    }

    #[test]
    fn overlong_exp_golomb_rejected() {
        let data = [0u8; 8];
        assert!(BitReader::new(&data).ue().is_err());
        assert_eq!(BitReader::new(&[]).ue(), Err(ParseError::Truncated));
    }
}
