//! JSON lines, written by hand.
//!
//! Every log this tool writes is one JSON object per line (RFC 8259). The objects are flat or
//! nearly so, and nothing here parses JSON back, so a small builder is all it takes and the
//! crate needs no serialisation dependency.

use std::fmt::Write as _;

/// One JSON object, built field by field and closed by [`JsonObject::finish`].
///
/// Keys are written in call order. Nothing checks for a repeated key: each call site writes
/// a fixed set.
#[derive(Debug, Clone)]
pub struct JsonObject {
    buf: String,
    empty: bool,
}

impl Default for JsonObject {
    fn default() -> Self {
        Self::new()
    }
}

impl JsonObject {
    /// An object with no field yet.
    pub fn new() -> Self {
        Self {
            buf: String::from("{"),
            empty: true,
        }
    }

    fn key(&mut self, key: &str) {
        if !self.empty {
            self.buf.push(',');
        }
        self.empty = false;
        push_string(&mut self.buf, key);
        self.buf.push(':');
    }

    /// Adds a string field.
    #[must_use]
    pub fn str(mut self, key: &str, value: &str) -> Self {
        self.key(key);
        push_string(&mut self.buf, value);
        self
    }

    /// Adds an unsigned integer field.
    #[must_use]
    pub fn uint(mut self, key: &str, value: u64) -> Self {
        self.key(key);
        let _ = write!(self.buf, "{value}");
        self
    }

    /// Adds a signed integer field.
    #[must_use]
    pub fn int(mut self, key: &str, value: i64) -> Self {
        self.key(key);
        let _ = write!(self.buf, "{value}");
        self
    }

    /// Adds a number field. JSON has no NaN or infinity, so those are written as `null`.
    #[must_use]
    pub fn float(mut self, key: &str, value: f64) -> Self {
        self.key(key);
        if value.is_finite() {
            let _ = write!(self.buf, "{value}");
        } else {
            self.buf.push_str("null");
        }
        self
    }

    /// Adds a boolean field.
    #[must_use]
    pub fn bool(mut self, key: &str, value: bool) -> Self {
        self.key(key);
        self.buf.push_str(if value { "true" } else { "false" });
        self
    }

    /// Adds `null`.
    #[must_use]
    pub fn null(mut self, key: &str) -> Self {
        self.key(key);
        self.buf.push_str("null");
        self
    }

    /// Adds an unsigned integer, or `null` for `None`.
    #[must_use]
    pub fn opt_uint(self, key: &str, value: Option<u64>) -> Self {
        match value {
            Some(v) => self.uint(key, v),
            None => self.null(key),
        }
    }

    /// Adds a string, or `null` for `None`.
    #[must_use]
    pub fn opt_str(self, key: &str, value: Option<&str>) -> Self {
        match value {
            Some(v) => self.str(key, v),
            None => self.null(key),
        }
    }

    /// Adds a value that is already JSON: a nested object from [`JsonObject::finish`] or an
    /// array from [`array()`].
    #[must_use]
    pub fn raw(mut self, key: &str, json: &str) -> Self {
        self.key(key);
        self.buf.push_str(json);
        self
    }

    /// Closes the object and returns its text, without a newline.
    pub fn finish(mut self) -> String {
        self.buf.push('}');
        self.buf
    }
}

/// A JSON array of values that are already JSON.
pub fn array<I, S>(items: I) -> String
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let mut out = String::from("[");
    for (i, item) in items.into_iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        out.push_str(item.as_ref());
    }
    out.push(']');
    out
}

/// A JSON array of strings.
pub fn string_array<I, S>(items: I) -> String
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    array(items.into_iter().map(|s| {
        let mut quoted = String::new();
        push_string(&mut quoted, s.as_ref());
        quoted
    }))
}

/// Appends `value` as a JSON string: quoted, with `"`, `\` and every control character escaped.
/// Everything else is valid in a JSON string as it is, UTF-8 included.
pub fn push_string(out: &mut String, value: &str) {
    out.push('"');
    for c in value.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{08}' => out.push_str("\\b"),
            '\u{0c}' => out.push_str("\\f"),
            c if u32::from(c) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", u32::from(c));
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

#[cfg(test)]
mod tests {
    use super::{JsonObject, array, string_array};

    #[test]
    fn an_object_writes_its_fields_in_order() {
        let line = JsonObject::new()
            .str("ev", "map")
            .uint("window", 42)
            .int("x", -3)
            .bool("or", true)
            .null("parent")
            .finish();
        assert_eq!(
            line,
            r#"{"ev":"map","window":42,"x":-3,"or":true,"parent":null}"#
        );
    }

    #[test]
    fn an_empty_object_is_braces() {
        assert_eq!(JsonObject::new().finish(), "{}");
    }

    #[test]
    fn strings_escape_quotes_backslashes_and_control_characters() {
        let line = JsonObject::new().str("t", "a\"b\\c\nd\u{01}é λ").finish();
        assert_eq!(line, r#"{"t":"a\"b\\c\nd\u0001é λ"}"#);
    }

    #[test]
    fn non_finite_numbers_are_null() {
        let line = JsonObject::new()
            .float("a", 1.5)
            .float("b", f64::NAN)
            .float("c", f64::INFINITY)
            .finish();
        assert_eq!(line, r#"{"a":1.5,"b":null,"c":null}"#);
    }

    #[test]
    fn arrays_nest() {
        let inner = JsonObject::new().uint("n", 1).finish();
        let line = JsonObject::new()
            .raw("objects", &array([inner.as_str(), "2"]))
            .raw("names", &string_array(["a", "b\"c"]))
            .raw("none", &array(Vec::<String>::new()))
            .finish();
        assert_eq!(
            line,
            r#"{"objects":[{"n":1},2],"names":["a","b\"c"],"none":[]}"#
        );
    }
}
