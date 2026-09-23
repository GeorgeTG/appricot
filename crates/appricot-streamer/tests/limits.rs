//! The wire's outermost bound as the transport sees it: one WebSocket message over
//! `MAX_MESSAGE_BYTES`.
//!
//! The server configures its WebSocket layer with the wire's own cap, `MAX_MESSAGE_BYTES`, for
//! a whole message and for any one frame of it (tungstenite's defaults are 64 MiB and 16 MiB).
//! So a message too big for the wire never reaches the streamer's bounded decoder, however it
//! is cut: a single frame dies on its header while the sender is still writing (finding of
//! 2026-09-21, internal test over the real loopback server), and a message fragmented into
//! frames under the cap dies at the fragment that crosses it, before it is assembled. The
//! streamer-level enforcement (`ServerError` code 1 + `Bye(BYE_LIMIT_VIOLATION)`) still exists
//! and is proved by the hand-built oversized-token test in `tests/streamer.rs`: an envelope
//! inside the transport cap whose field lengths break the limits table gets the full refusal.
//! What this file pins is which layer answers when the envelope itself cannot even arrive.

// The shared mock carries helpers this binary does not exercise; that is what sharing means.
#[allow(dead_code)]
mod common;

use std::time::Duration;

use appricot_proto::limits::MAX_MESSAGE_BYTES;
use futures_util::{SinkExt, StreamExt};
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::protocol::frame::Frame;
use tokio_tungstenite::tungstenite::protocol::frame::coding::{CloseCode, Data, OpCode};

use common::harness::{Client, connect, handshake, spawn_mock_server};

/// Waits for the connection to end, and fails if any protocol message comes first: the
/// enforcement happened below the streamer, so no `ServerError` and no `Bye` precede the end.
async fn expect_ended_below_the_protocol(ws: &mut Client) {
    let ended = tokio::time::timeout(Duration::from_secs(15), ws.next())
        .await
        .expect("the oversized message ends the connection");
    match ended {
        // However the ending arrives — an outright close, a transport error, or a bare close
        // frame — no protocol envelope preceded it.
        None | Some(Err(_) | Ok(Message::Close(None))) => {}
        // A close frame from the WebSocket layer; tungstenite answers a size violation with
        // close code 1009 (message too big) when the peer gets that far.
        Some(Ok(Message::Close(Some(close)))) => {
            assert_eq!(
                close.code,
                CloseCode::Size,
                "the close names a size violation"
            );
        }
        Some(Ok(Message::Binary(bytes))) => panic!(
            "a {}-byte message reached the protocol layer; the transport cap did not fire",
            bytes.len()
        ),
        Some(Ok(other)) => panic!("expected the connection to end, got {other:?}"),
    }
}

/// Sends one binary message of `total` zero bytes cut into two frames, each under the frame
/// cap. The send may fail part-way once the server has cut the connection.
async fn send_in_two_fragments(ws: &mut Client, total: usize) -> bool {
    let first = total / 2;
    let head = Frame::message(vec![0u8; first], OpCode::Data(Data::Binary), false);
    let tail = Frame::message(vec![0u8; total - first], OpCode::Data(Data::Continue), true);
    ws.send(Message::Frame(head)).await.is_ok() && ws.send(Message::Frame(tail)).await.is_ok()
}

#[tokio::test]
async fn an_oversized_single_frame_message_never_reaches_the_handler() {
    let (server, _mock) = spawn_mock_server().await;
    let (mut ws, _reply) = handshake(&server.addr, None).await;

    // One binary WebSocket message one byte over the wire cap, sent as the single frame a
    // normal client emits. The transport's own frame cap (MAX_MESSAGE_BYTES) refuses it
    // first: the server reads the frame header, sees the size, and can kill the connection
    // while the client is still writing — the send itself may fail. The session handler never
    // sees these bytes.
    let oversized = vec![0u8; MAX_MESSAGE_BYTES + 1];
    if ws.send(Message::Binary(oversized.into())).await.is_ok() {
        expect_ended_below_the_protocol(&mut ws).await;
    }
}

#[tokio::test]
async fn a_fragmented_message_over_the_cap_is_refused_before_it_is_assembled() {
    let (server, _mock) = spawn_mock_server().await;
    let (mut ws, _reply) = handshake(&server.addr, None).await;

    // Two frames of 8 MiB each, one byte over the wire cap together. With the transport left at
    // its 64 MiB default, both would be assembled and copied, and the decoder would answer with
    // ServerError + Bye; with the cap set, the second fragment ends the connection first.
    if send_in_two_fragments(&mut ws, MAX_MESSAGE_BYTES + 1).await {
        expect_ended_below_the_protocol(&mut ws).await;
    }
}

#[tokio::test]
async fn a_fragmented_first_message_over_the_cap_claims_nothing() {
    let (server, _mock) = spawn_mock_server().await;
    let mut ws = connect(&server.addr).await;

    // The same flood as the first message, before any authentication: it is refused below the
    // protocol, and the slot it held goes back to the next client.
    if send_in_two_fragments(&mut ws, MAX_MESSAGE_BYTES + 1).await {
        expect_ended_below_the_protocol(&mut ws).await;
    }
    drop(ws);

    let (_ws, reply) = handshake(&server.addr, None).await;
    assert!(!reply.resumed, "the next client is served a fresh session");
}
