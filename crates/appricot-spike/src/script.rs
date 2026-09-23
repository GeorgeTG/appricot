//! Input scripts: what the recorder does as a host, step by step.
//!
//! A script drives the application through the streamer exactly as a browser client would:
//! `FocusNotify`, `PointerMove`, `PointerButton`, `PointerAxis` and `Key` on the wire, which the
//! streamer turns into XTEST input. So the spike's keyboard check (a Greek word, an AltGr
//! character, a dead-key accent) runs the real input path, repeatably.
//!
//! One command per line; `#` starts a comment; blank lines are skipped.
//!
//! | Command | Meaning |
//! |---|---|
//! | `wait <ms>` | Pause. |
//! | `wait-for toplevel <n> [<timeout-ms>]` | Wait until the n-th toplevel (1-based, in announce order) is announced. |
//! | `wait-for popup [<timeout-ms>]` | Wait until a popup is announced after this point. |
//! | `focus [<n>]` | Send `FocusNotify` for the n-th toplevel (default 1); it becomes the input target. |
//! | `target toplevel <n>` / `target popup` | Aim pointer input at the n-th toplevel, or at the newest live popup, without a focus change. |
//! | `move <x> <y>` | Pointer to surface coordinates. |
//! | `click <x> <y> [<button>]` | Move, press, release (button 1 by default). |
//! | `wheel <steps>` | Wheel steps, positive down. |
//! | `type <text>` | Every character of the rest of the line as a key tap, sent as a browser sends it (docs/protocol/v0.md §8). |
//! | `key <name or 0xKEYSYM> [code=<Code>] [mods=<n>] [down\|up]` | One key: a tap, or only its press or release. |
//! | `mark <label>` | Set the run's scenario label. |
//! | `snap <label>` | One PNG per live surface. |
//! | `stop` | End the run. |
//!
//! Key names: `Return`, `Tab`, `BackSpace`, `Escape`, `Delete`, `Home`, `End`, `Left`, `Up`,
//! `Right`, `Down`, `PageUp`, `PageDown`, `space`, `Shift_L`, `Control_L`, `Alt_L`,
//! `ISO_Level3_Shift` (AltGr) and `F1` to `F12`.

use std::time::Duration;

/// A pause between a key's press and its release, and between characters of `type`, so the
/// application sees keys at a human pace rather than all in one read.
pub const KEY_GAP: Duration = Duration::from_millis(20);

/// A pause between the steps of a click.
pub const CLICK_GAP: Duration = Duration::from_millis(40);

/// The default for `wait-for`.
pub const DEFAULT_WAIT_FOR: Duration = Duration::from_secs(30);

/// The two roles a script can wait for or aim at.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RoleKind {
    /// A toplevel.
    Toplevel,
    /// A popup.
    Popup,
}

/// Whether a key step presses, releases, or both.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyPhase {
    /// Press only.
    Down,
    /// Release only.
    Up,
}

/// One step, after expansion (a `click` or a `type` becomes several).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Step {
    /// Pause.
    Wait(Duration),
    /// Wait for an announcement: the n-th toplevel, or the next popup.
    WaitFor {
        /// Which role.
        role: RoleKind,
        /// For toplevels, which one (1-based); ignored for popups.
        nth: usize,
        /// Give up, and fail the run, after this long.
        timeout: Duration,
    },
    /// `FocusNotify` for the n-th toplevel, which becomes the target.
    Focus(usize),
    /// Aim at the n-th toplevel.
    TargetToplevel(usize),
    /// Aim at the newest live popup.
    TargetPopup,
    /// Pointer to target coordinates.
    Move(i32, i32),
    /// A button press or release on the target.
    Button {
        /// X button number.
        button: u32,
        /// Press or release.
        pressed: bool,
    },
    /// Wheel steps, positive down.
    Wheel(i32),
    /// One key event.
    Key {
        /// The keysym, per v0 §8.
        keysym: u32,
        /// The physical code, never empty.
        code: String,
        /// The modifier bitmask of v0 §8.
        mods: u32,
        /// Press or release.
        pressed: bool,
    },
    /// Set the scenario label.
    Mark(String),
    /// Take snapshots.
    Snap(String),
    /// End the run.
    Stop,
}

/// A parsed script: its steps, each with the line it came from.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Script {
    /// The steps, in order, with their 1-based source line.
    pub steps: Vec<(usize, Step)>,
}

/// A line the parser could not read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScriptError {
    /// The 1-based line.
    pub line: usize,
    /// What is wrong with it.
    pub message: String,
}

impl std::fmt::Display for ScriptError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "script line {}: {}", self.line, self.message)
    }
}

impl std::error::Error for ScriptError {}

/// The keysym a browser client sends for the character `c` (docs/protocol/v0.md §8, C5):
/// printable Latin-1 is the codepoint itself, everything else is 0x0100_0000 plus the
/// codepoint.
pub fn keysym_for_char(c: char) -> u32 {
    let cp = u32::from(c);
    if (0x20..=0x7e).contains(&cp) || (0xa0..=0xff).contains(&cp) {
        cp
    } else {
        0x0100_0000 + cp
    }
}

/// The physical code a US layout gives `c`, as `KeyboardEvent.code` names it, or
/// `Unidentified` (C6: the code is never empty).
pub fn code_for_char(c: char) -> String {
    match c {
        'a'..='z' => format!("Key{}", c.to_ascii_uppercase()),
        'A'..='Z' => format!("Key{c}"),
        '0'..='9' => format!("Digit{c}"),
        ' ' => "Space".to_owned(),
        _ => "Unidentified".to_owned(),
    }
}

/// A named key: its keysym and its code.
pub fn named_key(name: &str) -> Option<(u32, &'static str)> {
    let fixed = match name {
        "Return" => (0xff0d, "Enter"),
        "Tab" => (0xff09, "Tab"),
        "BackSpace" => (0xff08, "Backspace"),
        "Escape" => (0xff1b, "Escape"),
        "Delete" => (0xffff, "Delete"),
        "Home" => (0xff50, "Home"),
        "End" => (0xff57, "End"),
        "Left" => (0xff51, "ArrowLeft"),
        "Up" => (0xff52, "ArrowUp"),
        "Right" => (0xff53, "ArrowRight"),
        "Down" => (0xff54, "ArrowDown"),
        "PageUp" => (0xff55, "PageUp"),
        "PageDown" => (0xff56, "PageDown"),
        "space" => (0x20, "Space"),
        "Shift_L" => (0xffe1, "ShiftLeft"),
        "Control_L" => (0xffe3, "ControlLeft"),
        "Alt_L" => (0xffe9, "AltLeft"),
        "ISO_Level3_Shift" => (0xfe03, "AltRight"),
        _ => {
            const F_CODES: [&str; 12] = [
                "F1", "F2", "F3", "F4", "F5", "F6", "F7", "F8", "F9", "F10", "F11", "F12",
            ];
            let n: u32 = name.strip_prefix('F')?.parse().ok()?;
            let index = usize::try_from(n.checked_sub(1)?).ok()?;
            return F_CODES.get(index).map(|code| (0xffbd + n, *code));
        }
    };
    Some(fixed)
}

/// Parses a script.
pub fn parse(text: &str) -> Result<Script, ScriptError> {
    let mut steps = Vec::new();
    for (index, raw) in text.lines().enumerate() {
        let line = index + 1;
        let err = |message: &str| ScriptError {
            line,
            message: message.to_owned(),
        };
        let trimmed = raw.trim_start();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let (command, rest) = trimmed.split_once(' ').unwrap_or((trimmed, ""));
        let args: Vec<&str> = rest.split_whitespace().collect();
        let mut push = |step: Step| steps.push((line, step));
        match command {
            "wait" => push(Step::Wait(Duration::from_millis(num(&args, 0, &err)?))),
            "wait-for" => {
                let (role, nth, timeout_at) = match args.first() {
                    Some(&"toplevel") => (RoleKind::Toplevel, num(&args, 1, &err)?, 2),
                    Some(&"popup") => (RoleKind::Popup, 1, 1),
                    _ => return Err(err("wait-for needs `toplevel <n>` or `popup`")),
                };
                let timeout = match args.get(timeout_at) {
                    Some(_) => Duration::from_millis(num(&args, timeout_at, &err)?),
                    None => DEFAULT_WAIT_FOR,
                };
                push(Step::WaitFor { role, nth, timeout });
            }
            "focus" => push(Step::Focus(if args.is_empty() {
                1
            } else {
                num(&args, 0, &err)?
            })),
            "target" => match args.first() {
                Some(&"toplevel") => push(Step::TargetToplevel(num(&args, 1, &err)?)),
                Some(&"popup") => push(Step::TargetPopup),
                _ => return Err(err("target needs `toplevel <n>` or `popup`")),
            },
            "move" => push(Step::Move(int(&args, 0, &err)?, int(&args, 1, &err)?)),
            "click" => {
                let (x, y) = (int(&args, 0, &err)?, int(&args, 1, &err)?);
                let button = if args.len() > 2 {
                    num(&args, 2, &err)?
                } else {
                    1
                };
                push(Step::Move(x, y));
                push(Step::Wait(CLICK_GAP));
                push(Step::Button {
                    button,
                    pressed: true,
                });
                push(Step::Wait(CLICK_GAP));
                push(Step::Button {
                    button,
                    pressed: false,
                });
            }
            "wheel" => push(Step::Wheel(int(&args, 0, &err)?)),
            "type" => {
                if rest.is_empty() {
                    return Err(err("type needs text"));
                }
                for c in rest.chars() {
                    let (keysym, code) = (keysym_for_char(c), code_for_char(c));
                    for pressed in [true, false] {
                        push(Step::Key {
                            keysym,
                            code: code.clone(),
                            mods: 0,
                            pressed,
                        });
                        push(Step::Wait(KEY_GAP));
                    }
                }
            }
            "key" => {
                for step in parse_key(&args, &err)? {
                    push(step);
                }
            }
            "mark" | "snap" => {
                let label = crate::control::sanitize_label(rest);
                if label.is_empty() {
                    return Err(err("needs a label"));
                }
                push(if command == "mark" {
                    Step::Mark(label)
                } else {
                    Step::Snap(label)
                });
            }
            "stop" => push(Step::Stop),
            _ => return Err(err(&format!("unknown command `{command}`"))),
        }
    }
    Ok(Script { steps })
}

fn parse_key(args: &[&str], err: &dyn Fn(&str) -> ScriptError) -> Result<Vec<Step>, ScriptError> {
    let Some(name) = args.first() else {
        return Err(err("key needs a name or a keysym"));
    };
    let (keysym, mut code) = if let Some(hex) = name.strip_prefix("0x") {
        let keysym = u32::from_str_radix(hex, 16).map_err(|_| err("bad hex keysym"))?;
        (keysym, "Unidentified".to_owned())
    } else {
        let (keysym, code) = named_key(name).ok_or_else(|| err("unknown key name"))?;
        (keysym, code.to_owned())
    };
    let mut mods = 0;
    let mut phase = None;
    for arg in &args[1..] {
        if let Some(c) = arg.strip_prefix("code=") {
            if c.is_empty() {
                return Err(err("code= needs a value"));
            }
            c.clone_into(&mut code);
        } else if let Some(m) = arg.strip_prefix("mods=") {
            mods = m.parse().map_err(|_| err("mods= needs a number"))?;
        } else if *arg == "down" {
            phase = Some(KeyPhase::Down);
        } else if *arg == "up" {
            phase = Some(KeyPhase::Up);
        } else {
            return Err(err(&format!("unknown key argument `{arg}`")));
        }
    }
    let event = |pressed| Step::Key {
        keysym,
        code: code.clone(),
        mods,
        pressed,
    };
    Ok(match phase {
        Some(KeyPhase::Down) => vec![event(true), Step::Wait(KEY_GAP)],
        Some(KeyPhase::Up) => vec![event(false), Step::Wait(KEY_GAP)],
        None => vec![
            event(true),
            Step::Wait(KEY_GAP),
            event(false),
            Step::Wait(KEY_GAP),
        ],
    })
}

fn num<T: std::str::FromStr>(
    args: &[&str],
    at: usize,
    err: &dyn Fn(&str) -> ScriptError,
) -> Result<T, ScriptError> {
    args.get(at)
        .ok_or_else(|| err("missing a number"))?
        .parse()
        .map_err(|_| err("not a number"))
}

fn int(args: &[&str], at: usize, err: &dyn Fn(&str) -> ScriptError) -> Result<i32, ScriptError> {
    num(args, at, err)
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::{
        CLICK_GAP, KEY_GAP, RoleKind, Step, code_for_char, keysym_for_char, named_key, parse,
    };

    #[test]
    fn characters_get_the_keysyms_a_browser_sends() {
        assert_eq!(keysym_for_char('a'), 0x61);
        assert_eq!(keysym_for_char('é'), 0xe9);
        assert_eq!(keysym_for_char('λ'), 0x0100_03bb);
        assert_eq!(keysym_for_char('ά'), 0x0100_03ac);
        assert_eq!(keysym_for_char('€'), 0x0100_20ac);
        assert_eq!(code_for_char('q'), "KeyQ");
        assert_eq!(code_for_char('7'), "Digit7");
        assert_eq!(code_for_char('λ'), "Unidentified");
    }

    #[test]
    fn named_keys_carry_their_codes() {
        assert_eq!(named_key("Return"), Some((0xff0d, "Enter")));
        assert_eq!(named_key("ISO_Level3_Shift"), Some((0xfe03, "AltRight")));
        assert_eq!(named_key("F1"), Some((0xffbe, "F1")));
        assert_eq!(named_key("F12"), Some((0xffc9, "F12")));
        assert_eq!(named_key("F13"), None);
        assert_eq!(named_key("F0"), None);
        assert_eq!(named_key("Hyper"), None);
    }

    #[test]
    fn a_script_expands_clicks_and_typing() {
        let script = parse(
            "# log in\n\
             wait-for toplevel 1 5000\n\
             focus\n\
             click 10 20\n\
             type λά\n\
             key Return\n\
             mark after login\n\
             snap login\n\
             stop\n",
        )
        .expect("a valid script");
        let steps: Vec<_> = script.steps.iter().map(|(_, s)| s.clone()).collect();
        assert_eq!(
            steps[0],
            Step::WaitFor {
                role: RoleKind::Toplevel,
                nth: 1,
                timeout: Duration::from_secs(5)
            }
        );
        assert_eq!(steps[1], Step::Focus(1));
        assert_eq!(
            &steps[2..7],
            &[
                Step::Move(10, 20),
                Step::Wait(CLICK_GAP),
                Step::Button {
                    button: 1,
                    pressed: true
                },
                Step::Wait(CLICK_GAP),
                Step::Button {
                    button: 1,
                    pressed: false
                },
            ]
        );
        // Two characters, each a press and a release with a gap after each.
        assert_eq!(
            steps[7],
            Step::Key {
                keysym: 0x0100_03bb,
                code: "Unidentified".into(),
                mods: 0,
                pressed: true
            }
        );
        assert_eq!(steps[8], Step::Wait(KEY_GAP));
        assert_eq!(steps[14], Step::Wait(KEY_GAP));
        assert_eq!(
            steps[15],
            Step::Key {
                keysym: 0xff0d,
                code: "Enter".into(),
                mods: 0,
                pressed: true
            }
        );
        assert_eq!(steps[19], Step::Mark("after_login".into()));
        assert_eq!(steps[20], Step::Snap("login".into()));
        assert_eq!(steps[21], Step::Stop);
        assert_eq!(script.steps[0].0, 2, "steps keep their source line");
    }

    #[test]
    fn key_arguments_override_the_code_and_split_the_tap() {
        let script = parse("key 0x010020ac code=KeyE mods=32 down").expect("valid");
        assert_eq!(
            script.steps[0].1,
            Step::Key {
                keysym: 0x0100_20ac,
                code: "KeyE".into(),
                mods: 32,
                pressed: true
            }
        );
        assert_eq!(script.steps.len(), 2);
    }

    #[test]
    fn errors_name_their_line() {
        let err = parse("wait 10\nfly 1 2\n").expect_err("an unknown command");
        assert_eq!(err.line, 2);
        assert!(err.message.contains("fly"));
        assert_eq!(parse("click 1").expect_err("missing y").line, 1);
        assert!(parse("key Hyper").is_err());
        assert!(parse("key a code=").is_err());
        assert!(parse("type").is_err());
        assert!(parse("snap   ").is_err());
    }
}
