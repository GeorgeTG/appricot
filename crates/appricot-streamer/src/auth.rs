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
    use super::token_matches;

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
