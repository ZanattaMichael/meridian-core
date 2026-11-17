//! The expression language: every operator, what each makes of each pair of
//! types, precedence, and the errors.

use std::collections::BTreeMap;

use meridian_expr::{Expr, Names, Scope};
use meridian_value::{decode_value, Value};

const DATA: &str = r#"{
  "env": "prod",
  "count": 3,
  "ratio": 1.5,
  "install": true,
  "legacy": false,
  "nothing": null,
  "my-key": "hyphen",
  "region": {"name": "eu-west-1", "zones": 3, "tags": ["a", "b"]},
  "ports": [80, 443, 8080],
  "sites": [
    {"name": "shop", "port": 443, "tls": true},
    {"name": "blog", "port": 80, "tls": false}
  ],
  "labels": {"tier": "web", "env": "prod"}
}"#;

fn scope() -> Scope {
    Scope::new()
        .with("data", decode_value(DATA.as_bytes()).unwrap())
        .with_unset_hint(
            "data",
            "give it with --data, or test for it with defined({path})",
        )
        .with_env(BTreeMap::from([("HOME".to_owned(), "/root".to_owned())]))
}

fn json(text: &str) -> Value {
    decode_value(text.as_bytes()).unwrap()
}

fn eval(text: &str) -> Result<Value, String> {
    Expr::parse(text)?.eval(&scope())
}

/// Each case is an expression and the JSON its value must equal.
fn all_equal(cases: &[(&str, &str)]) {
    for (expression, want) in cases {
        let got = eval(expression).unwrap_or_else(|e| panic!("{expression}: {e}"));
        assert_eq!(
            got.canonical(),
            json(want).canonical(),
            "{expression} gave {}",
            got.canonical()
        );
    }
}

/// Each case is an expression and a part of the error it must give.
fn all_fail(cases: &[(&str, &str)]) {
    for (expression, want) in cases {
        match eval(expression) {
            Ok(value) => panic!("{expression} gave {} and should fail", value.canonical()),
            Err(e) => assert!(e.contains(want), "{expression}: {e}"),
        }
    }
}

#[test]
fn literals_are_themselves() {
    all_equal(&[
        ("true", "true"),
        ("false", "false"),
        ("null", "null"),
        ("3", "3"),
        ("-3", "-3"),
        ("1.5", "1.5"),
        ("-1.5", "-1.5"),
        ("'text'", "\"text\""),
        ("\"text\"", "\"text\""),
        ("'it\\'s'", "\"it's\""),
        ("'a\\nb'", "\"a\\nb\""),
        ("'a\\tb'", "\"a\\tb\""),
        ("'back\\\\slash'", "\"back\\\\slash\""),
        ("[]", "[]"),
        ("[1, 'a', true, null]", "[1, \"a\", true, null]"),
        ("[1, 2,]", "[1, 2]"),
        ("[[1], [2, 3]]", "[[1], [2, 3]]"),
        ("{}", "{}"),
        (
            "{name: 'web', port: 80}",
            "{\"name\": \"web\", \"port\": 80}",
        ),
        (
            "{'my-key': 1, \"other key\": 2}",
            "{\"my-key\": 1, \"other key\": 2}",
        ),
        ("{a: {b: [1]},}", "{\"a\": {\"b\": [1]}}"),
    ]);
}

#[test]
fn paths_read_the_scope() {
    all_equal(&[
        ("data.env", "\"prod\""),
        ("data.region.name", "\"eu-west-1\""),
        ("data.region['name']", "\"eu-west-1\""),
        ("data['region']['zones']", "3"),
        ("data.my-key", "\"hyphen\""),
        ("data['my-key']", "\"hyphen\""),
        ("data.ports[0]", "80"),
        ("data.ports[2]", "8080"),
        ("data.ports[-1]", "8080"),
        ("data.ports.1", "443"),
        ("data.sites[1].name", "\"blog\""),
        ("data.region.tags[data.count - 2]", "\"b\""),
        ("data.nothing", "null"),
        ("[10, 20][1]", "20"),
        ("{a: 1}.a", "1"),
        ("{a: 1}['a']", "1"),
    ]);
    all_fail(&[
        ("data.missing", "data.missing is not set (give it with --data, or test for it with defined(data.missing))"),
        ("data.region.missing", "data.region.missing is not set"),
        ("data.ports[3]", "data.ports[3] is not set"),
        ("data.ports.7", "data.ports.7 is not set"),
        ("data.ports[-4]", "data.ports[-4] is not set"),
        ("data['nope']", "data['nope'] is not set"),
        ("data.env.name", "data.env is a string, so it has no key \"name\""),
        ("data.count.x", "data.count is a number, so it has no key \"x\""),
        ("data.nothing.x", "data.nothing is null, so it has no key \"x\""),
        ("data.ports['a']", "data.ports is a list, so it is indexed with a number, not a string"),
        ("data.ports[1.5]", "an index into a list is a whole number"),
        ("data.region[0]", "data.region is a mapping, so it is indexed with text, not a number"),
        ("data.env[0]", "data.env is a string, so it cannot be indexed"),
    ]);
}

#[test]
fn equality_compares_values_of_the_same_type() {
    all_equal(&[
        ("data.env == 'prod'", "true"),
        ("data.env != 'prod'", "false"),
        ("3 == 3.0", "true"),
        ("data.count == 3.0", "true"),
        ("1.5 == data.ratio", "true"),
        // No silent conversion: text is never a number.
        ("'3' == 3", "false"),
        ("'3' != 3", "true"),
        ("'true' == true", "false"),
        ("null == null", "true"),
        ("data.nothing == null", "true"),
        ("null == false", "false"),
        ("0 == false", "false"),
        ("'' == null", "false"),
        ("[1, 2] == [1, 2]", "true"),
        ("[1, 2] == [1.0, 2.0]", "true"),
        ("[1, 2] == [2, 1]", "false"),
        ("[1] == [1, 1]", "false"),
        ("{a: 1, b: 2} == {b: 2, a: 1}", "true"),
        ("{a: 1} == {a: 1.0}", "true"),
        ("{a: 1} == {a: 2}", "false"),
        ("{a: 1} == {b: 1}", "false"),
        ("data.region.tags == ['a', 'b']", "true"),
    ]);
}

#[test]
fn ordering_is_for_two_numbers_or_two_texts() {
    all_equal(&[
        ("1 < 2", "true"),
        ("2 < 1", "false"),
        ("2 <= 2", "true"),
        ("2 >= 3", "false"),
        ("3 > 2.5", "true"),
        ("data.count >= 3", "true"),
        ("data.ratio < data.count", "true"),
        ("-1 < 0", "true"),
        ("'a' < 'b'", "true"),
        ("'b' <= 'a'", "false"),
        ("'abc' > 'ab'", "true"),
        ("'Z' < 'a'", "true"),
    ]);
    all_fail(&[
        ("1 < 'a'", "cannot compare a number and a string with <"),
        ("'a' >= 1", "cannot compare a string and a number with >="),
        (
            "true < false",
            "cannot compare a boolean and a boolean with <",
        ),
        ("null > 1", "cannot compare null and a number with >"),
        ("[1] < [2]", "cannot compare a list and a list with <"),
        ("data.env > 3", "cannot compare a string and a number"),
    ]);
}

#[test]
fn logic_takes_only_true_and_false_and_stops_early() {
    all_equal(&[
        ("true && true", "true"),
        ("true && false", "false"),
        ("false || true", "true"),
        ("false || false", "false"),
        ("!true", "false"),
        ("!!true", "true"),
        ("!data.legacy", "true"),
        // && binds tighter than ||.
        ("false && false || true", "true"),
        ("false && (false || true)", "false"),
        ("true || false && false", "true"),
        // The right side is not read when the left settles it.
        ("false && data.missing", "false"),
        ("true || data.missing", "true"),
        ("defined(data.missing) && data.missing == 1", "false"),
        ("!defined(data.missing) || data.missing", "true"),
    ]);
    all_fail(&[
        (
            "data.env && true",
            "data.env is a string, not true or false; compare it with == or !=",
        ),
        (
            "true && data.count",
            "data.count is a number, not true or false",
        ),
        ("!data.env", "not true or false"),
        ("!null", "null is null, not true or false"),
        ("1 || true", "1 is a number, not true or false"),
        ("true && data.missing", "data.missing is not set"),
        ("false || data.missing", "data.missing is not set"),
    ]);
}

#[test]
fn arithmetic_is_for_numbers_and_plus_also_joins() {
    all_equal(&[
        ("1 + 2", "3"),
        ("1 + 2.5", "3.5"),
        ("5 - 7", "-2"),
        ("2 * 3", "6"),
        ("2 * 1.5", "3.0"),
        ("6 / 3", "2"),
        ("7 / 2", "3.5"),
        ("7.0 / 2", "3.5"),
        ("7 % 3", "1"),
        ("-7 % 3", "-1"),
        ("7.5 % 2", "1.5"),
        ("1 + 2 * 3", "7"),
        ("(1 + 2) * 3", "9"),
        ("10 - 2 - 3", "5"),
        ("2 * 3 % 4", "2"),
        ("-data.count", "-3"),
        ("--3", "3"),
        ("- 3", "-3"),
        ("1 - -1", "2"),
        ("1-1", "0"),
        ("data.count + 1", "4"),
        ("data.count * data.ratio", "4.5"),
        ("'a' + 'b'", "\"ab\""),
        ("data.env + '-' + data.region.name", "\"prod-eu-west-1\""),
        ("[1] + [2, 3]", "[1, 2, 3]"),
        ("data.ports + [22]", "[80, 443, 8080, 22]"),
    ]);
    all_fail(&[
        (
            "'3' + 3",
            "cannot add a string and a number with +; convert one first, with toString or toNumber",
        ),
        ("3 + '3'", "cannot add a number and a string with +"),
        ("[1] + 1", "cannot add a list and a number"),
        ("{a: 1} + {b: 2}", "cannot add a mapping and a mapping"),
        ("true + 1", "cannot add a boolean and a number"),
        ("null + 1", "cannot add null and a number"),
        ("'a' - 'b'", "cannot calculate a string and a string with -"),
        ("'a' * 2", "cannot calculate a string and a number with *"),
        ("[1] / 2", "cannot calculate a list and a number with /"),
        ("1 / 0", "1 is divided by zero"),
        ("data.count % 0", "data.count is divided by zero"),
        ("1.0 / 0.0", "divided by zero"),
        ("-'a'", "is not a number, so it has no negative"),
        ("9223372036854775807 + 1", "too large for a whole number"),
        ("9223372036854775807 * 2", "too large for a whole number"),
        ("-9223372036854775807 - 2", "too large for a whole number"),
    ]);
}

#[test]
fn in_tests_a_list_a_mapping_or_text() {
    all_equal(&[
        ("80 in data.ports", "true"),
        ("81 in data.ports", "false"),
        ("80.0 in data.ports", "true"),
        ("'80' in data.ports", "false"),
        ("81 not in data.ports", "true"),
        ("80 not in data.ports", "false"),
        ("data.env in ['prod', 'staging']", "true"),
        ("data.env not in ['dev']", "true"),
        ("'tier' in data.labels", "true"),
        ("'zone' in data.labels", "false"),
        ("3 in data.labels", "false"),
        ("'west' in data.region.name", "true"),
        ("'east' in data.region.name", "false"),
        ("'' in 'abc'", "true"),
        ("[1] in [[1], [2]]", "true"),
        ("null in [null]", "true"),
        ("1 in []", "false"),
    ]);
    all_fail(&[
        (
            "1 in 2",
            "in tests for an item of a list, a key of a mapping or text inside text; 2 is a number",
        ),
        ("'a' in data.count", "data.count is a number"),
        ("1 in 'abc'", "in looks for text inside text; 1 is a number"),
        ("'a' not in null", "not in tests for"),
    ]);
}

#[test]
fn a_pattern_matches_text() {
    all_equal(&[
        ("data.env =~ '^pr'", "true"),
        ("data.env =~ '^dev'", "false"),
        ("data.env !~ '^dev'", "true"),
        ("data.env !~ 'prod'", "false"),
        ("data.region.name =~ '^eu-(west|east)-[0-9]+$'", "true"),
        ("'Web01' =~ '(?i)^web'", "true"),
    ]);
    all_fail(&[
        ("data.env =~ '('", "\"(\" is not a valid pattern"),
        (
            "data.count =~ '3'",
            "=~ matches text against a pattern; it was given a number and a string",
        ),
        ("'a' !~ 3", "!~ matches text against a pattern"),
    ]);
}

#[test]
fn a_choice_reads_only_the_side_it_takes() {
    all_equal(&[
        ("true ? 1 : 2", "1"),
        ("false ? 1 : 2", "2"),
        (
            "data.env == 'prod' ? 'm5.large' : 't3.micro'",
            "\"m5.large\"",
        ),
        ("data.legacy ? data.missing : 'safe'", "\"safe\""),
        ("data.install ? 'safe' : data.missing", "\"safe\""),
        // Right associative.
        ("false ? 1 : false ? 2 : 3", "3"),
        ("true ? false ? 1 : 2 : 3", "2"),
        ("1 < 2 ? 'a' : 'b'", "\"a\""),
        ("true || false ? [1] : [2]", "[1]"),
        ("(true ? 1 : 2) + 1", "2"),
    ]);
    all_fail(&[
        (
            "data.env ? 1 : 2",
            "data.env is a string, not true or false",
        ),
        ("true ? data.missing : 2", "data.missing is not set"),
    ]);
}

#[test]
fn the_null_fallback_answers_null_and_not_set_and_nothing_else() {
    all_equal(&[
        ("data.missing ?? 'fallback'", "\"fallback\""),
        ("data.nothing ?? 'fallback'", "\"fallback\""),
        ("data.env ?? 'fallback'", "\"prod\""),
        ("data.legacy ?? true", "false"),
        ("data.region.missing ?? data.region.name", "\"eu-west-1\""),
        ("data.a ?? data.b ?? 'c'", "\"c\""),
        ("data.missing ?? null", "null"),
        ("data.ports[9] ?? 0", "0"),
        // Binds looser than ||, tighter than the choice.
        ("data.missing ?? false || true", "true"),
        ("data.missing ?? true ? 1 : 2", "1"),
        ("(data.replicas ?? 1) + 1", "2"),
    ]);
    all_fail(&[
        // A mistake is not "missing".
        (
            "data.env.name ?? 'x'",
            "data.env is a string, so it has no key",
        ),
        (
            "data.missing ?? data.also_missing",
            "data.also_missing is not set",
        ),
        ("1 / 0 ?? 1", "divided by zero"),
    ]);
}

#[test]
fn precedence_runs_from_choice_down_to_postfix() {
    all_equal(&[
        ("1 + 2 == 3", "true"),
        ("1 + 2 < 2 * 2", "true"),
        ("1 < 2 == true", "true"),
        ("!false && true", "true"),
        ("!(false && true)", "true"),
        ("1 in [1] && 2 in [2]", "true"),
        ("-1 + 2", "1"),
        ("-(1 + 2)", "-3"),
        ("2 * -3", "-6"),
        ("[1, 2][0] + 1", "2"),
        ("{a: [1, 2]}.a[1] * 2", "4"),
        ("1 + 1 in [2]", "true"),
        ("'a' + 'b' =~ '^ab$'", "true"),
        ("1 == 1 && 2 == 2 || 3 == 4", "true"),
    ]);
}

#[test]
fn what_is_not_in_the_language_is_refused_when_it_is_read() {
    for (expression, want) in [
        ("", "the expression ends where a value was expected"),
        ("   ", "the expression ends where a value was expected"),
        ("data.", "a \".\" must be followed by a key"),
        ("data..a", "a \".\" must be followed by a key"),
        (
            "data.a ==",
            "the expression ends where a value was expected",
        ),
        (
            "data.a == == 1",
            "unexpected \"==\" where a value was expected",
        ),
        ("(data.a", "expected \")\", but the expression ends"),
        (
            "data.a)",
            "unexpected \")\" after the end of the expression",
        ),
        ("data.a data.b", "after the end of the expression"),
        (
            "1 == 1 == 1",
            "unexpected \"==\" after the end of the expression",
        ),
        (
            "1 < 2 < 3",
            "unexpected \"<\" after the end of the expression",
        ),
        (
            "data.a = 1",
            "unexpected \"=\" (to compare two values in a expression, write ==)",
        ),
        ("data.a @ 1", "unexpected \"@\""),
        ("data.a & data.b", "unexpected \"&\""),
        ("data.a | data.b", "unexpected \"|\""),
        ("'open", "a quoted text is never closed"),
        ("\"open\\", "a quoted text is never closed"),
        ("[1, 2", "expected \",\", but the expression ends"),
        ("[1 2]", "expected \",\", found 2"),
        ("{a 1}", "expected \":\", found 1"),
        ("{a: 1", "expected \",\", but the expression ends"),
        (
            "{1: 2}",
            "a key of a mapping is a name or quoted text, found 1",
        ),
        ("{a: 1, a: 2}", "the key \"a\" is set twice in one mapping"),
        ("{", "expected a key or \"}\", but the expression ends"),
        ("true ? 1", "expected \":\", but the expression ends"),
        (
            "true ? 1 : ",
            "the expression ends where a value was expected",
        ),
        ("f(1,", "the expression ends where a value was expected"),
        ("f(1 2)", "expected \",\", found 2"),
        ("data[0", "expected \"]\", but the expression ends"),
        ("* 2", "unexpected \"*\" where a value was expected"),
        (
            "1 not 2",
            "unexpected \"not\" after the end of the expression",
        ),
        (
            "99999999999999999999999999999999999999999999 + a.b.c.d.e.f.g.h.1x",
            "unexpected",
        ),
    ] {
        let err = Expr::parse(expression).expect_err(expression);
        assert!(err.contains(want), "{expression:?}: {err}");
    }
}

#[test]
fn an_expression_nested_too_deep_is_refused_and_does_not_overflow() {
    let deep = format!("{}1{}", "(".repeat(5000), ")".repeat(5000));
    let err = Expr::parse(&deep).expect_err("deep nesting");
    assert!(err.contains("nested more than"), "{err}");
    let deep = format!("{}true", "!".repeat(5000));
    assert!(Expr::parse(&deep).is_err());
    let deep = format!("{}1{}", "[".repeat(5000), "]".repeat(5000));
    assert!(Expr::parse(&deep).is_err());
}

#[test]
fn a_long_chain_of_operators_is_refused_and_does_not_overflow() {
    // A chain is not nested, but its tree is as deep as it is long.
    for chain in [
        format!("{}true", "true || ".repeat(10_000)),
        format!("{}1", "1 + ".repeat(10_000)),
        format!("{}1", "1 * ".repeat(10_000)),
        format!("x{}", ".a".repeat(10_000)),
        format!("x{}", "[0]".repeat(10_000)),
        format!("{}1", "true ? 1 : ".repeat(10_000)),
    ] {
        let err = Expr::parse(&chain).expect_err("a long chain");
        assert!(
            err.contains("more than 1000 operators") || err.contains("nested more than"),
            "{err}"
        );
    }
    // The longest chain allowed is read and evaluated on a small stack
    // (smaller than any thread's, in a debug build), so the chain itself
    // costs no stack: an editor's worker thread survives it too.
    let handle = std::thread::Builder::new()
        .stack_size(512 << 10)
        .spawn(|| {
            let scope = Scope::new();
            let chain = format!("{}1", "1 + ".repeat(1000));
            let expr = Expr::parse(&chain).unwrap();
            assert_eq!(expr.eval(&scope).unwrap(), Value::Int(1001));
            let chain = format!("{}false", "true && ".repeat(999));
            let expr = Expr::parse(&chain).unwrap();
            assert_eq!(expr.eval(&scope).unwrap(), Value::Bool(false));
            let chain = format!("{}'last'", "null ?? ".repeat(1000));
            let expr = Expr::parse(&chain).unwrap();
            assert_eq!(expr.eval(&scope).unwrap(), Value::String("last".to_owned()));
            let path = format!("data{}", ".a".repeat(1000));
            let expr = Expr::parse(&path).unwrap();
            let err = expr.eval(&scope).unwrap_err();
            assert!(err.contains("data"), "{err}");
            let path = format!("data{}", "[0]".repeat(1000));
            let expr = Expr::parse(&path).unwrap();
            let err = expr.eval(&scope).unwrap_err();
            assert!(err.contains("data"), "{err}");
            let chain = format!("{}3", "1 + 2 * ".repeat(500));
            let expr = Expr::parse(&chain).unwrap();
            assert_eq!(expr.eval(&scope).unwrap(), Value::Int(1 + 499 * 2 + 2 * 3));
        })
        .unwrap();
    handle.join().unwrap();
    // What a person writes is far inside the limit.
    let fine = format!("{}1{}", "(".repeat(12), ")".repeat(12));
    assert_eq!(eval(&fine), Ok(Value::Int(1)));
}

#[test]
fn names_are_checked_without_any_data() {
    let names = Names::new()
        .allow("data")
        .allow("param")
        .refuse("facts", "facts are not available here")
        .hint("this place reads data.<key> and param.<name>");
    for expression in [
        "data.env == 'prod'",
        "param.port > 0 && data.x",
        "param",
        "length(param) > 0",
        "defined(data.a.b)",
        "map(data.sites, 's', s.name + param.suffix)",
        "filter(data.ports, 'p', p > 100)",
        "map(data.a, 'x', map(x.items, 'y', y + x.base))",
        "{a: data.x, b: [param.y]}",
        "data.a ?? param.b ?? 'c'",
        "data[param.key]",
    ] {
        Expr::parse(expression)
            .unwrap()
            .check(&names)
            .unwrap_or_else(|e| panic!("{expression}: {e}"));
    }
    for (expression, want) in [
        (
            "env == 'prod'",
            "unknown name \"env\" (this place reads data.<key> and param.<name>)",
        ),
        ("data.a == other.b", "unknown name \"other\""),
        ("facts.os == 'x'", "facts are not available here"),
        ("defined(facts.os)", "facts are not available here"),
        (
            "data",
            "data must be followed by a key, as in data.environment",
        ),
        ("data == 1", "data must be followed by a key"),
        ("defined(env)", "unknown name \"env\""),
        ("defined('x')", "defined(...) takes a data.<key> path"),
        ("defined(1 + 1)", "defined(...) takes a data.<key> path"),
        ("nosuch(1)", "unknown function \"nosuch\""),
        (
            "Upper('a')",
            "unknown function \"Upper\" (did you mean upper?)",
        ),
        (
            "tostring(1)",
            "unknown function \"tostring\" (did you mean toString?)",
        ),
        ("upper()", "upper(...) takes 1 argument, and was given 0"),
        (
            "upper('a', 'b')",
            "upper(...) takes 1 argument, and was given 2",
        ),
        (
            "replace('a', 'b')",
            "replace(...) takes 3 arguments, and was given 2",
        ),
        (
            "substr('a')",
            "substr(...) takes 2 to 3 arguments, and was given 1",
        ),
        (
            "coalesce()",
            "coalesce(...) takes at least 1 argument, and was given 0",
        ),
        (
            "map(data.a, x, x)",
            "the second argument of map(...) is the name each item is given",
        ),
        ("map(data.a, 'x', y)", "unknown name \"y\""),
        // The name is bound only inside the third argument.
        ("map(data.a, 'x', x) == x", "unknown name \"x\""),
        ("[1, nope]", "unknown name \"nope\""),
        ("{a: nope.b}", "unknown name \"nope\""),
        ("true ? nope : 1", "unknown name \"nope\""),
        ("data[nope]", "unknown name \"nope\""),
    ] {
        let err = Expr::parse(expression)
            .and_then(|e| e.check(&names))
            .expect_err(expression);
        assert!(err.contains(want), "{expression}: {err}");
    }
}

#[test]
fn the_roots_an_expression_reads_are_reported() {
    let roots =
        |text: &str| -> Vec<String> { Expr::parse(text).unwrap().roots().into_iter().collect() };
    assert_eq!(roots("1 + 1"), Vec::<String>::new());
    assert_eq!(roots("data.a == param.b && data.c"), ["data", "param"]);
    assert_eq!(roots("map(data.a, 'x', x.n + var.m)"), ["data", "var"]);
    assert_eq!(roots("secret.db ?? env('X')"), ["secret"]);
    assert_eq!(
        roots("{a: [cond.x ? outputs.net.id : null]}"),
        ["cond", "outputs"]
    );
}

#[test]
fn a_condition_must_be_a_yes_or_a_no() {
    let truth = |text: &str| Expr::parse_condition(text)?.truth(&scope());
    assert_eq!(truth("data.install"), Ok(true));
    assert_eq!(truth("data.count > 2 && data.env in ['prod']"), Ok(true));
    assert_eq!(truth("length(data.ports) == 0"), Ok(false));
    for (condition, want) in [
        (
            "data.env",
            "data.env is a string, not true or false; compare it with == or !=",
        ),
        ("data.count", "data.count is a number, not true or false"),
        ("data.nothing", "data.nothing is null, not true or false"),
        ("data.ports", "data.ports is a list, not true or false"),
        ("data.region", "data.region is a mapping, not true or false"),
        ("'text'", "not true or false"),
        ("1 + 1", "the expression is a number, not true or false"),
        (
            "upper('a')",
            "the expression is a string, not true or false",
        ),
    ] {
        let err = truth(condition).expect_err(condition);
        assert!(err.contains(want), "{condition}: {err}");
    }
    // A condition's own diagnostics call it a condition.
    let err = Expr::parse_condition("").unwrap_err();
    assert_eq!(err, "the condition ends where a value was expected");
}

#[test]
fn the_same_expression_over_the_same_scope_is_always_the_same_value() {
    let scope = scope();
    for expression in [
        "sort(keys(data.labels))",
        "groupBy(data.sites, 'tls')",
        "merge(data.labels, {zone: 'a'})",
        "unique(data.ports + data.ports)",
        "toJson(data.region)",
        "sha256(toJson(data))",
        "map(data.sites, 's', {n: s.name, p: s.port * 2})",
    ] {
        let expr = Expr::parse(expression).unwrap();
        let first = expr.eval(&scope).unwrap().canonical();
        for _ in 0..20 {
            assert_eq!(
                expr.eval(&scope).unwrap().canonical(),
                first,
                "{expression}"
            );
        }
    }
}
