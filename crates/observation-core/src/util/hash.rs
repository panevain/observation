//! Stable 64-bit fingerprints.

/// A fingerprint as 16 lower-case hex digits.
pub fn fingerprint_hex(f: u64) -> String {
    format!("{f:016x}")
}

/// 64-bit FNV-1a: small, stable across platforms, Rust versions and runs
/// (unlike `std`'s `DefaultHasher`), which is what a fingerprint saved in a
/// file needs. Not collision-resistant against an adversary, and need not be.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Fnv64(u64);

impl Default for Fnv64 {
    fn default() -> Self {
        Self(0xcbf2_9ce4_8422_2325)
    }
}

impl Fnv64 {
    pub(crate) fn bytes(&mut self, b: &[u8]) -> &mut Self {
        for &x in b {
            self.0 ^= u64::from(x);
            self.0 = self.0.wrapping_mul(0x0000_0100_0000_01b3);
        }
        self
    }

    pub(crate) fn u64(&mut self, v: u64) -> &mut Self {
        self.bytes(&v.to_le_bytes())
    }

    pub(crate) fn i64(&mut self, v: i64) -> &mut Self {
        self.bytes(&v.to_le_bytes())
    }

    pub(crate) fn f64(&mut self, v: f64) -> &mut Self {
        self.u64(v.to_bits())
    }

    pub(crate) fn f32(&mut self, v: f32) -> &mut Self {
        self.bytes(&v.to_bits().to_le_bytes())
    }

    /// Length-prefixed, so `("ab", "c")` and `("a", "bc")` differ.
    pub(crate) fn str(&mut self, s: &str) -> &mut Self {
        self.u64(s.len() as u64).bytes(s.as_bytes())
    }

    pub(crate) fn opt_str(&mut self, s: Option<&str>) -> &mut Self {
        match s {
            Some(s) => self.bytes(&[1]).str(s),
            None => self.bytes(&[0]),
        }
    }

    pub(crate) fn finish(&self) -> u64 {
        self.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fnv_is_stable() {
        // The published FNV-1a 64 test vectors.
        assert_eq!(Fnv64::default().finish(), 0xcbf2_9ce4_8422_2325);
        assert_eq!(Fnv64::default().bytes(b"a").finish(), 0xaf63_dc4c_8601_ec8c);
        assert_eq!(
            Fnv64::default().bytes(b"foobar").finish(),
            0x8594_4171_f739_67e8
        );
        let a = Fnv64::default().str("ab").str("c").finish();
        let b = Fnv64::default().str("a").str("bc").finish();
        assert_ne!(a, b);
        assert_eq!(fingerprint_hex(0xab), "00000000000000ab");
    }
}
