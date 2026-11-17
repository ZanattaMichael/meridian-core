//! Comparing two configurations by what they mean.
//!
//! A round-trip test imports a tool's configuration, compiles the document
//! that comes out, and asks whether the tool would read the result the same
//! way it read the original. Comparing text cannot answer that: key order,
//! quoting and number spelling all differ without meaning anything. This
//! module compares values, and [`Rules`] names the differences a particular
//! tool does not care about.

use std::collections::BTreeSet;

use meridian_sdk::Value;

/// The differences a tool does not care about. The default cares about all
/// of them except how a number is spelled.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Rules {
    /// A key whose value is null means the same as the key being absent.
    pub null_is_absent: bool,
    /// A key whose value is an empty map or an empty list means the same as
    /// the key being absent.
    pub empty_is_absent: bool,
    /// A list of exactly one map means the same as that map. This is how
    /// Terraform's JSON syntax reads a nested block.
    pub one_block_is_a_list: bool,
}

/// Every place two values differ in meaning, as lines a person can read:
/// the path, then what each side holds. Empty when they mean the same.
pub fn diff(expected: &Value, actual: &Value, rules: Rules) -> Vec<String> {
    let mut out = Vec::new();
    walk("", expected, actual, rules, &mut out);
    out
}

/// Panics with every difference when two values do not mean the same.
pub fn assert_same(expected: &Value, actual: &Value, rules: Rules) {
    let differences = diff(expected, actual, rules);
    assert!(
        differences.is_empty(),
        "the two configurations differ in {} place(s):\n{}",
        differences.len(),
        differences.join("\n")
    );
}

fn number(value: &Value) -> Option<f64> {
    match value {
        Value::Int(i) => Some(*i as f64),
        Value::Uint(u) => Some(*u as f64),
        Value::Float(f) => Some(*f),
        _ => None,
    }
}

fn absent(value: &Value, rules: Rules) -> bool {
    match value {
        Value::Null => rules.null_is_absent,
        Value::Map(m) => rules.empty_is_absent && m.is_empty(),
        Value::List(l) => rules.empty_is_absent && l.is_empty(),
        _ => false,
    }
}

fn shown(value: &Value) -> String {
    let text = value.canonical();
    if text.chars().count() > 120 {
        format!("{}...", text.chars().take(117).collect::<String>())
    } else {
        text
    }
}

fn at(path: &str) -> &str {
    if path.is_empty() {
        "(the whole value)"
    } else {
        path
    }
}

fn walk(path: &str, expected: &Value, actual: &Value, rules: Rules, out: &mut Vec<String>) {
    // Two ways of writing nothing are the same nothing.
    if absent(expected, rules) && absent(actual, rules) {
        return;
    }
    if let (Some(a), Some(b)) = (number(expected), number(actual)) {
        if a != b {
            out.push(format!(
                "{}: expected {}, found {}",
                at(path),
                shown(expected),
                shown(actual)
            ));
        }
        return;
    }
    match (expected, actual) {
        (Value::Map(a), Value::Map(b)) => {
            let keys: BTreeSet<&String> = a.keys().chain(b.keys()).collect();
            for key in keys {
                let child = if path.is_empty() {
                    key.clone()
                } else {
                    format!("{path}.{key}")
                };
                match (a.get(key), b.get(key)) {
                    (Some(x), Some(y)) => walk(&child, x, y, rules, out),
                    (Some(x), None) if !absent(x, rules) => {
                        out.push(format!("{child}: expected {}, found nothing", shown(x)));
                    }
                    (None, Some(y)) if !absent(y, rules) => {
                        out.push(format!("{child}: expected nothing, found {}", shown(y)));
                    }
                    _ => {}
                }
            }
        }
        (Value::List(a), Value::List(b)) => {
            if a.len() != b.len() {
                out.push(format!(
                    "{}: expected {} item(s), found {}",
                    at(path),
                    a.len(),
                    b.len()
                ));
                return;
            }
            for (i, (x, y)) in a.iter().zip(b).enumerate() {
                walk(&format!("{path}[{i}]"), x, y, rules, out);
            }
        }
        (Value::List(a), Value::Map(_))
            if rules.one_block_is_a_list && a.len() == 1 && matches!(a[0], Value::Map(_)) =>
        {
            walk(path, &a[0], actual, rules, out);
        }
        (Value::Map(_), Value::List(b))
            if rules.one_block_is_a_list && b.len() == 1 && matches!(b[0], Value::Map(_)) =>
        {
            walk(path, expected, &b[0], rules, out);
        }
        _ => {
            if expected != actual {
                out.push(format!(
                    "{}: expected {}, found {}",
                    at(path),
                    shown(expected),
                    shown(actual)
                ));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use meridian_sdk::decode_value;

    fn value(json: &str) -> Value {
        decode_value(json.as_bytes()).unwrap()
    }

    #[test]
    fn key_order_and_number_spelling_do_not_matter() {
        let a = value(r#"{"a": 1, "b": [1, 2.0], "c": {"x": "y"}}"#);
        let b = value(r#"{"c": {"x": "y"}, "b": [1.0, 2], "a": 1}"#);
        assert!(diff(&a, &b, Rules::default()).is_empty());
    }

    #[test]
    fn every_difference_is_reported_with_its_path() {
        let a = value(r#"{"a": 1, "b": [1, 2], "c": {"x": "y"}, "only": true}"#);
        let b = value(r#"{"a": 2, "b": [1, 3], "c": {"x": "z"}, "extra": null}"#);
        assert_eq!(
            diff(&a, &b, Rules::default()),
            [
                "a: expected 1, found 2",
                "b[1]: expected 2, found 3",
                "c.x: expected \"y\", found \"z\"",
                "extra: expected nothing, found null",
                "only: expected true, found nothing",
            ]
        );
    }

    #[test]
    fn the_rules_name_what_a_tool_does_not_care_about() {
        let a = value(r#"{"lifecycle": [{"x": 1}], "tags": {}, "n": null}"#);
        let b = value(r#"{"lifecycle": {"x": 1}}"#);
        assert_eq!(diff(&a, &b, Rules::default()).len(), 3);
        let lenient = Rules {
            null_is_absent: true,
            empty_is_absent: true,
            one_block_is_a_list: true,
        };
        assert!(diff(&a, &b, lenient).is_empty());
        assert!(diff(&b, &a, lenient).is_empty());
        // Both sides present, each empty in its own way.
        assert!(diff(&value(r#"{"x": null}"#), &value(r#"{"x": {}}"#), lenient).is_empty());
        assert_eq!(
            diff(
                &value(r#"{"x": null}"#),
                &value(r#"{"x": {}}"#),
                Rules::default()
            )
            .len(),
            1
        );
    }

    #[test]
    fn lists_of_different_lengths_are_one_difference() {
        let a = value("[1, 2, 3]");
        let b = value("[1, 2]");
        assert_eq!(
            diff(&a, &b, Rules::default()),
            ["(the whole value): expected 3 item(s), found 2"]
        );
    }
}
