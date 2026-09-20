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
//! The grace override is process state, not a field of [`Config`]: the session pump reads it
//! where it parks a session (see [`resume_grace_ms`]), so every reader in the process sees one
//! truth. Tests set it with [`set_resume_grace_ms`] in-process instead of through the
//! environment, which no test may mutate while others run.

use std::env;
use std::fmt;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};

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
    /// The stream token, as bytes (the wire carries it as bytes).
    pub token: Vec<u8>,
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

/// Reads the configuration from the environment.
///
/// Fails, naming the variable, on every misconfiguration; the caller refuses to serve.
pub fn from_env() -> Result<Config, ConfigError> {
    let bind = match env::var("APPRICOT_BIND") {
        Ok(v) => parse_bind(&v)?,
        Err(_) => Bind::Loopback { port: 0 },
    };

    let token = env::var("APPRICOT_STREAM_TOKEN")
        .map_err(|_| ConfigError::NoToken)?
        .into_bytes();
    if token.is_empty() {
        return Err(ConfigError::EmptyToken);
    }
    if token.len() > MAX_TOKEN_BYTES {
        return Err(ConfigError::TokenTooLong(token.len()));
    }

    let display = env::var("APPRICOT_DISPLAY")
        .ok()
        .or_else(|| env::var("DISPLAY").ok());

    let log_filter = env::var("APPRICOT_LOG").unwrap_or_else(|_| "info".to_owned());

    // The test/demo knob: applied to the process (the session pump reads it there), not carried
    // in the struct — see the module docs.
    if let Ok(v) = env::var("APPRICOT_RESUME_GRACE_MS") {
        set_resume_grace_ms(parse_resume_grace(&v)?);
    }

    Ok(Config {
        bind,
        token,
        display,
        log_filter,
    })
}

#[cfg(test)]
mod tests {
    use super::{
        Bind, ConfigError, MAX_TOKEN_BYTES, parse_bind, parse_resume_grace, resume_grace_ms,
        set_resume_grace_ms,
    };

    fn unix(path: &str) -> Bind {
        Bind::Unix { path: path.into() }
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
