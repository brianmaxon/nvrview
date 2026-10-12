//! A tiny JSON object writer for control events.
//!
//! Events are a handful of flat objects with strings, integers, booleans, and
//! one array of small objects, written a few times a second at most. A
//! hand-written writer covers that in a few lines; a serialization framework
//! would add several crates to every platform build for no gain. Strings that
//! come from devices (names, models) are escaped per RFC 8259, and the tests
//! parse every event shape with a real JSON parser.

use std::fmt::Write;

/// Builds one JSON object.
pub(crate) struct Object {
    buf: String,
    empty: bool,
}

impl Object {
    /// An empty object.
    pub(crate) fn new() -> Object {
        Object {
            buf: String::from("{"),
            empty: true,
        }
    }

    /// An object whose first member is `"type": kind`.
    pub(crate) fn event(kind: &str) -> Object {
        Object::new().str("type", kind)
    }

    fn key(&mut self, key: &str) {
        if !self.empty {
            self.buf.push(',');
        }
        self.empty = false;
        escape_into(&mut self.buf, key);
        self.buf.push(':');
    }

    /// Adds a string member.
    pub(crate) fn str(mut self, key: &str, value: &str) -> Object {
        self.key(key);
        escape_into(&mut self.buf, value);
        self
    }

    /// Adds an unsigned integer member.
    pub(crate) fn u64(mut self, key: &str, value: u64) -> Object {
        self.key(key);
        let _ = write!(self.buf, "{value}");
        self
    }

    /// Adds a number member; a value that is not finite is written as null.
    pub(crate) fn f64(mut self, key: &str, value: f64) -> Object {
        self.key(key);
        if value.is_finite() {
            // Three decimals are plenty for diagnostics. Display never uses
            // exponent notation, so the text is a valid JSON number.
            let _ = write!(self.buf, "{}", (value * 1e3).round() / 1e3);
        } else {
            self.buf.push_str("null");
        }
        self
    }

    /// Adds a boolean member.
    pub(crate) fn bool(mut self, key: &str, value: bool) -> Object {
        self.key(key);
        self.buf.push_str(if value { "true" } else { "false" });
        self
    }

    /// Adds a nested object member.
    pub(crate) fn object(mut self, key: &str, value: Object) -> Object {
        self.key(key);
        self.buf.push_str(&value.finish());
        self
    }

    /// Adds an array of booleans.
    pub(crate) fn bools(mut self, key: &str, values: impl IntoIterator<Item = bool>) -> Object {
        self.key(key);
        self.buf.push('[');
        for (i, v) in values.into_iter().enumerate() {
            if i > 0 {
                self.buf.push(',');
            }
            self.buf.push_str(if v { "true" } else { "false" });
        }
        self.buf.push(']');
        self
    }

    /// Adds an array of strings.
    pub(crate) fn strs<'a>(
        mut self,
        key: &str,
        values: impl IntoIterator<Item = &'a str>,
    ) -> Object {
        self.key(key);
        self.buf.push('[');
        for (i, v) in values.into_iter().enumerate() {
            if i > 0 {
                self.buf.push(',');
            }
            escape_into(&mut self.buf, v);
        }
        self.buf.push(']');
        self
    }

    /// Adds an array of arrays of strings.
    pub(crate) fn str_arrays<'a, I>(
        mut self,
        key: &str,
        values: impl IntoIterator<Item = I>,
    ) -> Object
    where
        I: IntoIterator<Item = &'a str>,
    {
        self.key(key);
        self.buf.push('[');
        for (i, inner) in values.into_iter().enumerate() {
            if i > 0 {
                self.buf.push(',');
            }
            self.buf.push('[');
            for (k, v) in inner.into_iter().enumerate() {
                if k > 0 {
                    self.buf.push(',');
                }
                escape_into(&mut self.buf, v);
            }
            self.buf.push(']');
        }
        self.buf.push(']');
        self
    }

    /// Adds an array of objects.
    pub(crate) fn objects(mut self, key: &str, values: impl IntoIterator<Item = Object>) -> Object {
        self.key(key);
        self.buf.push('[');
        for (i, v) in values.into_iter().enumerate() {
            if i > 0 {
                self.buf.push(',');
            }
            self.buf.push_str(&v.finish());
        }
        self.buf.push(']');
        self
    }

    /// The finished JSON text.
    pub(crate) fn finish(mut self) -> String {
        self.buf.push('}');
        self.buf
    }
}

/// Appends `s` as a JSON string literal.
fn escape_into(out: &mut String, s: &str) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_valid_json() {
        let j = Object::event("channels")
            .u64("device", 1)
            .bool("ok", true)
            .f64("x", 1.23456)
            .f64("nan", f64::NAN)
            .strs("tiers", ["main", "sub"])
            .objects(
                "channels",
                [
                    Object::new().u64("channel", 0).str("name", "Channel 1"),
                    Object::new(),
                ],
            )
            .finish();
        assert_eq!(
            j,
            r#"{"type":"channels","device":1,"ok":true,"x":1.235,"nan":null,"tiers":["main","sub"],"channels":[{"channel":0,"name":"Channel 1"},{}]}"#
        );
        let v: serde_json::Value = serde_json::from_str(&j).unwrap();
        assert_eq!(v["channels"][0]["name"], "Channel 1");
    }

    #[test]
    fn escapes_device_strings() {
        let nasty = "a\"b\\c\nd\u{1}e\u{7f}\u{e9}\u{1F600}</x>";
        let j = Object::event("x").str("name", nasty).finish();
        let v: serde_json::Value = serde_json::from_str(&j).unwrap();
        assert_eq!(v["name"], nasty);
        assert!(!j.contains('\n'));
    }
}
