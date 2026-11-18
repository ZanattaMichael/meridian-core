//! Resolution of an untagged plain YAML scalar to a typed value.
//!
//! The rules are those of the YAML library the Go implementation used
//! (gopkg.in/yaml.v3), so a document means the same thing to this core as it
//! did to the one it replaces: `true`/`false` are the only booleans (`yes` and
//! `on` are strings), integers accept `0x`, `0o`, `0b` and a leading-zero
//! octal, and underscores are digit separators.
//!
//! Two rules are deliberately not carried over. That library turned a plain
//! scalar shaped like a date (`2001-12-14`) into a timestamp, which then
//! reached a target rewritten as `2001-12-14T00:00:00Z`; here it stays the
//! string the author wrote. And it read `08` and `09`, which are not octal,
//! as the floats 8 and 9; here a run of digits with a leading zero that is
//! not a valid octal number is text, since no number was meant.

/// What a scalar resolved to. `Str` carries no payload: the text is the
/// scalar's own raw value.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Scalar {
    Null,
    Bool(bool),
    Int(i64),
    Uint(u64),
    Float(f64),
    Str,
}

pub fn resolve_plain(s: &str) -> Scalar {
    match s {
        "" | "~" | "null" | "Null" | "NULL" => return Scalar::Null,
        "true" | "True" | "TRUE" => return Scalar::Bool(true),
        "false" | "False" | "FALSE" => return Scalar::Bool(false),
        ".nan" | ".NaN" | ".NAN" => return Scalar::Float(f64::NAN),
        ".inf" | ".Inf" | ".INF" | "+.inf" | "+.Inf" | "+.INF" => {
            return Scalar::Float(f64::INFINITY)
        }
        "-.inf" | "-.Inf" | "-.INF" => return Scalar::Float(f64::NEG_INFINITY),
        _ => {}
    }
    // Only a scalar that starts like a number can be one.
    if !matches!(s.as_bytes()[0], b'0'..=b'9' | b'-' | b'+' | b'.') {
        return Scalar::Str;
    }
    let plain: String = s.chars().filter(|c| *c != '_').collect();
    match parse_int(&plain) {
        Some(Int::Signed(i)) => return Scalar::Int(i),
        Some(Int::Unsigned(u)) => return Scalar::Uint(u),
        None => {}
    }
    if is_leading_zero_digits(&plain) {
        return Scalar::Str;
    }
    if is_yaml_float(&plain) {
        if let Ok(f) = plain.parse::<f64>() {
            return Scalar::Float(f);
        }
    }
    Scalar::Str
}

enum Int {
    Signed(i64),
    Unsigned(u64),
}

/// Parses an integer the way a base-0 parse does: an optional sign, then a
/// `0x`/`0o`/`0b` prefix, a bare leading zero meaning octal, or decimal.
fn parse_int(s: &str) -> Option<Int> {
    let (negative, body) = match s.as_bytes().first()? {
        b'-' => (true, &s[1..]),
        b'+' => (false, &s[1..]),
        _ => (false, s),
    };
    let (radix, digits) = match body.as_bytes() {
        [b'0', b'x' | b'X', ..] => (16, &body[2..]),
        [b'0', b'o' | b'O', ..] => (8, &body[2..]),
        [b'0', b'b' | b'B', ..] => (2, &body[2..]),
        [b'0', _, ..] => (8, &body[1..]),
        _ => (10, body),
    };
    if digits.is_empty() || !digits.bytes().all(|b| (b as char).is_digit(radix)) {
        return None;
    }
    let magnitude = u64::from_str_radix(digits, radix).ok()?;
    if negative {
        // i64::MIN has a magnitude one larger than i64::MAX.
        if magnitude > i64::MAX as u64 + 1 {
            return None;
        }
        Some(Int::Signed((magnitude as i128).wrapping_neg() as i64))
    } else if let Ok(i) = i64::try_from(magnitude) {
        Some(Int::Signed(i))
    } else {
        Some(Int::Unsigned(magnitude))
    }
}

/// `^[-+]?0[0-9]+$`: digits with a leading zero that did not parse as octal,
/// such as `08`, which the float rule would otherwise accept.
pub(crate) fn is_leading_zero_digits(s: &str) -> bool {
    let body = s.strip_prefix(['-', '+']).unwrap_or(s);
    body.len() > 1 && body.starts_with('0') && body.bytes().all(|b| b.is_ascii_digit())
}

/// `^[-+]?(\.[0-9]+|[0-9]+(\.[0-9]*)?)([eE][-+]?[0-9]+)?$`
fn is_yaml_float(s: &str) -> bool {
    let b = s.as_bytes();
    let mut i = 0;
    if matches!(b.first(), Some(b'-' | b'+')) {
        i += 1;
    }
    let digits = |i: &mut usize| {
        let start = *i;
        while *i < b.len() && b[*i].is_ascii_digit() {
            *i += 1;
        }
        *i - start
    };
    if b.get(i) == Some(&b'.') {
        i += 1;
        if digits(&mut i) == 0 {
            return false;
        }
    } else {
        if digits(&mut i) == 0 {
            return false;
        }
        if b.get(i) == Some(&b'.') {
            i += 1;
            digits(&mut i);
        }
    }
    if matches!(b.get(i), Some(b'e' | b'E')) {
        i += 1;
        if matches!(b.get(i), Some(b'-' | b'+')) {
            i += 1;
        }
        if digits(&mut i) == 0 {
            return false;
        }
    }
    i == b.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_the_way_the_reference_library_does() {
        use Scalar::*;
        for (input, want) in [
            ("", Null),
            ("~", Null),
            ("null", Null),
            ("true", Bool(true)),
            ("FALSE", Bool(false)),
            // YAML 1.1 booleans are strings.
            ("yes", Str),
            ("on", Str),
            ("No", Str),
            ("0", Int(0)),
            ("8080", Int(8080)),
            ("-12", Int(-12)),
            ("+7", Int(7)),
            ("1_000", Int(1000)),
            ("0x1F", Int(31)),
            ("0o17", Int(15)),
            ("0b101", Int(5)),
            // A bare leading zero is octal, as file modes are usually written.
            ("0644", Int(420)),
            // Not valid octal, and no number was meant: text, not the
            // float the reference library makes of it.
            ("08", Str),
            ("09", Str),
            ("-08", Str),
            ("0_9", Str),
            ("08.5", Float(8.5)),
            ("-9223372036854775808", Int(i64::MIN)),
            ("18446744073709551615", Uint(u64::MAX)),
            ("1.5", Float(1.5)),
            ("1.", Float(1.0)),
            (".5", Float(0.5)),
            ("1e3", Float(1000.0)),
            ("-.inf", Float(f64::NEG_INFINITY)),
            // Everything below merely starts like a number.
            ("1.2.3", Str),
            ("2001-12-14", Str),
            ("2001-12-14T21:59:43Z", Str),
            ("-", Str),
            (".", Str),
            ("0x", Str),
            ("1e", Str),
            ("ubuntu-22.04", Str),
            ("22.04-lts", Str),
        ] {
            assert_eq!(resolve_plain(input), want, "{input:?}");
        }
        assert!(matches!(resolve_plain(".nan"), Float(f) if f.is_nan()));
    }
}
