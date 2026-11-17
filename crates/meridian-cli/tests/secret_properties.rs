//! Property tests for secrets in expressions: marking a value as a secret
//! changes who may see it and nothing else.
//!
//! The model is the expression engine itself, without secrets. Every
//! generated expression is worked out twice, from the same text: once where
//! `secret.a` and `secret.b` are secrets, and once where they are ordinary
//! text holding the same characters. What the two give must agree: the same
//! yes or no from a comparison, the same characters behind a secret. And
//! where the first gives something anyone can read, it holds no secret.

use std::collections::BTreeMap;
use std::sync::Arc;

use meridian_expr::{Expr, Scope, Template};
use meridian_secret::{SecretTable, SecretValue};
use meridian_value::Value;
use proptest::prelude::*;
use proptest::test_runner::FileFailurePersistence;

/// Long and unlike anything the generators write, so that finding one in a
/// result is finding a leak. Each has characters `urlEncode` changes and
/// space at both ends for `trim` to take off.
const A: &str = " s3cr3t/A&kq7Zp2=not real 9fXw ";
const B: &str = " tok?B+Zx81 not-a-real#token ";

fn cases(count: u32) -> ProptestConfig {
    ProptestConfig {
        cases: count,
        failure_persistence: Some(Box::new(FileFailurePersistence::Direct(
            "tests/secret_properties.proptest-regressions",
        ))),
        ..ProptestConfig::default()
    }
}

/// The two readings of one document: with secrets, and with the same
/// characters as plain text.
struct Readings {
    table: Arc<SecretTable>,
    secret: Scope,
    plain: Scope,
}

fn readings() -> Readings {
    let held = |text: &str| SecretValue::from_reader(text.as_bytes(), 4096).unwrap();
    let table = Arc::new(SecretTable::new());
    let data: BTreeMap<String, Value> = [
        ("host".to_owned(), Value::from("db.internal")),
        ("user".to_owned(), Value::from("app user")),
        ("empty".to_owned(), Value::from("")),
    ]
    .into();
    let secrets: BTreeMap<String, Value> = [
        ("a".to_owned(), Value::Secret(table.insert(held(A)))),
        ("b".to_owned(), Value::Secret(table.insert(held(B)))),
        // An optional secret that was not found.
        ("none".to_owned(), Value::Null),
    ]
    .into();
    let texts: BTreeMap<String, Value> = [
        ("a".to_owned(), Value::from(A)),
        ("b".to_owned(), Value::from(B)),
        ("none".to_owned(), Value::Null),
    ]
    .into();
    Readings {
        secret: Scope::new()
            .with("secret", Value::Map(secrets))
            .with("data", Value::Map(data.clone()))
            .with_secrets(Arc::clone(&table)),
        plain: Scope::new()
            .with("secret", Value::Map(texts))
            .with("data", Value::Map(data)),
        table,
    }
}

/// No text anyone can read may hold a secret, whole.
fn clean(text: &str) -> bool {
    !text.contains(A) && !text.contains(B) && !text.contains(A.trim()) && !text.contains(B.trim())
}

/// Something that reads a secret whichever way it is worked out.
fn a_secret() -> impl Strategy<Value = String> {
    prop::sample::select(vec!["secret.a", "secret.b"]).prop_map(str::to_owned)
}

/// Text nobody needs to protect.
fn plain_text() -> impl Strategy<Value = String> {
    prop::sample::select(vec![
        "'x'",
        "''",
        "' '",
        "'a b'",
        "'p@ss/w0rd'",
        "data.host",
        "data.user",
        "data.empty",
    ])
    .prop_map(str::to_owned)
}

/// A yes or no, some of them from comparing a secret.
fn truth(text: BoxedStrategy<String>) -> impl Strategy<Value = String> {
    prop_oneof![
        Just("true".to_owned()),
        Just("false".to_owned()),
        Just("secret.none == null".to_owned()),
        Just("secret.a != null".to_owned()),
        Just("defined(secret.b)".to_owned()),
        (text.clone(), text.clone()).prop_map(|(a, b)| format!("({a} == {b})")),
        (text.clone(), text).prop_map(|(a, b)| format!("({a} != {b})")),
    ]
}

/// An expression whose answer is text: built only from what a secret may be
/// given to, so that it can be worked out whether or not it holds one.
fn text() -> BoxedStrategy<String> {
    let leaf = prop_oneof![a_secret(), plain_text()];
    leaf.prop_recursive(4, 24, 3, |inner| {
        prop_oneof![
            (inner.clone(), inner.clone()).prop_map(|(a, b)| format!("({a} + {b})")),
            inner.clone().prop_map(|a| format!("urlEncode({a})")),
            inner.clone().prop_map(|a| format!("base64Encode({a})")),
            inner.clone().prop_map(|a| format!("trim({a})")),
            inner
                .clone()
                .prop_map(|a| format!("base64Decode(base64Encode({a}))")),
            (truth(inner.clone().boxed()), inner.clone(), inner.clone())
                .prop_map(|(c, a, b)| format!("({c} ? {a} : {b})")),
            inner.clone().prop_map(|a| format!("(secret.none ?? {a})")),
            (inner.clone(), inner.clone()).prop_map(|(a, b)| format!("coalesce({a}, {b})")),
            inner
                .clone()
                .prop_map(|a| format!("default(secret.none, {a})")),
            inner.prop_map(|a| format!("required({a}, 'needed')")),
        ]
    })
    .boxed()
}

/// A function that would have to copy a secret out to work on it.
fn refusing() -> impl Strategy<Value = &'static str> {
    prop::sample::select(vec![
        "upper", "lower", "title", "length", "sha256", "toString", "toJson", "quote", "basename",
        "dirname", "reverse", "type",
    ])
}

/// Whether what the secret reading gave is what the plain reading gave: the
/// same value where it is plain, the same characters where it is a secret.
fn agree(readings: &Readings, with: &Value, without: &Value) -> Result<(), TestCaseError> {
    match (with, without) {
        (Value::Secret(at), Value::String(want)) => {
            let same = readings.table.same_as_text(*at, want);
            prop_assert_eq!(same.ok(), Some(true), "the secret is not {:?}", want);
        }
        (with, without) => {
            prop_assert_eq!(with, without);
            prop_assert!(clean(&format!("{with:?}")), "readable: {:?}", with);
        }
    }
    Ok(())
}

proptest! {
    #![proptest_config(cases(1500))]

    /// The heart of it: an expression means the same whether or not what it
    /// reads is a secret, and nothing readable comes out holding one.
    #[test]
    fn a_secret_changes_who_may_read_a_value_and_not_the_value(source in text()) {
        let readings = readings();
        let expr = Expr::parse(&source);
        prop_assert!(expr.is_ok(), "{}: {:?}", source, expr);
        let expr = expr.unwrap();
        let with = expr.eval(&readings.secret);
        let without = expr.eval(&readings.plain);
        match (&with, &without) {
            (Ok(with), Ok(without)) => agree(&readings, with, without)?,
            (Err(with), Err(_)) => prop_assert!(clean(with), "{}", with),
            _ => prop_assert!(false, "{}: {:?} with secrets, {:?} without", source, with, without),
        }
        // What can be printed of the answer says nothing.
        if let Ok(with) = &with {
            prop_assert!(clean(&format!("{with:?}")), "{:?}", with);
        }
    }

    /// A comparison is the one way out, and it lets out a yes or a no: the
    /// same one the characters themselves would give.
    #[test]
    fn comparing_secrets_gives_the_answer_the_characters_give(source in truth(text())) {
        let readings = readings();
        let expr = Expr::parse(&source).unwrap();
        let with = expr.eval(&readings.secret);
        let without = expr.eval(&readings.plain);
        prop_assert!(matches!(with, Ok(Value::Bool(_))), "{}: {:?}", source, with);
        prop_assert_eq!(with, without, "{}", source);
    }

    /// Text with secrets placed in it is one secret, holding exactly what
    /// the same text would hold with the characters written out.
    #[test]
    fn text_with_a_secret_placed_in_it_is_a_secret_holding_the_whole_text(
        pieces in prop::collection::vec((prop::sample::select(vec!["", "x", " = ", "user:", "/"]), text()), 1..4),
        tail in prop::sample::select(vec!["", "@db", " "]),
    ) {
        let readings = readings();
        let mut source = String::new();
        for (before, expr) in &pieces {
            source.push_str(before);
            source.push_str(&format!("{}{{ {expr} }}}}", "${"));
        }
        source.push_str(tail);
        let template = Template::parse(&source).unwrap();
        let with = template.eval(&readings.secret);
        let without = template.eval(&readings.plain);
        match (&with, &without) {
            (Ok(with), Ok(without)) => agree(&readings, with, without)?,
            (Err(with), Err(_)) => prop_assert!(clean(with), "{}", with),
            _ => prop_assert!(false, "{}: {:?} with secrets, {:?} without", source, with, without),
        }
    }

    /// A function that is not on the short list refuses a secret, says which
    /// functions take one, and repeats nothing of the value. Given the same
    /// expression without a secret in it, it does its work.
    #[test]
    fn a_function_off_the_list_refuses_a_secret_and_only_a_secret(
        function in refusing(),
        source in text(),
    ) {
        let readings = readings();
        let inner = Expr::parse(&source).unwrap();
        let outer = Expr::parse(&format!("{function}({source})")).unwrap();
        let is_secret = matches!(inner.eval(&readings.secret), Ok(Value::Secret(_)));
        let with = outer.eval(&readings.secret);
        if is_secret {
            let refused = with.err().unwrap_or_default();
            prop_assert!(
                refused.contains(&format!("{function}(...) cannot be given a secret")),
                "{}({}) gave {:?}", function, source, refused
            );
            prop_assert!(refused.contains("urlEncode"), "{}", refused);
            prop_assert!(clean(&refused), "{}", refused);
        } else {
            prop_assert_eq!(with, outer.eval(&readings.plain), "{}({})", function, source);
        }
    }

    /// Once the secrets are wiped, what was made from them is gone as well:
    /// nothing derived keeps a copy that outlives them.
    #[test]
    fn nothing_made_from_a_secret_outlives_the_wipe(source in text()) {
        let readings = readings();
        let made = Expr::parse(&source).unwrap().eval(&readings.secret);
        readings.table.wipe_all();
        prop_assert_eq!(readings.table.held(), 0);
        if let Ok(Value::Secret(at)) = made {
            prop_assert!(!readings.table.is_held(at));
            prop_assert!(readings.table.same_as_text(at, "").is_err());
        }
    }
}
