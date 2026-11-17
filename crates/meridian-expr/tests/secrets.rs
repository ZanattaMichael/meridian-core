//! A secret in an expression: the expression holds which secret, never its
//! bytes, and what it makes of one is a secret too, except a comparison,
//! which is a plain yes or no.

use std::collections::BTreeMap;
use std::sync::Arc;

use meridian_expr::{Expr, Scope, Template};
use meridian_secret::{SecretTable, SecretValue};
use meridian_value::{SecretRef, Value};

const PASSWORD: &str = "p@ss/w0rd-9fXw";

struct Fixture {
    table: Arc<SecretTable>,
    scope: Scope,
}

fn secret(text: &str) -> SecretValue {
    SecretValue::from_reader(text.as_bytes(), 4096).unwrap()
}

fn fixture() -> Fixture {
    let table = Arc::new(SecretTable::new());
    let mut secrets = BTreeMap::new();
    secrets.insert(
        "db_password".to_owned(),
        Value::Secret(table.insert(secret(PASSWORD))),
    );
    secrets.insert(
        "user".to_owned(),
        Value::Secret(table.insert(secret("deploy"))),
    );
    secrets.insert(
        "pending".to_owned(),
        Value::Secret(table.declare("pending")),
    );
    let mut data = BTreeMap::new();
    data.insert("host".to_owned(), Value::from("db.internal"));
    data.insert("port".to_owned(), Value::Int(5432));
    let scope = Scope::new()
        .with("secret", Value::Map(secrets))
        .with("data", Value::Map(data))
        .with_secrets(Arc::clone(&table));
    Fixture { table, scope }
}

impl Fixture {
    fn eval(&self, text: &str) -> Result<Value, String> {
        Expr::parse(text).unwrap().eval(&self.scope)
    }

    fn place(&self, text: &str) -> Result<Value, String> {
        Template::parse(text).unwrap().eval_literal(&self.scope)
    }

    fn secret(&self, value: Value) -> SecretRef {
        match value {
            Value::Secret(at) => at,
            other => panic!("not a secret: {other:?}"),
        }
    }

    fn is(&self, value: Value, want: &str) -> bool {
        let at = self.secret(value);
        self.table
            .with(at, |held| held.expose(|bytes| bytes == want.as_bytes()))
            .unwrap()
    }
}

/// No text made from a value or an error may hold the secret.
fn clean(text: &str) {
    assert!(!text.contains(PASSWORD), "the secret is in: {text}");
    assert!(!text.contains("p%40ss"), "its encoded form is in: {text}");
}

#[test]
fn reading_a_secret_gives_a_reference_that_shows_nothing() {
    let f = fixture();
    let value = f.eval("secret.db_password").unwrap();
    assert_eq!(format!("{value:?}"), "Secret((sensitive))");
    assert_eq!(value.canonical(), "\"(sensitive)\"");
    assert_eq!(value.pretty().trim_end(), "\"(sensitive)\"");
    assert_eq!(value.describe(), "a secret");
    assert!(f.is(value, PASSWORD));
}

#[test]
fn a_secret_joined_to_text_is_a_secret() {
    let f = fixture();
    let joined = f.eval("\"password=\" + secret.db_password").unwrap();
    assert!(f.is(joined, &format!("password={PASSWORD}")));
    let joined = f.eval("secret.user + \":\" + secret.db_password").unwrap();
    assert!(f.is(joined, &format!("deploy:{PASSWORD}")));
    let joined = f.eval("secret.user + secret.user").unwrap();
    assert!(f.is(joined, "deploydeploy"));
}

#[test]
fn a_secret_placed_inside_text_makes_the_whole_text_a_secret() {
    let f = fixture();
    let url = f
        .place(concat!(
            "postgres://$",
            "{{ secret.user }}:$",
            "{{ urlEncode(secret.db_password) }}@$",
            "{{ data.host }}:$",
            "{{ data.port }}/app"
        ))
        .unwrap();
    clean(&format!("{url:?} {}", url.canonical()));
    assert!(f.is(
        url,
        "postgres://deploy:p%40ss%2Fw0rd-9fXw@db.internal:5432/app"
    ));

    // Alone, it keeps being the secret it was.
    let whole = f.place(concat!("$", "{{ secret.db_password }}")).unwrap();
    assert!(f.is(whole, PASSWORD));
    // Text with no secret in it is still plain text.
    let plain = f.place(concat!("host=$", "{{ data.host }}")).unwrap();
    assert_eq!(plain, Value::from("host=db.internal"));
}

#[test]
fn the_functions_that_take_a_secret_give_one() {
    let f = fixture();
    assert!(f.is(
        f.eval("urlEncode(secret.db_password)").unwrap(),
        "p%40ss%2Fw0rd-9fXw"
    ));
    assert!(f.is(f.eval("base64Encode(secret.user)").unwrap(), "ZGVwbG95"));
    assert!(f.is(
        f.eval("base64Decode(base64Encode(secret.db_password))")
            .unwrap(),
        PASSWORD
    ));
    assert!(f.is(f.eval("trim(secret.user)").unwrap(), "deploy"));
    // The same answer the function gives for plain text.
    assert_eq!(
        f.eval("urlEncode(\"p@ss/w0rd-9fXw\")").unwrap(),
        Value::from("p%40ss%2Fw0rd-9fXw")
    );
}

#[test]
fn every_other_function_refuses_a_secret_and_says_which_take_one() {
    let f = fixture();
    for call in [
        "upper(secret.db_password)",
        "toString(secret.db_password)",
        "toJson(secret.db_password)",
        "toJson({\"password\": secret.db_password})",
        "length(secret.db_password)",
        "sha256(secret.db_password)",
        "replace(secret.db_password, \"p\", \"q\")",
        "split(secret.db_password, \"/\")",
        "substr(secret.db_password, 0, 2)",
        "join([secret.user, secret.db_password], \":\")",
        "format(\"%s\", secret.db_password)",
        "startsWith(secret.db_password, \"p\")",
        "contains(secret.db_password, \"@\")",
        "regexMatch(secret.db_password, \"^p\")",
        "first([secret.db_password])",
        "quote(secret.db_password)",
    ] {
        let err = f.eval(call).unwrap_err();
        let name = call.split('(').next().unwrap();
        assert!(
            err.contains(&format!("{name}(...) cannot be given a secret"))
                && err.contains("base64Decode, base64Encode, trim, urlEncode"),
            "{call}: {err}"
        );
        clean(&err);
    }
}

#[test]
fn operators_other_than_comparison_and_joining_refuse_a_secret() {
    let f = fixture();
    for expr in [
        "secret.db_password < \"q\"",
        "secret.db_password >= secret.user",
        "secret.db_password * 2",
        "secret.db_password - 1",
        "secret.db_password matches \"^p\"",
        "secret.db_password in [\"a\", \"b\"]",
    ] {
        let Ok(parsed) = Expr::parse(expr) else {
            // Not every operator here is one the language has.
            continue;
        };
        let err = parsed.eval(&f.scope).unwrap_err();
        assert!(err.contains("secret"), "{expr}: {err}");
        clean(&err);
    }
    let err = f.eval("secret.db_password + 1").unwrap_err();
    assert!(err.contains("cannot add a secret and a number"), "{err}");
    let err = f.eval("!secret.db_password").unwrap_err();
    clean(&err);
    let err = Expr::parse_condition("secret.db_password")
        .unwrap()
        .truth(&f.scope)
        .unwrap_err();
    assert!(err.contains("is a secret, not true or false"), "{err}");
}

#[test]
fn a_comparison_gives_a_plain_answer() {
    let f = fixture();
    assert_eq!(
        f.eval("secret.db_password == \"p@ss/w0rd-9fXw\"").unwrap(),
        Value::Bool(true)
    );
    assert_eq!(
        f.eval("\"something else\" == secret.db_password").unwrap(),
        Value::Bool(false)
    );
    assert_eq!(
        f.eval("secret.db_password != secret.user").unwrap(),
        Value::Bool(true)
    );
    assert_eq!(
        f.eval("secret.user == secret.user").unwrap(),
        Value::Bool(true)
    );
    // A secret is text: it is never a number, a list or null.
    assert_eq!(f.eval("secret.user == 7").unwrap(), Value::Bool(false));
    assert_eq!(f.eval("secret.user != null").unwrap(), Value::Bool(true));
    // So a condition can branch on one.
    assert!(
        Expr::parse_condition("secret.user == \"deploy\" && data.port == 5432")
            .unwrap()
            .truth(&f.scope)
            .unwrap()
    );
}

#[test]
fn a_secret_passes_through_what_only_chooses_between_values() {
    let f = fixture();
    assert!(f.is(f.eval("secret.missing ?? secret.user").unwrap(), "deploy"));
    assert!(f.is(
        f.eval("default(secret.missing, secret.db_password)")
            .unwrap(),
        PASSWORD
    ));
    assert!(f.is(
        f.eval("coalesce(secret.missing, secret.user)").unwrap(),
        "deploy"
    ));
    assert!(f.is(
        f.eval("data.port == 5432 ? secret.user : secret.db_password")
            .unwrap(),
        "deploy"
    ));
    assert!(f.is(
        f.eval("required(secret.user, \"a user is needed\")")
            .unwrap(),
        "deploy"
    ));
    assert_eq!(f.eval("defined(secret.user)").unwrap(), Value::Bool(true));
    assert_eq!(
        f.eval("defined(secret.missing)").unwrap(),
        Value::Bool(false)
    );
    // Inside a list or a mapping it is still the reference.
    let list = f.eval("[secret.user, \"plain\"]").unwrap();
    assert_eq!(list.canonical(), "[\"(sensitive)\",\"plain\"]");
}

#[test]
fn a_secret_that_is_not_fetched_is_described_and_cannot_be_compared() {
    let f = fixture();
    let placed = f
        .place(concat!("token=$", "{{ urlEncode(secret.pending) }}\n"))
        .unwrap();
    let at = f.secret(placed);
    assert_eq!(f.table.describe(at), "token=urlEncode((secret pending))\n");
    let err = f.eval("secret.pending == \"x\"").unwrap_err();
    assert_eq!(
        err,
        "(secret pending) is not fetched here: a secret is fetched by plan and apply, never by \
         validate or compile"
    );
    // Whether it is null is whether it was found, which only a fetch can
    // say: an optional secret is null on a machine that does not have it.
    for asked in [
        "secret.pending != null",
        "secret.pending == null",
        "null == secret.pending",
    ] {
        assert_eq!(
            f.eval(asked).unwrap_err(),
            "(secret pending) is not fetched here: a secret is fetched by plan and apply, \
             never by validate or compile",
            "{asked}"
        );
    }
    // One that was fetched was found, and one that was wiped is not said to
    // be awaiting a fetch.
    assert_eq!(f.eval("secret.user == null").unwrap(), Value::Bool(false));
    assert_eq!(
        f.table.awaited(at),
        Some("token=urlEncode((secret pending))\n".to_owned())
    );
    f.table.wipe_all();
    assert_eq!(f.table.awaited(at), None);
}

#[test]
fn a_secret_cannot_be_read_where_no_secrets_are_held() {
    // A value that names a secret, in a place that was given no table.
    let scope = Scope::new().with(
        "secret",
        Value::Map(
            [("x".to_owned(), Value::Secret(SecretRef::new(0)))]
                .into_iter()
                .collect(),
        ),
    );
    let err = Expr::parse("secret.x + \"a\"")
        .unwrap()
        .eval(&scope)
        .unwrap_err();
    assert_eq!(
        err,
        "a secret cannot be read here: this place holds no secrets"
    );
    let err = Template::parse(concat!("a $", "{{ secret.x }}"))
        .unwrap()
        .eval(&scope)
        .unwrap_err();
    assert!(err.contains("this place holds no secrets"), "{err}");
}

#[test]
fn no_error_about_a_secret_holds_it() {
    let f = fixture();
    for expr in [
        "base64Decode(secret.db_password)",
        "toNumber(secret.db_password)",
        "secret.db_password.field",
        "secret.db_password[0]",
        "cidrHost(secret.db_password, 1)",
        "semverCompare(secret.db_password, \"1.0.0\")",
        "fromJson(secret.db_password)",
    ] {
        let err = f.eval(expr).unwrap_err();
        clean(&err);
    }
}
