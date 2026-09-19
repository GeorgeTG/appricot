//! Strings with a byte cap, checked before they are copied.

use std::fmt;

/// A UTF-8 string of at most `MAX` bytes.
///
/// Every string that crosses the wire is one of these. The cap counts bytes, not characters,
/// because bytes are what a decoder reads and allocates.
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct BoundedString<const MAX: usize>(String);

impl<const MAX: usize> BoundedString<MAX> {
    /// The cap, in bytes.
    pub const MAX_BYTES: usize = MAX;

    /// Copies `s` if it fits, or fails with [`BoundError::TooLong`] before copying anything.
    pub fn new(s: &str) -> Result<Self, BoundError> {
        check_len(s.len(), MAX)?;
        Ok(Self(s.to_owned()))
    }

    /// Copies as much of `s` as fits, cut at a character boundary.
    ///
    /// For text a backend reads from the app, such as a window title, where a long value
    /// should be shortened rather than dropped.
    pub fn truncated(s: &str) -> Self {
        let mut end = s.len().min(MAX);
        while !s.is_char_boundary(end) {
            end -= 1;
        }
        Self(s[..end].to_owned())
    }

    /// Decodes UTF-8 bytes that must fit.
    ///
    /// Fails with [`BoundError::TooLong`] before looking at the bytes, or with
    /// [`BoundError::InvalidUtf8`].
    pub fn decode(bytes: &[u8]) -> Result<Self, BoundError> {
        check_len(bytes.len(), MAX)?;
        let Ok(s) = std::str::from_utf8(bytes) else {
            return Err(BoundError::InvalidUtf8);
        };
        Ok(Self(s.to_owned()))
    }

    /// Decodes a string whose length comes first, as a little-endian `u16`.
    ///
    /// Returns the string and the bytes after it. The length is checked against `MAX` before
    /// the payload is touched, so a hostile length cannot make the decoder read or allocate
    /// more than the cap. Fails with [`BoundError::Truncated`] when the input ends early, with
    /// [`BoundError::TooLong`], or with [`BoundError::InvalidUtf8`].
    pub fn decode_prefixed(bytes: &[u8]) -> Result<(Self, &[u8]), BoundError> {
        let Some((prefix, rest)) = bytes.split_first_chunk::<2>() else {
            return Err(BoundError::Truncated);
        };
        let len = usize::from(u16::from_le_bytes(*prefix));
        check_len(len, MAX)?;
        let Some((payload, rest)) = rest.split_at_checked(len) else {
            return Err(BoundError::Truncated);
        };
        Ok((Self::decode(payload)?, rest))
    }

    /// The string.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The length, in bytes.
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// True for the empty string.
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Unwraps the inner `String`.
    pub fn into_string(self) -> String {
        self.0
    }
}

impl<const MAX: usize> AsRef<str> for BoundedString<MAX> {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl<const MAX: usize> fmt::Display for BoundedString<MAX> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl<const MAX: usize> TryFrom<&str> for BoundedString<MAX> {
    type Error = BoundError;

    fn try_from(s: &str) -> Result<Self, Self::Error> {
        Self::new(s)
    }
}

impl<const MAX: usize> TryFrom<String> for BoundedString<MAX> {
    type Error = BoundError;

    fn try_from(s: String) -> Result<Self, Self::Error> {
        check_len(s.len(), MAX)?;
        Ok(Self(s))
    }
}

/// Why a string was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BoundError {
    /// The string is longer than its cap.
    TooLong {
        /// Its length, in bytes.
        len: usize,
        /// The cap, in bytes.
        max: usize,
    },
    /// The input ended before the string did.
    Truncated,
    /// The bytes are not valid UTF-8.
    InvalidUtf8,
}

impl fmt::Display for BoundError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TooLong { len, max } => write!(f, "{len} bytes, over the cap of {max}"),
            Self::Truncated => f.write_str("input ended inside a string"),
            Self::InvalidUtf8 => f.write_str("string is not valid UTF-8"),
        }
    }
}

impl std::error::Error for BoundError {}

fn check_len(len: usize, max: usize) -> Result<(), BoundError> {
    if len > max {
        return Err(BoundError::TooLong { len, max });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{BoundError, BoundedString};

    type Tiny = BoundedString<4>;

    #[test]
    fn accepts_a_string_at_the_cap() {
        let s = Tiny::new("abcd").expect("four bytes fit a cap of four");
        assert_eq!(s.as_str(), "abcd");
        assert_eq!(s.len(), 4);
    }

    #[test]
    fn refuses_one_byte_over_the_cap() {
        let got = Tiny::new("abcde");
        assert_eq!(got, Err(BoundError::TooLong { len: 5, max: 4 }));
    }

    #[test]
    fn counts_bytes_not_characters() {
        // Two Greek letters are four bytes in UTF-8.
        let got = BoundedString::<3>::new("αβ");
        assert_eq!(got, Err(BoundError::TooLong { len: 4, max: 3 }));
    }

    #[test]
    fn truncates_at_a_character_boundary() {
        // "αβγ" is six bytes. Three would split "β", so only "α" is kept.
        let s = BoundedString::<3>::truncated("αβγ");
        assert_eq!(s.as_str(), "α");
    }

    #[test]
    fn refuses_invalid_utf8() {
        let got = Tiny::decode(&[0xff]);
        assert_eq!(got, Err(BoundError::InvalidUtf8));
    }

    #[test]
    fn checks_a_length_prefix_before_the_payload() {
        // The prefix claims 65535 bytes and none follow. The cap refuses it before the
        // decoder looks for them, so the error is TooLong, not Truncated.
        let got = Tiny::decode_prefixed(&[0xff, 0xff]);
        let len = usize::from(u16::MAX);
        assert_eq!(got, Err(BoundError::TooLong { len, max: 4 }));
    }

    #[test]
    fn reports_a_payload_shorter_than_its_prefix() {
        let got = Tiny::decode_prefixed(&[3, 0, b'a']);
        assert_eq!(got, Err(BoundError::Truncated));
    }

    #[test]
    fn returns_the_bytes_after_a_prefixed_string() {
        let bytes = [2, 0, b'h', b'i', 9];
        let (s, rest) = Tiny::decode_prefixed(&bytes).expect("fits");
        assert_eq!(s.as_str(), "hi");
        assert_eq!(rest, &[9]);
    }
}
