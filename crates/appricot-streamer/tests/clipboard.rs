//! Integration tests: the M2 clipboard row as the wire sees it (docs/roadmap.md M2: "Text
//! clipboard in both directions, only as the host's policy allows, and a clipboard write to
//! the user only inside a user gesture").
//!
//! The host-to-app direction is `ClipboardSet`: text capped by `MAX_CLIPBOARD_BYTES` must
//! reach [`appricot_core::InputSink::clipboard_set`] with the same UTF-8 bytes, and one byte
//! over the cap must end the session with `ServerError(1)` + `Bye(BYE_LIMIT_VIOLATION)`. The
//! app-to-host direction is `ClipboardAsk` (a backend that reports a paste it cannot serve
//! surfaces it to the client, and nothing else — the answer stays the host's decision) and
//! `ClipboardText` (an app copy surfaces once, identical consecutive text is not re-sent, a
//! host paste re-arms the rule, and a text the wire cannot carry sends nothing and kills
//! nothing).
//!
//! Every test spawns the in-process server on an ephemeral loopback port with the mock
//! backend from `common`, through the shared harness.

// The shared mock carries helpers this binary does not exercise; that is what sharing means.
#[allow(dead_code)]
mod common;

use appricot_core::SurfaceEvent;
use appricot_proto::limits::MAX_CLIPBOARD_BYTES;
use appricot_proto::wire::{Body, ByeReason};

use common::Input;
use common::harness::{
    Client, envelope, expect_error_and_bye, handshake, read_body, send, spawn_mock_server,
};

// -------------------------------------------------------------------------------------------
// Host to app: ClipboardSet
// -------------------------------------------------------------------------------------------

#[tokio::test]
async fn clipboard_set_text_reaches_the_backend_with_the_same_utf8_bytes() {
    let (server, mock) = spawn_mock_server().await;
    let (mut ws, _reply) = handshake(&server.addr, None).await;

    // Multi-byte text first: what arrives must be the same UTF-8, not a mangled copy.
    let greek = "Γειά σου, πρόχειρο!".to_owned();
    send(
        &mut ws,
        &envelope(Body::ClipboardSet(appricot_proto::wire::ClipboardSet {
            text: greek.clone(),
        })),
    )
    .await;

    // Then exactly at the cap: MAX_CLIPBOARD_BYTES bytes of two-byte characters. The cap is
    // inclusive, and a boundary-sized paste is legal traffic.
    let at_cap = "α".repeat(MAX_CLIPBOARD_BYTES / 2);
    assert_eq!(
        at_cap.len(),
        MAX_CLIPBOARD_BYTES,
        "the fixture must be exactly at the cap in UTF-8 bytes"
    );
    send(
        &mut ws,
        &envelope(Body::ClipboardSet(appricot_proto::wire::ClipboardSet {
            text: at_cap.clone(),
        })),
    )
    .await;

    assert_eq!(
        mock.wait_input(2).await,
        vec![
            Input::Clipboard { text: greek },
            Input::Clipboard { text: at_cap },
        ],
        "the backend receives the same text, byte for byte, in order"
    );
}

#[tokio::test]
async fn a_clipboard_set_one_byte_over_the_cap_closes_with_server_error_and_bye() {
    let (server, _mock) = spawn_mock_server().await;
    let (mut ws, _reply) = handshake(&server.addr, None).await;

    // Envelope.clipboard_set is field 22, wire type 2; ClipboardSet.text is field 1, wire
    // type 2. Hand-built so the encoder's own gate cannot refuse it first: this is what a
    // hostile peer sends, one byte over MAX_CLIPBOARD_BYTES.
    send(&mut ws, &crafted_oversized_clipboard_set()).await;
    expect_error_and_bye(&mut ws, 1, ByeReason::ByeLimitViolation).await;
}

/// Protobuf bytes of `Envelope.clipboard_set` = `ClipboardSet.text` of
/// `MAX_CLIPBOARD_BYTES + 1` bytes, encoded by hand (field 22 of Envelope, wire type 2;
/// field 1 of ClipboardSet, wire type 2).
fn crafted_oversized_clipboard_set() -> Vec<u8> {
    fn varint(mut v: u64) -> Vec<u8> {
        let mut out = Vec::new();
        loop {
            let mut byte = (v & 0x7f) as u8;
            v >>= 7;
            if v != 0 {
                byte |= 0x80;
            }
            out.push(byte);
            if v == 0 {
                return out;
            }
        }
    }

    let text_len = (MAX_CLIPBOARD_BYTES + 1) as u64;
    let mut clipboard_set = vec![0x0a]; // ClipboardSet.text: field 1, wire type 2
    clipboard_set.extend(varint(text_len));
    clipboard_set.extend(vec![
        b'A';
        usize::try_from(text_len)
            .expect("fits the cap plus one")
    ]);

    let mut envelope = vec![0xb2, 0x01]; // Envelope.clipboard_set: field 22, wire type 2
    envelope.extend(varint(clipboard_set.len() as u64));
    envelope.extend(clipboard_set);
    envelope
}

// -------------------------------------------------------------------------------------------
// App to host: ClipboardAsk
// -------------------------------------------------------------------------------------------

#[tokio::test]
async fn a_backend_paste_request_surfaces_as_clipboard_ask_on_the_wire() {
    let (server, mock) = spawn_mock_server().await;
    let (mut ws, _reply) = handshake(&server.addr, None).await;

    // The app pasted and the backend holds no text: the host must be asked.
    mock.push(SurfaceEvent::ClipboardRequested);

    match read_body(&mut ws).await {
        Body::ClipboardAsk(_) => {} // the whole message: the host decides what, if anything
        other => panic!("expected a ClipboardAsk, got {other:?}"),
    }
}

// -------------------------------------------------------------------------------------------
// App to host: ClipboardText
// -------------------------------------------------------------------------------------------

/// The text of one `ClipboardText` body, or a panic naming what came instead.
async fn expect_clipboard_text(ws: &mut Client, expected: &str) {
    match read_body(ws).await {
        Body::ClipboardText(m) => assert_eq!(m.text, expected, "the copied text, byte for byte"),
        other => panic!("expected a ClipboardText, got {other:?}"),
    }
}

/// Nothing arrives for `millis` milliseconds; the session is alive to prove it afterwards.
async fn expect_quiet(ws: &mut Client, millis: u64, why: &str) {
    // A quiet timeout is the pass; anything that arrived names the failure.
    if let Ok(body) =
        tokio::time::timeout(std::time::Duration::from_millis(millis), read_body(ws)).await
    {
        panic!("{why}: got {body:?}");
    }
}

#[tokio::test]
async fn an_app_copy_surfaces_once_and_identical_text_is_not_resent() {
    let (server, mock) = spawn_mock_server().await;
    let (mut ws, _reply) = handshake(&server.addr, None).await;

    // The app copied Greek text: the host receives it once, as untrusted text.
    mock.push(SurfaceEvent::ClipboardText {
        text: "αντιγραμμένο".into(),
    });
    expect_clipboard_text(&mut ws, "αντιγραμμένο").await;

    // The app copied the same text again: nothing. A different text: one message.
    mock.push(SurfaceEvent::ClipboardText {
        text: "αντιγραμμένο".into(),
    });
    expect_quiet(&mut ws, 300, "identical consecutive text is not re-sent").await;
    mock.push(SurfaceEvent::ClipboardText {
        text: "άλλο".into(),
    });
    expect_clipboard_text(&mut ws, "άλλο").await;
}

#[tokio::test]
async fn a_host_paste_re_arms_the_not_twice_rule() {
    let (server, mock) = spawn_mock_server().await;
    let (mut ws, _reply) = handshake(&server.addr, None).await;

    mock.push(SurfaceEvent::ClipboardText { text: "α".into() });
    expect_clipboard_text(&mut ws, "α").await;

    // The host pastes; the streamer owns the selection again.
    send(
        &mut ws,
        &envelope(Body::ClipboardSet(appricot_proto::wire::ClipboardSet {
            text: "pasted".into(),
        })),
    )
    .await;
    assert_eq!(
        mock.wait_input(1).await,
        vec![Input::Clipboard {
            text: "pasted".into()
        }]
    );

    // The app copies the very same text it copied before: the paste made it a change again.
    mock.push(SurfaceEvent::ClipboardText { text: "α".into() });
    expect_clipboard_text(&mut ws, "α").await;
}

#[tokio::test]
async fn an_over_cap_copy_sends_nothing_and_ends_nothing() {
    let (server, mock) = spawn_mock_server().await;
    let (mut ws, _reply) = handshake(&server.addr, None).await;

    // A text the wire cannot carry reports nothing - and is no session fault.
    mock.push(SurfaceEvent::ClipboardText {
        text: "x".repeat(MAX_CLIPBOARD_BYTES + 1),
    });
    expect_quiet(&mut ws, 300, "an over-cap text is dropped whole").await;

    // The session is alive and still coalesces: the next copy is served.
    mock.push(SurfaceEvent::ClipboardText {
        text: "после".into(),
    });
    expect_clipboard_text(&mut ws, "после").await;
}
