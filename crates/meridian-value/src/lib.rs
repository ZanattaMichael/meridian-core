//! The free-form value Meridian carries wherever a document is not typed: a
//! resource's `params`, and resolved hierarchy data.
//!
//! It lives in its own crate because both sides of the plugin boundary need
//! it. The core builds values from documents; a plugin receives them over the
//! wire. Neither should have to depend on the other to name the type.

pub mod host;
pub mod placed;

use std::collections::BTreeMap;

pub use placed::PlacedEscape;

/// A free-form value as it appears under a resource's `params`.
///
/// Maps are ordered by key, so nothing downstream can come to depend on the
/// order a document happened to declare its keys in.
#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    Null,
    Bool(bool),
    Int(i64),
    /// An integer literal too large for `Int`.
    Uint(u64),
    Float(f64),
    String(String),
    List(Vec<Value>),
    Map(BTreeMap<String, Value>),
    /// A secret. The value holds no bytes of it, only which secret it is; the
    /// bytes stay in the run's secret table. It prints and encodes as
    /// [`SENSITIVE`], so nothing made from a `Value` can show a secret.
    Secret(SecretRef),
}

/// What stands in for a secret wherever a value is shown or written.
pub const SENSITIVE: &str = "(sensitive)";

/// Names one secret of a run: a slot in the run's secret table. It is not the
/// secret and nothing about the secret can be learned from it.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SecretRef(u64);

impl SecretRef {
    pub fn new(slot: u64) -> Self {
        SecretRef(slot)
    }

    pub fn slot(self) -> u64 {
        self.0
    }
}

impl std::fmt::Debug for SecretRef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(SENSITIVE)
    }
}

impl Value {
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Value::String(s) => Some(s),
            _ => None,
        }
    }

    pub fn as_map(&self) -> Option<&BTreeMap<String, Value>> {
        match self {
            Value::Map(m) => Some(m),
            _ => None,
        }
    }

    pub fn as_list(&self) -> Option<&[Value]> {
        match self {
            Value::List(l) => Some(l),
            _ => None,
        }
    }

    /// The word a diagnostic uses for this value's shape.
    pub fn describe(&self) -> &'static str {
        match self {
            Value::Null => "null",
            Value::Bool(_) => "a boolean",
            Value::Int(_) | Value::Uint(_) | Value::Float(_) => "a number",
            Value::String(_) => "a string",
            Value::List(_) => "a list",
            Value::Map(_) => "a mapping",
            Value::Secret(_) => "a secret",
        }
    }

    /// Renders the value as compact JSON with map keys in sorted order.
    ///
    /// Two values that compare equal always render identically, which makes
    /// this the determinism oracle for the whole toolchain, and it is also the
    /// encoding values cross the plugin boundary in. The bytes are exactly
    /// what Go's `encoding/json` produces for the same value, because plugins
    /// built against the Go SDK were written and tested against that encoding:
    /// `<`, `>` and `&` are escaped, and a float is written the shortest way
    /// that round-trips, without an exponent between 1e-6 and 1e21.
    ///
    /// JSON has no NaN or infinity. They render as the quoted strings `"NaN"`,
    /// `"+Inf"` and `"-Inf"`.
    pub fn canonical(&self) -> String {
        let mut out = String::new();
        self.write_canonical(&mut out);
        out
    }

    /// Renders the value as indented JSON for a person to read: two spaces a
    /// level, map keys in sorted order, a trailing newline. It is what a
    /// plugin writes when its tool's own file format is JSON.
    ///
    /// Unlike [`Value::canonical`] it escapes only what JSON requires, so an
    /// expression such as `${a.b}` or a `<` stays readable.
    pub fn pretty(&self) -> String {
        let mut out = String::new();
        self.write_pretty(&mut out, 0);
        out.push('\n');
        out
    }

    fn write_pretty(&self, out: &mut String, depth: usize) {
        let indent = |out: &mut String, depth: usize| {
            out.push('\n');
            for _ in 0..depth {
                out.push_str("  ");
            }
        };
        match self {
            Value::String(s) => write_plain_string(s, out),
            Value::List(items) if !items.is_empty() => {
                out.push('[');
                for (i, item) in items.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    indent(out, depth + 1);
                    item.write_pretty(out, depth + 1);
                }
                indent(out, depth);
                out.push(']');
            }
            Value::Map(map) if !map.is_empty() => {
                out.push('{');
                for (i, (key, value)) in map.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    indent(out, depth + 1);
                    write_plain_string(key, out);
                    out.push_str(": ");
                    value.write_pretty(out, depth + 1);
                }
                indent(out, depth);
                out.push('}');
            }
            other => other.write_canonical(out),
        }
    }

    fn write_canonical(&self, out: &mut String) {
        match self {
            Value::Null => out.push_str("null"),
            Value::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
            Value::Int(i) => out.push_str(&i.to_string()),
            Value::Uint(u) => out.push_str(&u.to_string()),
            Value::Float(f) => write_float(*f, out),
            Value::String(s) => write_string(s, out),
            Value::List(items) => {
                out.push('[');
                for (i, item) in items.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    item.write_canonical(out);
                }
                out.push(']');
            }
            Value::Map(map) => write_map(map, out),
            // Across the wire and in anything written, a secret is its mask.
            Value::Secret(_) => write_string(SENSITIVE, out),
        }
    }
}

/// Reads one JSON value, keeping integers as integers. Empty input is null.
pub fn decode_value(json: &[u8]) -> Result<Value, String> {
    if json.is_empty() {
        return Ok(Value::Null);
    }
    serde_json::from_slice::<serde_json::Value>(json)
        .map(Value::from)
        .map_err(|e| format!("decoding a value: {e}"))
}

/// Decodes the JSON object a params or data map crosses the wire as. Empty
/// input is an empty map, so the receiving side never has to tell "absent"
/// from "empty".
///
/// An integer literal decodes as an integer, not a float: a port of 8080 must
/// not come back as 8080.0 and change the emitted artifact.
pub fn decode_map(json: &[u8]) -> Result<BTreeMap<String, Value>, String> {
    if json.is_empty() {
        return Ok(BTreeMap::new());
    }
    let value: serde_json::Value =
        serde_json::from_slice(json).map_err(|e| format!("decoding values: {e}"))?;
    match Value::from(value) {
        Value::Map(map) => Ok(map),
        other => Err(format!(
            "decoding values: expected an object, got {}",
            other.describe()
        )),
    }
}

impl From<serde_json::Value> for Value {
    fn from(value: serde_json::Value) -> Self {
        match value {
            serde_json::Value::Null => Value::Null,
            serde_json::Value::Bool(b) => Value::Bool(b),
            serde_json::Value::Number(n) => {
                if let Some(i) = n.as_i64() {
                    Value::Int(i)
                } else if let Some(u) = n.as_u64() {
                    Value::Uint(u)
                } else {
                    Value::Float(n.as_f64().unwrap_or(f64::NAN))
                }
            }
            serde_json::Value::String(s) => Value::String(s),
            serde_json::Value::Array(items) => {
                Value::List(items.into_iter().map(Value::from).collect())
            }
            serde_json::Value::Object(map) => {
                Value::Map(map.into_iter().map(|(k, v)| (k, Value::from(v))).collect())
            }
        }
    }
}

/// A JSON string with only the escapes JSON requires.
fn write_plain_string(s: &str, out: &mut String) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
}

/// [`Value::canonical`] for a top-level map, such as a resource's params.
pub fn canonical_map(map: &BTreeMap<String, Value>) -> String {
    let mut out = String::new();
    write_map(map, &mut out);
    out
}

fn write_map(map: &BTreeMap<String, Value>, out: &mut String) {
    out.push('{');
    for (i, (key, value)) in map.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        write_string(key, out);
        out.push(':');
        value.write_canonical(out);
    }
    out.push('}');
}

fn write_float(f: f64, out: &mut String) {
    if f.is_nan() {
        return out.push_str("\"NaN\"");
    }
    if f.is_infinite() {
        return out.push_str(if f > 0.0 { "\"+Inf\"" } else { "\"-Inf\"" });
    }
    let abs = f.abs();
    if abs != 0.0 && !(1e-6..1e21).contains(&abs) {
        // Rust writes 1e21 and 1e-7; Go writes 1e+21 and 1e-7.
        let text = format!("{f:e}");
        match text.split_once('e') {
            Some((mantissa, exponent)) if !exponent.starts_with('-') => {
                out.push_str(mantissa);
                out.push_str("e+");
                out.push_str(exponent);
            }
            _ => out.push_str(&text),
        }
    } else {
        out.push_str(&f.to_string());
    }
}

fn write_string(s: &str, out: &mut String) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{8}' => out.push_str("\\b"),
            '\u{c}' => out.push_str("\\f"),
            '<' | '>' | '&' | '\u{2028}' | '\u{2029}' => {
                out.push_str(&format!("\\u{:04x}", c as u32));
            }
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
}

impl From<&str> for Value {
    fn from(s: &str) -> Self {
        Value::String(s.to_owned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every expectation here was produced by running the value through Go's
    /// `json.Marshal`. Plugins receive values in this encoding, so the two
    /// have to agree to the byte.
    #[test]
    fn floats_render_as_go_does() {
        for (f, want) in [
            (0.0, "0"),
            (-0.0, "-0"),
            (1.0, "1"),
            (-1.0, "-1"),
            (80.0, "80"),
            (8080.0, "8080"),
            (0.5, "0.5"),
            (1.5, "1.5"),
            (6.02214, "6.02214"),
            (0.1, "0.1"),
            (1.0 / 3.0, "0.3333333333333333"),
            (1234.5678, "1234.5678"),
            (100000.0, "100000"),
            (1e6, "1000000"),
            (1e-5, "0.00001"),
            (1e-6, "0.000001"),
            (9.99e-7, "9.99e-7"),
            (1e-7, "1e-7"),
            (-2.5e-8, "-2.5e-8"),
            (1.5e-10, "1.5e-10"),
            (5e-324, "5e-324"),
            (1e20, "100000000000000000000"),
            (123456789012345680000.0, "123456789012345680000"),
            (1e21, "1e+21"),
            (1.5e21, "1.5e+21"),
            (1e22, "1e+22"),
            (1e100, "1e+100"),
            (f64::MAX, "1.7976931348623157e+308"),
        ] {
            assert_eq!(Value::Float(f).canonical(), want, "{f:e}");
        }
    }

    #[test]
    fn strings_render_as_go_does() {
        for (s, want) in [
            ("plain", r#""plain""#),
            ("q\"uote", r#""q\"uote""#),
            ("back\\slash", r#""back\\slash""#),
            ("nl\n", r#""nl\n""#),
            ("cr\r", r#""cr\r""#),
            ("tab\t", r#""tab\t""#),
            ("\u{8}\u{c}", r#""\b\f""#),
            ("\u{0}\u{1}\u{1f}", r#""\u0000\u0001\u001f""#),
            ("\u{7f}", "\"\u{7f}\""),
            ("<tag> & more", r#""\u003ctag\u003e \u0026 more""#),
            ("\u{2028}\u{2029}", r#""\u2028\u2029""#),
            ("héllo ✓ 😀", r#""héllo ✓ 😀""#),
            ("a/b", r#""a/b""#),
        ] {
            assert_eq!(Value::from(s).canonical(), want);
        }
    }

    #[test]
    fn maps_render_sorted_and_non_finite_floats_are_quoted() {
        let map: BTreeMap<String, Value> = [
            (
                "b".to_owned(),
                Value::List(vec![Value::Int(3), Value::Null, Value::Bool(true)]),
            ),
            ("a".to_owned(), Value::Uint(u64::MAX)),
            ("c".to_owned(), Value::Float(f64::NAN)),
            ("d".to_owned(), Value::Float(f64::NEG_INFINITY)),
        ]
        .into_iter()
        .collect();
        assert_eq!(
            canonical_map(&map),
            r#"{"a":18446744073709551615,"b":[3,null,true],"c":"NaN","d":"-Inf"}"#
        );
        assert_eq!(Value::Map(map.clone()).canonical(), canonical_map(&map));
    }

    #[test]
    fn a_map_survives_the_wire_with_its_number_types() {
        let json = br#"{"port":8080,"ratio":0.5,"big":18446744073709551615,"neg":-3,"list":[1,"two",null,true],"nested":{"b":2,"a":1}}"#;
        let map = decode_map(json).unwrap();
        assert_eq!(map["port"], Value::Int(8080));
        assert_eq!(map["ratio"], Value::Float(0.5));
        assert_eq!(map["big"], Value::Uint(u64::MAX));
        assert_eq!(map["neg"], Value::Int(-3));
        assert_eq!(
            canonical_map(&map),
            r#"{"big":18446744073709551615,"list":[1,"two",null,true],"neg":-3,"nested":{"a":1,"b":2},"port":8080,"ratio":0.5}"#
        );
        // A whole-number float is written without a fraction, so it is read back
        // as an integer. The emitted artifact is the same either way.
        let whole: BTreeMap<String, Value> =
            [("n".to_owned(), Value::Float(80.0))].into_iter().collect();
        assert_eq!(
            decode_map(canonical_map(&whole).as_bytes()).unwrap()["n"],
            Value::Int(80)
        );

        assert!(decode_map(b"").unwrap().is_empty());
        assert_eq!(
            decode_map(b"[1]").unwrap_err(),
            "decoding values: expected an object, got a list"
        );
        assert!(decode_map(b"{")
            .unwrap_err()
            .starts_with("decoding values: "));
    }
}
