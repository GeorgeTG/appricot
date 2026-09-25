//! The CLIPBOARD selection the backend owns and serves as text.
//!
//! The backend takes ownership of `CLIPBOARD` at connect, with an invisible window of its
//! own, so that every paste the app makes arrives as a `SelectionRequest` here:
//!
//! - When the host has given text through `InputSink::clipboard_set`, that text is served
//!   (as `UTF8_STRING`, `STRING`/`TEXT` in Latin-1, and `text/plain;charset=utf-8`).
//! - When the backend holds no text, the request is answered with a refusal and reported
//!   upstream as `ClipboardRequested`, so the host can push text for the next paste.
//!
//! `TARGETS` lists what is served, `TARGETS` and `TIMESTAMP` included, as `ATOM` data in
//! format 32; `TIMESTAMP` answers with the server time the backend took the selection at, as
//! `INTEGER` in format 32. Both are targets every selection owner must support (ICCCM,
//! "Target Atoms", <https://xorg.freedesktop.org/archive/X11R7.6/doc/xorg-docs/specs/ICCCM/icccm.html>,
//! checked 2026-09-23). `COMPOUND_TEXT` and `MULTIPLE` are refused rather
//! than half-served; `INCR` is never advertised, so a requester that needs it falls back or
//! fails on a small transfer.
//!
//! The other direction is the fetch: when another client takes the selection (the app
//! copying something), the backend becomes a requestor of the very selection it used to own,
//! asking the new owner for `UTF8_STRING` into the `APPRICOT_CLIPBOARD_FETCH` property of
//! its window. The answer, when it arrives in time, is one `ClipboardText` surface event.
//! The fetch is bounded in both directions: the reply property is read at
//! [`MAX_CLIPBOARD_BYTES`] plus one long, and a fetch that hears nothing by its deadline is
//! dropped, so a hostile app cannot stall the backend on a conversion that never comes.

use std::time::Instant;

use appricot_core::MAX_CLIPBOARD_BYTES;

/// The clipboard the backend serves and fetches: the text, or nothing.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Clipboard {
    /// The text the host last set, served to every requester.
    pub text: Option<String>,
    /// The server time the backend last took the selection at, from XFixes; 0 until the
    /// first notification says (the selection taken at connect reports none).
    pub acquired: u32,
    /// The fetch of the app's `UTF8_STRING` in flight, if one is.
    pub fetch: Option<ClipboardFetch>,
}

/// A fetch of the new owner's `UTF8_STRING`, started when the app took the selection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ClipboardFetch {
    /// When the fetch gives up. The backend never blocks on the answer; this bounds how
    /// long a late one is still believed to belong to a live fetch.
    pub deadline: Instant,
}

/// Reads at most this many 32-bit longs of a fetched reply: one byte past
/// [`MAX_CLIPBOARD_BYTES`], which is all it takes to tell a carriable text from one the wire
/// refuses — without ever reading the rest of a hostile owner's property.
#[allow(clippy::cast_possible_truncation)] // a const: try_from is not const-callable, and the cap (65536) fits a u32 by construction
pub(crate) const CLIPBOARD_FETCH_LONGS: u32 = (MAX_CLIPBOARD_BYTES / 4) as u32 + 1;

/// Decodes Latin-1 bytes (an X `STRING` property) into a `String`. Every byte is a
/// codepoint in Latin-1, so this cannot fail.
pub(crate) fn latin1_decode(bytes: &[u8]) -> String {
    bytes.iter().map(|&b| char::from(b)).collect()
}

/// Encodes text as Latin-1 for the `STRING` and `TEXT` targets. Characters beyond U+00FF
/// become `?`: the X target carries a single byte per character and the paste must not
/// silently truncate.
pub(crate) fn latin1_encode(text: &str) -> Vec<u8> {
    text.chars()
        .map(|c| u8::try_from(u32::from(c)).unwrap_or(b'?'))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{Clipboard, latin1_decode, latin1_encode};

    #[test]
    fn latin1_decodes_byte_by_byte() {
        assert_eq!(latin1_decode(&[0x61, 0xe9, 0xff]), "a\u{e9}\u{ff}");
    }

    #[test]
    fn latin1_encodes_within_the_byte_range() {
        assert_eq!(latin1_encode("a\u{e9}\u{ff}"), vec![0x61, 0xe9, 0xff]);
    }

    #[test]
    fn latin1_replaces_what_it_cannot_carry() {
        assert_eq!(latin1_encode("αβ"), vec![b'?', b'?']);
        assert_eq!(latin1_encode("a—b"), vec![b'a', b'?', b'b']);
    }

    #[test]
    fn an_empty_clipboard_holds_nothing() {
        assert_eq!(Clipboard::default().text, None);
        assert!(Clipboard::default().fetch.is_none());
    }
}
