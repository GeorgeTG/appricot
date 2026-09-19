//! The X server the tests run against has what the S1 backend needs: Composite 0.4 or newer,
//! Damage, XTEST and XFixes.
//!
//! This test needs a running X server, and it never skips. Without `DISPLAY` it panics and
//! says where to run it, because a suite that skips looks like a suite that passes.

use appricot_x11::{MIN_COMPOSITE, REQUIRED_EXTENSIONS, probe_extensions};
use x11rb::connection::RequestConnection;

const RUN_HINT: &str = "run inside the dev container: docker compose run --rm dev just test";

#[test]
fn the_x_server_has_composite_damage_xtest_and_xfixes() {
    let display = std::env::var("DISPLAY").unwrap_or_default();
    assert!(!display.is_empty(), "DISPLAY is not set; {RUN_HINT}");

    let (conn, _screen) = match x11rb::connect(Some(display.as_str())) {
        Ok(connected) => connected,
        Err(e) => panic!("cannot connect to {display:?}: {e}; {RUN_HINT}"),
    };

    for name in REQUIRED_EXTENSIONS {
        let info = conn.extension_information(name).expect("QueryExtension");
        assert!(info.is_some(), "no {name} extension on {display:?}");
    }

    let found = match probe_extensions(&conn) {
        Ok(found) => found,
        Err(e) => panic!("{display:?} cannot host appricot-x11: {e}"),
    };
    let composite = found.composite;
    assert!(
        composite >= MIN_COMPOSITE,
        "Composite {composite:?} is older than {MIN_COMPOSITE:?}"
    );
}
