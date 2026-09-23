//! The token gate: one comparison, no early exit.
//!
//! `APPRICOT_STREAM_TOKEN` hands the streamer the session credential. The first client message
//! carries the same bytes (rule 8, docs/protocol/README.md), and they must match before anything
//! else happens. A plain `==` on slices returns at the first differing byte, so the time the
//! comparison takes would leak how much of the token is right. [`token_matches`] instead walks
//! both inputs to the end whatever it finds, folding every difference into one accumulator, and
//! decides at the last byte.
//!
//! This is not a constant-time comparison in the cryptographic sense (it does not hide length,
//! and the instruction timing of the loop is not flattened); it has no early exit, which is the
//! property this gate needs. A remote attacker who cannot measure the loop's duration learns
//! nothing from it either way.
//!
//! The token itself travels through the process as a [`StreamToken`], whose `Debug` prints a
//! placeholder and which has no `Display`: a `?config` in a log line or a panic message cannot
//! put the secret into the container's logs (threat model §4.1, "Tokens redacted in
//! `Debug`/`Display`").

use std::fmt;

/// The stream token, redacted wherever it is formatted.
///
/// `Debug` prints `StreamToken(<redacted>, N bytes)`; there is no `Display`. Equality uses
/// [`token_matches`], so comparing two tokens has no early exit either.
#[derive(Clone)]
pub struct StreamToken(Vec<u8>);

impl StreamToken {
    /// Wraps the token's bytes.
    pub fn new(bytes: Vec<u8>) -> Self {
        Self(bytes)
    }

    /// The token's bytes, for the one comparison that needs them.
    pub fn expose(&self) -> &[u8] {
        &self.0
    }

    /// How many bytes the token has.
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// Whether the token has no bytes at all.
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// True when `offered` is this token, byte for byte (see [`token_matches`]).
    pub fn matches(&self, offered: &[u8]) -> bool {
        token_matches(&self.0, offered)
    }
}

impl From<Vec<u8>> for StreamToken {
    fn from(bytes: Vec<u8>) -> Self {
        Self(bytes)
    }
}

impl fmt::Debug for StreamToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "StreamToken(<redacted>, {} bytes)", self.0.len())
    }
}

impl PartialEq for StreamToken {
    fn eq(&self, other: &Self) -> bool {
        token_matches(&self.0, &other.0)
    }
}

impl Eq for StreamToken {}

/// True when `offered` is byte-for-byte `expected`.
///
/// Both inputs are walked in full; the loop count depends only on the longer length, never on
/// where the inputs differ.
pub fn token_matches(expected: &[u8], offered: &[u8]) -> bool {
    let len = expected.len().max(offered.len());
    let mut diff = 0u8;
    for i in 0..len {
        // A byte past the end of one input is a mismatch folded into the same accumulator.
        let a = expected.get(i).copied().unwrap_or(0);
        let b = offered.get(i).copied().unwrap_or(0);
        diff |= a ^ b;
    }
    // A length difference is a difference, folded in the same way.
    diff |= u8::from(expected.len() != offered.len());
    diff == 0
}

#[cfg(test)]
mod tests {
    use super::{StreamToken, token_matches};

    #[test]
    fn a_token_never_formats_its_bytes() {
        let token = StreamToken::new(b"sesame-open-please".to_vec());
        let shown = format!("{token:?}");
        assert!(!shown.contains("sesame"), "Debug leaked the token: {shown}");
        assert_eq!(shown, "StreamToken(<redacted>, 18 bytes)");
        assert!(token.matches(b"sesame-open-please"));
        assert!(!token.matches(b"sesame"));
    }

    #[test]
    fn equal_tokens_match() {
        assert!(token_matches(b"sesame", b"sesame"));
    }

    #[test]
    fn different_tokens_do_not_match() {
        assert!(!token_matches(b"sesame", b"sesamE"));
        assert!(!token_matches(b"sesame", b"sesame "));
        assert!(!token_matches(b"sesame", b"sesam"));
    }

    #[test]
    fn the_empty_token_matches_only_itself() {
        assert!(token_matches(b"", b""));
        assert!(!token_matches(b"", b"x"));
        assert!(!token_matches(b"x", b""));
    }

    #[test]
    fn a_shared_prefix_is_not_enough() {
        assert!(!token_matches(
            b"aaaaaaaaaaaaaaaaaaaa",
            b"aaaaaaaaaaaaaaaaaaab"
        ));
    }
}
