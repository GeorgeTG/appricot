//! appricot-spike: the measuring tools of the M1 X11 capture spike (docs/spike/README.md).
//!
//! Dev-only. It measures the product from outside, the way a host and another X client would,
//! and it is never shipped:
//!
//! - [`observe`]: the application's window inventory and damage, as an X client on the display;
//! - [`record`]: a host on the wire that records frames, snapshots and tiles, and can drive
//!   input from a [`script`];
//! - [`bench`](mod@bench): the codec bench over the recorded tiles;
//! - [`mem`]: memory and CPU per process and per container;
//! - [`fake_app`]: a stand-in application for the harness's own tests.
//!
//! Every tool of one run shares a run directory and its [`control`] files.

pub mod bench;
pub mod control;
pub mod fake_app;
pub mod json;
pub mod mem;
pub mod observe;
pub mod png;
pub mod record;
pub mod script;
pub mod stats;
pub mod tiles;
