//! Bit reader with Exp-Golomb codes, over RBSP bytes.

/// Removes emulation-prevention bytes (`00 00 03` becomes `00 00`).
pub(crate) fn unescape(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len());
    let mut zeros = 0;
    for &b in data {
        if zeros >= 2 && b == 3 {
            zeros = 0;
            continue;
        }
        zeros = if b == 0 { zeros + 1 } else { 0 };
        out.push(b);
    }
    out
}

pub(crate) struct BitReader<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> BitReader<'a> {
    pub(crate) fn new(data: &'a [u8]) -> Self {
        Self { data, pos: 0 }
    }

    pub(crate) fn bit(&mut self) -> Option<u32> {
        let byte = *self.data.get(self.pos / 8)?;
        let bit = (byte >> (7 - self.pos % 8)) & 1;
        self.pos += 1;
        Some(u32::from(bit))
    }

    pub(crate) fn bits(&mut self, n: u32) -> Option<u32> {
        debug_assert!(n <= 32);
        let mut v: u64 = 0;
        for _ in 0..n {
            v = (v << 1) | u64::from(self.bit()?);
        }
        Some(v as u32)
    }

    pub(crate) fn skip(&mut self, n: usize) -> Option<()> {
        if self.pos + n > self.data.len() * 8 {
            return None;
        }
        self.pos += n;
        Some(())
    }

    pub(crate) fn ue(&mut self) -> Option<u32> {
        let mut zeros = 0;
        while self.bit()? == 0 {
            zeros += 1;
            if zeros > 32 {
                return None;
            }
        }
        if zeros == 0 {
            return Some(0);
        }
        let rest = u64::from(self.bits(zeros)?);
        u32::try_from((1u64 << zeros) - 1 + rest).ok()
    }

    pub(crate) fn se(&mut self) -> Option<i32> {
        let k = i64::from(self.ue()?);
        let v = if k % 2 == 1 { (k + 1) / 2 } else { -(k / 2) };
        i32::try_from(v).ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exp_golomb() {
        // 1 | 010 | 011 | 00100 | 00101 -> ue 0,1,2,3,4
        let mut r = BitReader::new(&[0b1010_0110, 0b0100_0010, 0b1000_0000]);
        let v: Vec<u32> = (0..5).map(|_| r.ue().unwrap()).collect();
        assert_eq!(v, [0, 1, 2, 3, 4]);
        // se mapping: 1->1, 2->-1, 3->2
        let mut r = BitReader::new(&[0b0100_1100, 0b1000_0000]);
        assert_eq!(r.se(), Some(1));
        assert_eq!(r.se(), Some(-1));
        assert_eq!(r.se(), Some(2));
    }

    #[test]
    fn unescape_removes_prevention_bytes() {
        assert_eq!(unescape(&[0, 0, 3, 1, 0, 0, 3]), [0, 0, 1, 0, 0]);
        assert_eq!(unescape(&[0, 3, 0]), [0, 3, 0]);
    }
}
