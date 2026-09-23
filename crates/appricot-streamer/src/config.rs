//! The `serve` configuration: every knob is an environment variable.
//!
//! | Variable | Meaning | Default |
//! |---|---|---|
//! | `APPRICOT_BIND` | Where the server listens. `loopback:<port>` binds `127.0.0.1` on that port (port 0 lets the OS choose); `unix:<path>` binds a unix domain socket. Nothing else is accepted. | `loopback:0` |
//! | `APPRICOT_STREAM_TOKEN` | The per-session stream token the first client message must carry. Empty or longer than the wire cap (`MAX_TOKEN_BYTES`) is refused, because no valid Hello could ever match it. | none; `serve` refuses to start |
//! | `APPRICOT_DISPLAY` | The X display to connect to, as `$DISPLAY` spells it. | `$DISPLAY` |
//! | `APPRICOT_LOG` | A `tracing` filter directive (the same syntax `EnvFilter` takes). | `info` |
//! | `APPRICOT_RESUME_GRACE_MS` | Overrides the resume grace this process honours — both the window a parked session waits out and the `resume_grace_ms` the `HelloReply` advertises, so the reply always names what the server will actually honour. It exists for tests and the manual demo only; the protocol's value stays the limits table's. | `RESUME_GRACE_MS` (10 s) |
//!
//! The bind grammar is closed on purpose: the streamer runs inside the app's sandbox and must
//! never listen on an address another container or the host could reach. A value that is neither
//! `loopback:` nor `unix:` is an error, not a fallback.
//!
//! A variable that is set but is not valid UTF-8 is an error that names it, never a silent
//! fallback: a garbled `APPRICOT_BIND` must not quietly bind an ephemeral port. The token is the
//! one exception on unix, where its raw bytes are taken as they are, because the wire carries the
//! token as bytes.
//!
//! The grace override is process state, not a field of [`Config`]: the session pump reads it
//! where it parks a session (see [`resume_grace_ms`]), so every reader in the process sees one
//! truth. Tests set it with [`set_resume_grace_ms`] in-process instead of through the
//! environment, which no test may mutate while others run. The handshake deadline
//! ([`handshake_timeout_ms`]) is process state in the same way, with no variable at all: only a
//! test shortens it.

use std::env;
use std::ffi::OsString;
use std::fmt;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};

use crate::auth::StreamToken;

/// Most bytes a stream token may carry; the wire's `MAX_TOKEN_BYTES`
/// (crates/appricot-proto/src/limits.rs). A token longer than this can never match a Hello the
/// bounded decoder accepts, so holding one would be a misconfiguration, not a secret.
pub use appricot_proto::limits::MAX_TOKEN_BYTES;

/// The resume grace a process honours when nothing overrides it: the wire's `RESUME_GRACE_MS`
/// (crates/appricot-proto/src/limits.rs).
use appricot_proto::limits::RESUME_GRACE_MS;

/// Where the server listens.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Bind {
    /// `127.0.0.1` on this port. Port 0 asks the OS for a free one.
    Loopback {
        /// The TCP port.
        port: u16,
    },
    /// A unix domain socket at this path.
    Unix {
        /// The socket path.
        path: PathBuf,
    },
}

impl fmt::Display for Bind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Loopback { port } => write!(f, "loopback:{port}"),
            Self::Unix { path } => write!(f, "unix:{}", path.display()),
        }
    }
}

/// A `serve` configuration, already validated.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    /// Where the server listens.
    pub bind: Bind,
    /// The stream token, as bytes (the wire carries it as bytes). Redacted in `Debug`.
    pub token: StreamToken,
    /// The X display to connect to; `None` means `$DISPLAY`.
    pub display: Option<String>,
    /// The `tracing` filter directive.
    pub log_filter: String,
}

/// Why a configuration is refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfigError {
    /// `APPRICOT_BIND` is not `loopback:<port>` or `unix:<path>`. The offending value.
    BadBind(String),
    /// The port in `loopback:<port>` is not a `u16`. The offending value.
    BadPort(String),
    /// `APPRICOT_STREAM_TOKEN` is missing.
    NoToken,
    /// `APPRICOT_STREAM_TOKEN` is empty.
    EmptyToken,
    /// `APPRICOT_STREAM_TOKEN` is longer than the wire cap. The length.
    TokenTooLong(usize),
    /// `APPRICOT_RESUME_GRACE_MS` is not a whole number of milliseconds, `1` or more. The
    /// offending value.
    BadResumeGrace(String),
    /// The named variable is set, but its value is not valid UTF-8.
    NotUnicode(&'static str),
}

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::BadBind(v) => {
                write!(
                    f,
                    "APPRICOT_BIND must be loopback:<port> or unix:<path>, got {v:?}"
                )
            }
            Self::BadPort(v) => write!(f, "the port in APPRICOT_BIND is not a number: {v:?}"),
            Self::NoToken => f.write_str(
                "APPRICOT_STREAM_TOKEN is not set; the streamer refuses to serve without it",
            ),
            Self::EmptyToken => f.write_str("APPRICOT_STREAM_TOKEN is empty"),
            Self::TokenTooLong(len) => write!(
                f,
                "APPRICOT_STREAM_TOKEN is {len} bytes, over the wire cap of {MAX_TOKEN_BYTES}"
            ),
            Self::BadResumeGrace(v) => write!(
                f,
                "APPRICOT_RESUME_GRACE_MS must be a whole number of milliseconds, 1 or more, got {v:?}"
            ),
            Self::NotUnicode(name) => write!(f, "{name} is set but is not valid UTF-8"),
        }
    }
}

impl std::error::Error for ConfigError {}

/// Parses `loopback:<port>` and `unix:<path>`; anything else is [`ConfigError::BadBind`].
///
/// The forms are exact: no `0.0.0.0`, no host names, no scheme. The refused value travels back
/// in the error so the operator sees what was read.
pub fn parse_bind(value: &str) -> Result<Bind, ConfigError> {
    if let Some(path) = value.strip_prefix("unix:") {
        if path.is_empty() {
            return Err(ConfigError::BadBind(value.to_owned()));
        }
        return Ok(Bind::Unix {
            path: PathBuf::from(path),
        });
    }
    if let Some(port) = value.strip_prefix("loopback:") {
        let port = port
            .parse::<u16>()
            .map_err(|_| ConfigError::BadPort(port.to_owned()))?;
        return Ok(Bind::Loopback { port });
    }
    Err(ConfigError::BadBind(value.to_owned()))
}

/// The resume-grace override, in milliseconds; `0` means "no override". Written by
/// [`set_resume_grace_ms`] only.
static RESUME_GRACE_OVERRIDE_MS: AtomicU32 = AtomicU32::new(0);

/// The resume grace this process honours, in milliseconds.
///
/// This is both the window a parked session waits out (docs/protocol/v0.md §7) and the value
/// `HelloReply.resume_grace_ms` advertises, so the reply always names what the server will
/// actually honour. It is [`RESUME_GRACE_MS`] unless [`set_resume_grace_ms`] — or
/// `APPRICOT_RESUME_GRACE_MS`, applied by [`from_env`] — overrode it.
pub fn resume_grace_ms() -> u32 {
    match RESUME_GRACE_OVERRIDE_MS.load(Ordering::Acquire) {
        0 => RESUME_GRACE_MS,
        ms => ms,
    }
}

/// Overrides the resume grace this process honours (see [`resume_grace_ms`]); `0` restores the
/// default.
///
/// The override is process-wide, which is the point: the session pump reads it where it parks a
/// session, without the grace having to travel through every signature between `serve` and the
/// park. It exists so a test can exercise grace expiry without waiting out the protocol's ten
/// seconds, and so the manual demo can shorten the wait from the outside. Nothing in production
/// sets it.
pub fn set_resume_grace_ms(ms: u32) {
    RESUME_GRACE_OVERRIDE_MS.store(ms, Ordering::Release);
}

/// Parses `APPRICOT_RESUME_GRACE_MS`: a whole number of milliseconds, `1` or more.
///
/// `0` is refused: a grace of zero would park a session and tear it down in the same breath,
/// which is what never parking it at all already means.
pub fn parse_resume_grace(value: &str) -> Result<u32, ConfigError> {
    match value.parse::<u32>() {
        Ok(ms) if ms >= 1 => Ok(ms),
        _ => Err(ConfigError::BadResumeGrace(value.to_owned())),
    }
}

/// How long an upgraded socket may take to deliver its `Hello`, in milliseconds, when nothing
/// overrides it (docs/protocol/v0.md §2). A socket that has not authenticated by then is refused
/// with `Bye(BYE_PROTOCOL_VIOLATION)` and the session slot is handed back.
pub const HANDSHAKE_TIMEOUT_MS: u32 = 5_000;

/// The handshake-deadline override, in milliseconds; `0` means "no override". Written by
/// [`set_handshake_timeout_ms`] only.
static HANDSHAKE_TIMEOUT_OVERRIDE_MS: AtomicU32 = AtomicU32::new(0);

/// The handshake deadline this process honours, in milliseconds: [`HANDSHAKE_TIMEOUT_MS`]
/// unless [`set_handshake_timeout_ms`] overrode it.
pub fn handshake_timeout_ms() -> u32 {
    match HANDSHAKE_TIMEOUT_OVERRIDE_MS.load(Ordering::Acquire) {
        0 => HANDSHAKE_TIMEOUT_MS,
        ms => ms,
    }
}

/// Overrides the handshake deadline this process honours; `0` restores the default.
///
/// Process-wide, like [`set_resume_grace_ms`], and for the same reason: it exists so a test can
/// watch a silent socket time out without waiting out the real deadline. Nothing in production
/// sets it.
pub fn set_handshake_timeout_ms(ms: u32) {
    HANDSHAKE_TIMEOUT_OVERRIDE_MS.store(ms, Ordering::Release);
}

/// Reads the configuration from the environment.
///
/// Fails, naming the variable, on every misconfiguration; the caller refuses to serve.
pub fn from_env() -> Result<Config, ConfigError> {
    let (config, grace) = from_vars(|name| env::var_os(name))?;
    // The test/demo knob: applied to the process (the session pump reads it there), not carried
    // in the struct — see the module docs.
    if let Some(ms) = grace {
        set_resume_grace_ms(ms);
    }
    Ok(config)
}

/// Reads the configuration through `get`, which answers one variable by name.
///
/// This is [`from_env`] without the environment, so it can be tested without mutating the
/// process. The resume-grace override comes back beside the [`Config`] instead of being
/// applied here; `from_env` applies it.
fn from_vars(get: impl Fn(&str) -> Option<OsString>) -> Result<(Config, Option<u32>), ConfigError> {
    let text = |name: &'static str| -> Result<Option<String>, ConfigError> {
        get(name)
            .map(|v| v.into_string().map_err(|_| ConfigError::NotUnicode(name)))
            .transpose()
    };

    let bind = match text("APPRICOT_BIND")? {
        Some(v) => parse_bind(&v)?,
        None => Bind::Loopback { port: 0 },
    };

    let token = token_bytes(get("APPRICOT_STREAM_TOKEN").ok_or(ConfigError::NoToken)?)?;
    if token.is_empty() {
        return Err(ConfigError::EmptyToken);
    }
    if token.len() > MAX_TOKEN_BYTES {
        return Err(ConfigError::TokenTooLong(token.len()));
    }

    let display = match text("APPRICOT_DISPLAY")? {
        Some(v) => Some(v),
        None => text("DISPLAY")?,
    };

    let log_filter = text("APPRICOT_LOG")?.unwrap_or_else(|| "info".to_owned());

    let grace = text("APPRICOT_RESUME_GRACE_MS")?
        .map(|v| parse_resume_grace(&v))
        .transpose()?;

    let config = Config {
        bind,
        token: StreamToken::new(token),
        display,
        log_filter,
    };
    Ok((config, grace))
}

/// The token's bytes: raw on unix, where the wire's `bytes` field can carry any of them.
#[cfg(unix)]
#[expect(
    clippy::unnecessary_wraps,
    reason = "the non-unix twin fails on a token that is not UTF-8; one signature for both"
)]
fn token_bytes(value: OsString) -> Result<Vec<u8>, ConfigError> {
    use std::os::unix::ffi::OsStringExt;
    Ok(value.into_vec())
}

/// The token's bytes: its UTF-8, where the platform has no raw form of a variable.
#[cfg(not(unix))]
fn token_bytes(value: OsString) -> Result<Vec<u8>, ConfigError> {
    value
        .into_string()
        .map(String::into_bytes)
        .map_err(|_| ConfigError::NotUnicode("APPRICOT_STREAM_TOKEN"))
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::ffi::OsString;

    use super::{
        Bind, ConfigError, MAX_TOKEN_BYTES, from_vars, parse_bind, parse_resume_grace,
        resume_grace_ms, set_resume_grace_ms,
    };

    fn unix(path: &str) -> Bind {
        Bind::Unix { path: path.into() }
    }

    /// A fake environment: these variables and nothing else.
    fn vars(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<OsString> {
        let map: HashMap<String, OsString> = pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), OsString::from(*v)))
            .collect();
        move |name| map.get(name).cloned()
    }

    #[test]
    fn from_vars_reads_every_variable() {
        let (config, grace) = from_vars(vars(&[
            ("APPRICOT_BIND", "loopback:8391"),
            ("APPRICOT_STREAM_TOKEN", "sesame"),
            ("APPRICOT_DISPLAY", ":7"),
            ("DISPLAY", ":99"),
            ("APPRICOT_LOG", "debug"),
            ("APPRICOT_RESUME_GRACE_MS", "250"),
        ]))
        .expect("a complete environment is accepted");
        assert_eq!(config.bind, Bind::Loopback { port: 8391 });
        assert!(config.token.matches(b"sesame"));
        assert_eq!(config.display.as_deref(), Some(":7"));
        assert_eq!(config.log_filter, "debug");
        assert_eq!(grace, Some(250));
    }

    #[test]
    fn from_vars_defaults_what_is_absent() {
        let (config, grace) =
            from_vars(vars(&[("APPRICOT_STREAM_TOKEN", "t"), ("DISPLAY", ":99")]))
                .expect("only the token is required");
        assert_eq!(config.bind, Bind::Loopback { port: 0 });
        assert_eq!(
            config.display.as_deref(),
            Some(":99"),
            "DISPLAY is the fallback"
        );
        assert_eq!(config.log_filter, "info");
        assert_eq!(grace, None);
    }

    #[test]
    fn from_vars_refuses_a_missing_empty_or_long_token() {
        assert_eq!(from_vars(vars(&[])).err(), Some(ConfigError::NoToken));
        assert_eq!(
            from_vars(vars(&[("APPRICOT_STREAM_TOKEN", "")])).err(),
            Some(ConfigError::EmptyToken)
        );
        let long = "x".repeat(MAX_TOKEN_BYTES + 1);
        assert_eq!(
            from_vars(vars(&[("APPRICOT_STREAM_TOKEN", &long)])).err(),
            Some(ConfigError::TokenTooLong(MAX_TOKEN_BYTES + 1))
        );
    }

    #[test]
    fn a_config_never_formats_its_token() {
        let (config, _) =
            from_vars(vars(&[("APPRICOT_STREAM_TOKEN", "hunter2-secret")])).expect("accepted");
        let shown = format!("{config:?}");
        assert!(
            !shown.contains("hunter2"),
            "Debug leaked the token: {shown}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_variable_that_is_not_utf8_is_named_not_ignored() {
        use std::os::unix::ffi::OsStringExt;

        let garbled = OsString::from_vec(vec![b'l', b'o', 0xff, 0xfe]);
        for name in [
            "APPRICOT_BIND",
            "APPRICOT_DISPLAY",
            "APPRICOT_LOG",
            "APPRICOT_RESUME_GRACE_MS",
        ] {
            let bad = garbled.clone();
            let env = move |asked: &str| match asked {
                "APPRICOT_STREAM_TOKEN" => Some(OsString::from("t")),
                n if n == name => Some(bad.clone()),
                _ => None,
            };
            assert_eq!(
                from_vars(env).err(),
                Some(ConfigError::NotUnicode(name)),
                "{name} set to non-UTF-8 is an error, not a fallback"
            );
        }

        // The token is bytes on the wire, so its raw bytes are taken as they are.
        let raw = garbled.clone();
        let env = move |asked: &str| (asked == "APPRICOT_STREAM_TOKEN").then(|| raw.clone());
        let (config, _) = from_vars(env).expect("a binary token is a token");
        assert!(config.token.matches(&[b'l', b'o', 0xff, 0xfe]));
    }

    #[test]
    fn loopback_with_a_port() {
        assert_eq!(
            parse_bind("loopback:8080"),
            Ok(Bind::Loopback { port: 8080 })
        );
        assert_eq!(parse_bind("loopback:0"), Ok(Bind::Loopback { port: 0 }));
    }

    #[test]
    fn a_unix_socket_path() {
        assert_eq!(
            parse_bind("unix:/run/appricot/stream.sock"),
            Ok(unix("/run/appricot/stream.sock"))
        );
    }

    #[test]
    fn a_public_address_is_refused() {
        assert_eq!(
            parse_bind("0.0.0.0:8080"),
            Err(ConfigError::BadBind("0.0.0.0:8080".to_owned()))
        );
        assert_eq!(
            parse_bind("192.168.1.4:8080"),
            Err(ConfigError::BadBind("192.168.1.4:8080".to_owned()))
        );
        assert_eq!(
            parse_bind("[::]:8080"),
            Err(ConfigError::BadBind("[::]:8080".to_owned()))
        );
    }

    #[test]
    fn hostnames_are_refused() {
        assert_eq!(
            parse_bind("streamer.local:8080"),
            Err(ConfigError::BadBind("streamer.local:8080".to_owned()))
        );
        assert_eq!(
            parse_bind("localhost:8080"),
            Err(ConfigError::BadBind("localhost:8080".to_owned()))
        );
    }

    #[test]
    fn a_port_that_is_not_a_port_is_refused() {
        assert_eq!(
            parse_bind("loopback:notaport"),
            Err(ConfigError::BadPort("notaport".to_owned()))
        );
        assert_eq!(
            parse_bind("loopback:65536"),
            Err(ConfigError::BadPort("65536".to_owned()))
        );
    }

    #[test]
    fn an_empty_form_is_refused() {
        assert_eq!(parse_bind(""), Err(ConfigError::BadBind(String::new())));
        assert_eq!(
            parse_bind("unix:"),
            Err(ConfigError::BadBind("unix:".to_owned()))
        );
    }

    #[test]
    fn the_token_cap_matches_the_limits_table() {
        // The wire table (crates/appricot-proto/proto/appricot/v0/wire.proto) caps a token at
        // 256 bytes; the env gate must refuse the same length a Hello could never carry.
        assert_eq!(MAX_TOKEN_BYTES, 256);
    }

    #[test]
    fn a_grace_is_parsed_in_whole_milliseconds() {
        assert_eq!(parse_resume_grace("300"), Ok(300));
        assert_eq!(parse_resume_grace("4294967295"), Ok(u32::MAX));
        // Zero parks and tears down in the same breath; anything not a positive whole number
        // of milliseconds is refused with the value that was read.
        for refused in ["0", "-5", "1.5", "soon", ""] {
            assert!(
                matches!(parse_resume_grace(refused), Err(ConfigError::BadResumeGrace(v)) if v == refused),
                "{refused:?} must be refused"
            );
        }
    }

    #[test]
    fn a_grace_override_applies_and_zero_restores_the_default() {
        // Process state, touched by this test alone in this binary; restored before the end so
        // the default holds wherever the binary runs next.
        set_resume_grace_ms(300);
        assert_eq!(resume_grace_ms(), 300);
        set_resume_grace_ms(0);
        assert_eq!(resume_grace_ms(), super::RESUME_GRACE_MS);
    }
}
