//! The physical key code: what the wire's `Key.code` may carry, and nothing else.

use appricot_core::KeyCode;
use appricot_proto::limits::MAX_KEY_CODE_BYTES;

#[test]
fn a_browser_code_is_accepted_as_is() {
    for code in ["KeyA", "ShiftLeft", "Numpad4", "Unidentified", "F24"] {
        let key = KeyCode::new(code).expect("a valid code");
        assert_eq!(key.as_str(), code);
    }
}

#[test]
fn the_cap_is_the_wires() {
    assert_eq!(KeyCode::MAX_BYTES, MAX_KEY_CODE_BYTES);
    let longest = "A".repeat(MAX_KEY_CODE_BYTES);
    assert!(KeyCode::new(&longest).is_some());
    let too_long = "A".repeat(MAX_KEY_CODE_BYTES + 1);
    assert_eq!(KeyCode::new(&too_long), None);
}

#[test]
fn an_empty_or_non_ascii_code_is_refused() {
    assert_eq!(KeyCode::new(""), None);
    assert_eq!(KeyCode::new("Kéy"), None);
    // Short in characters, long in bytes: the cap counts bytes, and ASCII is required anyway.
    assert_eq!(KeyCode::new("ααααααααα"), None);
}
