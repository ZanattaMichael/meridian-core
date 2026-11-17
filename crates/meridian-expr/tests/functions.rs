//! The function library: every function with ordinary arguments, at its
//! edges, and with arguments of the wrong type.

use std::collections::{BTreeMap, BTreeSet};

use meridian_expr::{function_names, Expr, Scope};
use meridian_value::{decode_value, Value};

const DATA: &str = r#"{
  "env": "prod",
  "count": 3,
  "nothing": null,
  "ports": [80, 443, 8080],
  "sites": [
    {"name": "shop", "port": 443, "tls": true, "owner": "web"},
    {"name": "blog", "port": 80, "tls": false, "owner": "web"},
    {"name": "api", "port": 8443, "tls": true, "owner": "core"}
  ],
  "labels": {"tier": "web", "env": "prod"},
  "nested": {"a": {"x": 1, "y": 2}, "list": [1, 2]}
}"#;

fn scope() -> Scope {
    Scope::new()
        .with("data", decode_value(DATA.as_bytes()).unwrap())
        .with_env(BTreeMap::from([
            ("HOME".to_owned(), "/root".to_owned()),
            ("EMPTY".to_owned(), String::new()),
        ]))
}

fn eval(text: &str) -> Result<Value, String> {
    let expr = Expr::parse(text)?;
    expr.eval(&scope())
}

fn all_equal(cases: &[(&str, &str)]) {
    for (expression, want) in cases {
        let got = eval(expression).unwrap_or_else(|e| panic!("{expression}: {e}"));
        let want = decode_value(want.as_bytes()).unwrap_or_else(|e| panic!("{want}: {e}"));
        assert_eq!(
            got.canonical(),
            want.canonical(),
            "{expression} gave {}",
            got.canonical()
        );
    }
}

fn all_fail(cases: &[(&str, &str)]) {
    for (expression, want) in cases {
        match eval(expression) {
            Ok(value) => panic!("{expression} gave {} and should fail", value.canonical()),
            Err(e) => assert!(e.contains(want), "{expression}: {e}"),
        }
    }
}

#[test]
fn presence() {
    all_equal(&[
        ("defined(data.env)", "true"),
        ("defined(data.missing)", "false"),
        ("defined(data.nothing)", "true"),
        ("defined(data.nested.a.x)", "true"),
        ("defined(data.nested.a.z)", "false"),
        ("defined(data.ports[1])", "true"),
        ("defined(data.ports[9])", "false"),
        ("defined(data['env'])", "true"),
        ("coalesce(data.missing, data.nothing, 'c')", "\"c\""),
        ("coalesce(data.env, data.missing)", "\"prod\""),
        ("coalesce(null)", "null"),
        ("coalesce(data.missing, false)", "false"),
        ("default(data.missing, 5)", "5"),
        ("default(data.nothing, 5)", "5"),
        ("default(data.count, 5)", "3"),
        ("default(data.missing, data.env)", "\"prod\""),
        ("required(data.env, 'env is needed')", "\"prod\""),
    ]);
    all_fail(&[
        (
            "required(data.nothing, 'a region must be chosen')",
            "a region must be chosen",
        ),
        ("required(data.missing, 'x')", "data.missing is not set"),
        (
            "defined(data.env.deeper)",
            "data.env is a string, so it has no key",
        ),
        ("default(data.missing, data.also)", "data.also is not set"),
        ("coalesce(data.env.x, 1)", "so it has no key"),
    ]);
}

#[test]
fn text() {
    all_equal(&[
        ("lower('WeB')", "\"web\""),
        ("upper('web')", "\"WEB\""),
        ("upper('')", "\"\""),
        ("title('hello wide-world')", "\"Hello Wide-World\""),
        ("trim('  a b \\n')", "\"a b\""),
        ("trimPrefix('web-01', 'web-')", "\"01\""),
        ("trimPrefix('web-01', 'db-')", "\"web-01\""),
        ("trimSuffix('site.conf', '.conf')", "\"site\""),
        ("trimSuffix('site.conf', '.yaml')", "\"site.conf\""),
        ("replace('a-b-c', '-', '_')", "\"a_b_c\""),
        ("replace('abc', 'x', 'y')", "\"abc\""),
        ("split('a,b,c', ',')", "[\"a\", \"b\", \"c\"]"),
        ("split('abc', ',')", "[\"abc\"]"),
        ("split('', ',')", "[]"),
        ("split('a,,b', ',')", "[\"a\", \"\", \"b\"]"),
        ("join(['a', 'b'], '-')", "\"a-b\""),
        ("join([], '-')", "\"\""),
        ("join([1, true, 'x', 1.5], ',')", "\"1,true,x,1.5\""),
        ("join(data.ports, ' ')", "\"80 443 8080\""),
        (
            "format('%s.%s.example.com', 'web', data.env)",
            "\"web.prod.example.com\"",
        ),
        ("format('%d ports', length(data.ports))", "\"3 ports\""),
        ("format('100%%')", "\"100%\""),
        ("format('plain')", "\"plain\""),
        ("format('%s', true)", "\"true\""),
        ("startsWith('web01', 'web')", "true"),
        ("startsWith('web01', 'db')", "false"),
        ("endsWith('a.conf', '.conf')", "true"),
        ("endsWith('a.conf', '.yaml')", "false"),
        ("contains('eu-west-1', 'west')", "true"),
        ("contains(data.ports, 443)", "true"),
        ("contains(data.ports, 22)", "false"),
        ("contains(data.labels, 'tier')", "true"),
        ("contains(data.labels, 'zone')", "false"),
        ("substr('abcdef', 2)", "\"cdef\""),
        ("substr('abcdef', 1, 3)", "\"bcd\""),
        ("substr('abc', 5)", "\"\""),
        ("substr('abc', 1, 99)", "\"bc\""),
        ("substr('héllo', 1, 2)", "\"él\""),
        ("indent('a\\nb', 2)", "\"  a\\n  b\""),
        ("indent('a\\n\\nb', 4)", "\"    a\\n\\n    b\""),
        ("indent('a', 0)", "\"a\""),
        ("quote('say \"hi\"')", "\"\\\"say \\\\\\\"hi\\\\\\\"\\\"\""),
        ("regexMatch('web01', '^web[0-9]+$')", "true"),
        ("regexMatch('db01', '^web')", "false"),
        ("regexReplace('web01', '[0-9]+', 'N')", "\"webN\""),
        ("regexReplace('a-b', '(a)-(b)', '$2-$1')", "\"b-a\""),
        ("regexFind('web01.example.com', '[0-9]+')", "\"01\""),
        ("regexFind('web', '[0-9]+')", "null"),
        ("length('héllo')", "5"),
    ]);
    all_fail(&[
        (
            "lower(1)",
            "argument 1 of lower(...) must be text, and is a number",
        ),
        (
            "upper(null)",
            "argument 1 of upper(...) must be text, and is null",
        ),
        (
            "trim([' a '])",
            "argument 1 of trim(...) must be text, and is a list",
        ),
        (
            "replace('a', 1, 'b')",
            "argument 2 of replace(...) must be text",
        ),
        (
            "split('a', '')",
            "split(...) needs a separator that is not empty",
        ),
        ("split(['a'], ',')", "argument 1 of split(...) must be text"),
        (
            "join('abc', ',')",
            "argument 1 of join(...) must be a list, and is a string",
        ),
        (
            "join([[1]], ',')",
            "a list has no single text form; use join(...) or toJson(...)",
        ),
        ("join([{a: 1}], ',')", "a mapping has no single text form"),
        ("format('%s')", "format(...) has more % places than values"),
        (
            "format('%s', 1, 2)",
            "format(...) was given more values than",
        ),
        (
            "format('%d', 'x')",
            "%d in format(...) takes a whole number, and was given a string",
        ),
        (
            "format('%d', 1.5)",
            "%d in format(...) takes a whole number",
        ),
        (
            "format('%x', 1)",
            "format(...) understands %s, %d and %%, not %x",
        ),
        (
            "format('trailing %')",
            "format(...) understands %s, %d and %%",
        ),
        ("format('%s', [1])", "a list has no single text form"),
        (
            "startsWith(1, 'a')",
            "argument 1 of startsWith(...) must be text",
        ),
        (
            "contains(1, 2)",
            "argument 1 of contains(...) must be a list, a mapping or text",
        ),
        ("contains('abc', 1)", "argument 1 of contains(...)"),
        (
            "substr('abc', -1)",
            "substr(...) takes a start and a length that are not negative",
        ),
        (
            "substr('abc', 'x')",
            "argument 2 of substr(...) must be a whole number",
        ),
        (
            "indent('a', -1)",
            "argument 2 of indent(...) must be a count that is not negative",
        ),
        ("regexMatch('a', '[')", "\"[\" is not a valid pattern"),
        (
            "regexFind(1, 'a')",
            "argument 1 of regexFind(...) must be text",
        ),
    ]);
}

#[test]
fn numbers() {
    all_equal(&[
        ("min(3, 1, 2)", "1"),
        ("max(3, 1, 2)", "3"),
        ("min([3, 1, 2])", "1"),
        ("max(data.ports)", "8080"),
        ("min(1, 0.5)", "0.5"),
        ("max(1)", "1"),
        ("min(-1, -2)", "-2"),
        ("abs(-3)", "3"),
        ("abs(3)", "3"),
        ("abs(-1.5)", "1.5"),
        ("floor(1.9)", "1"),
        ("floor(-1.1)", "-2"),
        ("floor(2)", "2"),
        ("ceil(1.1)", "2"),
        ("ceil(-1.9)", "-1"),
        ("round(1.5)", "2"),
        ("round(1.4)", "1"),
        ("round(-1.5)", "-2"),
        ("round(3)", "3"),
        ("round(3.14159, 2)", "3.14"),
        ("round(2.5, 0)", "3"),
        ("sum([1, 2, 3])", "6"),
        ("sum([])", "0"),
        ("sum([1, 0.5])", "1.5"),
        ("sum(data.ports)", "8603"),
    ]);
    all_fail(&[
        (
            "min('a', 1)",
            "min(...) takes numbers, and was given a string",
        ),
        ("max([])", "max(...) was given no numbers"),
        ("max([1, 'a'])", "max(...) takes numbers"),
        ("abs('x')", "argument 1 of abs(...) must be a number"),
        ("floor(null)", "argument 1 of floor(...) must be a number"),
        ("round(1.5, 20)", "round(...) takes 0 to 15 decimal places"),
        ("round(1.5, -1)", "round(...) takes 0 to 15 decimal places"),
        (
            "sum([1, 'a'])",
            "sum(...) takes a list of numbers; item 1 is a string",
        ),
        ("sum(3)", "argument 1 of sum(...) must be a list"),
        (
            "sum([9223372036854775807, 1])",
            "too large for a whole number",
        ),
    ]);
}

#[test]
fn lists_and_mappings() {
    all_equal(&[
        ("length([1, 2])", "2"),
        ("length([])", "0"),
        ("length({a: 1})", "1"),
        ("length('')", "0"),
        ("keys({b: 1, a: 2})", "[\"a\", \"b\"]"),
        ("keys({})", "[]"),
        ("values({b: 1, a: 2})", "[2, 1]"),
        ("entries({a: 1})", "[{\"key\": \"a\", \"value\": 1}]"),
        ("entries({})", "[]"),
        ("merge({a: 1}, {b: 2})", "{\"a\": 1, \"b\": 2}"),
        ("merge({a: 1}, {a: 2})", "{\"a\": 2}"),
        ("merge({a: {x: 1}}, {a: {y: 2}})", "{\"a\": {\"y\": 2}}"),
        ("merge({a: 1})", "{\"a\": 1}"),
        ("merge({a: 1}, {b: 2}, {a: 3})", "{\"a\": 3, \"b\": 2}"),
        (
            "deepMerge({a: {x: 1}}, {a: {y: 2}})",
            "{\"a\": {\"x\": 1, \"y\": 2}}",
        ),
        ("deepMerge({a: {x: 1}}, {a: {x: 2}})", "{\"a\": {\"x\": 2}}"),
        ("deepMerge({a: [1]}, {a: [2]})", "{\"a\": [2]}"),
        ("deepMerge({a: {x: 1}}, {a: 5})", "{\"a\": 5}"),
        (
            "deepMerge(data.nested, {a: {z: 3}}).a",
            "{\"x\": 1, \"y\": 2, \"z\": 3}",
        ),
        ("concat([1], [2], [3, 4])", "[1, 2, 3, 4]"),
        ("concat([])", "[]"),
        ("flatten([[1, 2], [3], 4])", "[1, 2, 3, 4]"),
        ("flatten([[[1]], 2])", "[[1], 2]"),
        ("flatten([])", "[]"),
        ("unique([1, 2, 1, 3, 2])", "[1, 2, 3]"),
        ("unique(['a', 'a'])", "[\"a\"]"),
        ("unique([1, 1.0, '1'])", "[1, \"1\"]"),
        ("unique([{a: 1}, {a: 1}])", "[{\"a\": 1}]"),
        ("sort([3, 1, 2])", "[1, 2, 3]"),
        ("sort([1.5, 1, -2])", "[-2, 1, 1.5]"),
        ("sort(['b', 'a', 'C'])", "[\"C\", \"a\", \"b\"]"),
        ("sort([])", "[]"),
        ("reverse([1, 2, 3])", "[3, 2, 1]"),
        ("reverse('abc')", "\"cba\""),
        ("first([1, 2])", "1"),
        ("last([1, 2])", "2"),
        ("slice([1, 2, 3, 4], 1)", "[2, 3, 4]"),
        ("slice([1, 2, 3, 4], 1, 3)", "[2, 3]"),
        ("slice([1, 2, 3, 4], -2)", "[3, 4]"),
        ("slice([1, 2, 3, 4], 0, -1)", "[1, 2, 3]"),
        ("slice([1, 2], 5)", "[]"),
        ("slice([1, 2], 1, 0)", "[]"),
        ("range(3)", "[0, 1, 2]"),
        ("range(0)", "[]"),
        ("range(2, 5)", "[2, 3, 4]"),
        ("range(5, 2)", "[]"),
        ("range(0, 10, 3)", "[0, 3, 6, 9]"),
        ("range(5, 0, -2)", "[5, 3, 1]"),
        ("range(-2, 1)", "[-2, -1, 0]"),
        ("zip([1, 2], ['a', 'b'])", "[[1, \"a\"], [2, \"b\"]]"),
        ("zip([1, 2, 3], ['a'])", "[[1, \"a\"]]"),
        ("lookup({a: 1}, 'a')", "1"),
        ("lookup({a: 1}, 'b', 0)", "0"),
        ("lookup(data.labels, data.env, 'none')", "\"none\""),
        (
            "pick({a: 1, b: 2, c: 3}, ['a', 'c'])",
            "{\"a\": 1, \"c\": 3}",
        ),
        ("pick({a: 1}, ['z'])", "{}"),
        ("omit({a: 1, b: 2, c: 3}, ['a', 'c'])", "{\"b\": 2}"),
        ("omit({a: 1}, [])", "{\"a\": 1}"),
        ("any([false, true])", "true"),
        ("any([false, false])", "false"),
        ("any([])", "false"),
        ("all([true, true])", "true"),
        ("all([true, false])", "false"),
        ("all([])", "true"),
    ]);
    all_fail(&[
        (
            "length(3)",
            "argument 1 of length(...) must be text, a list or a mapping",
        ),
        (
            "keys([1])",
            "argument 1 of keys(...) must be a mapping, and is a list",
        ),
        ("values('a')", "argument 1 of values(...) must be a mapping"),
        (
            "merge({a: 1}, [1])",
            "argument 2 of merge(...) must be a mapping, and is a list",
        ),
        (
            "deepMerge(1)",
            "argument 1 of deepMerge(...) must be a mapping",
        ),
        ("concat([1], 2)", "argument 2 of concat(...) must be a list"),
        ("flatten('a')", "argument 1 of flatten(...) must be a list"),
        (
            "sort([1, 'a'])",
            "sort(...) orders a list of numbers or a list of text, not a mix",
        ),
        ("sort([[1]])", "not a mix"),
        (
            "reverse(1)",
            "argument 1 of reverse(...) must be a list or text",
        ),
        ("first([])", "first(...) was given an empty list"),
        ("last([])", "last(...) was given an empty list"),
        ("first('abc')", "argument 1 of first(...) must be a list"),
        (
            "slice([1], 'a')",
            "argument 2 of slice(...) must be a whole number",
        ),
        (
            "range(0, 10, 0)",
            "range(...) takes a step that is not zero",
        ),
        (
            "range(1000000)",
            "range(...) would build 1000000 items; the most is 10000",
        ),
        (
            "range(1.5)",
            "argument 1 of range(...) must be a whole number",
        ),
        (
            "range(-9223372036854775807, 9223372036854775807)",
            "the most is 10000",
        ),
        (
            "range(9223372036854775807, -9223372036854775807, -1)",
            "the most is 10000",
        ),
        ("zip([1], 2)", "argument 2 of zip(...) must be a list"),
        (
            "lookup({a: 1}, 'b')",
            "lookup(...) found no key \"b\", and was given no fallback",
        ),
        (
            "lookup([1], 'a')",
            "argument 1 of lookup(...) must be a mapping",
        ),
        (
            "pick({a: 1}, [1])",
            "argument 2 of pick(...) must be a list of keys, as text",
        ),
        (
            "omit({a: 1}, 'a')",
            "argument 2 of omit(...) must be a list",
        ),
        (
            "any([1])",
            "any(...) takes a list of true and false, and found a number in it",
        ),
        ("all(['true'])", "all(...) takes a list of true and false"),
    ]);
}

#[test]
fn map_and_filter_run_an_expression_over_each_item() {
    all_equal(&[
        ("map([1, 2, 3], 'n', n * 2)", "[2, 4, 6]"),
        ("map([], 'n', n * 2)", "[]"),
        ("map(data.sites, 's', s.name)", "[\"shop\", \"blog\", \"api\"]"),
        (
            "map(data.sites, 's', {id: format('site_%s', s.name), secure: s.tls})",
            "[{\"id\": \"site_shop\", \"secure\": true}, {\"id\": \"site_blog\", \"secure\": false}, {\"id\": \"site_api\", \"secure\": true}]",
        ),
        ("filter([1, 2, 3, 4], 'n', n % 2 == 0)", "[2, 4]"),
        ("filter(data.sites, 's', s.tls && s.port > 1000)", "[{\"name\": \"api\", \"owner\": \"core\", \"port\": 8443, \"tls\": true}]"),
        ("filter([1], 'n', false)", "[]"),
        ("map(filter(data.sites, 's', s.tls), 's', s.port)", "[443, 8443]"),
        // The item can read what is outside it.
        ("map([1, 2], 'n', n + data.count)", "[4, 5]"),
        // Nested, the inner name hides the outer one of the same name.
        ("map([[1, 2], [3]], 'row', map(row, 'n', n * length(row)))", "[[2, 4], [3]]"),
        ("map([1, 2], 'x', map([10], 'x', x))", "[[10], [10]]"),
        ("sum(map(data.sites, 's', s.port))", "8966"),
        ("any(map(data.sites, 's', s.owner == 'core'))", "true"),
        ("all(map(data.sites, 's', s.port > 0))", "true"),
        ("length(filter(data.ports, 'p', p in [80, 443]))", "2"),
        // A name of the item can be the name of a root; inside, it is the item.
        ("map([1], 'data', data + 1)", "[2]"),
    ]);
    all_fail(&[
        (
            "map(3, 'n', n)",
            "map(...) takes a list as its first argument; 3 is not one",
        ),
        (
            "filter(data.env, 'n', true)",
            "filter(...) takes a list as its first argument; data.env is not one",
        ),
        ("filter([1, 2], 'n', n)", "n is a number, not true or false"),
        (
            "map([1], 'n', n.x)",
            "n is a number, so it has no key \"x\"",
        ),
        ("map([1, 0], 'n', 1 / n)", "divided by zero"),
        ("map(data.sites, 's', s.missing)", "s.missing is not set"),
    ]);
}

#[test]
fn selection_from_a_list_of_mappings() {
    all_equal(&[
        ("pluck(data.sites, 'name')", "[\"shop\", \"blog\", \"api\"]"),
        ("pluck(data.sites, 'missing')", "[]"),
        ("pluck([], 'name')", "[]"),
        (
            "pluck(where(data.sites, 'tls', true), 'name')",
            "[\"shop\", \"api\"]",
        ),
        (
            "pluck(where(data.sites, 'owner', 'core'), 'port')",
            "[8443]",
        ),
        ("where(data.sites, 'port', 1)", "[]"),
        ("where(data.sites, 'port', 80.0)[0].name", "\"blog\""),
        ("keys(groupBy(data.sites, 'owner'))", "[\"core\", \"web\"]"),
        (
            "pluck(groupBy(data.sites, 'owner').web, 'name')",
            "[\"shop\", \"blog\"]",
        ),
        ("keys(groupBy(data.sites, 'tls'))", "[\"false\", \"true\"]"),
        (
            "keys(groupBy(data.sites, 'port'))",
            "[\"443\", \"80\", \"8443\"]",
        ),
        ("groupBy([], 'x')", "{}"),
    ]);
    all_fail(&[
        (
            "pluck([1], 'a')",
            "pluck(...) takes a list of mappings, and found a number in it",
        ),
        (
            "where(['a'], 'k', 1)",
            "where(...) takes a list of mappings, and found a string in it",
        ),
        ("groupBy([{a: [1]}], 'a')", "a list has no single text form"),
        (
            "where(data.sites, 1, 2)",
            "argument 2 of where(...) must be text",
        ),
    ]);
}

#[test]
fn types_and_conversions_are_asked_for_and_never_guessed() {
    all_equal(&[
        ("type(null)", "\"null\""),
        ("type(true)", "\"bool\""),
        ("type(1)", "\"number\""),
        ("type(1.5)", "\"number\""),
        ("type('a')", "\"string\""),
        ("type([])", "\"list\""),
        ("type({})", "\"map\""),
        ("toString(1)", "\"1\""),
        ("toString(1.5)", "\"1.5\""),
        ("toString(true)", "\"true\""),
        ("toString('a')", "\"a\""),
        ("toString(null)", "\"null\""),
        ("toNumber('3')", "3"),
        ("toNumber(' 3 ')", "3"),
        ("toNumber('-1.5')", "-1.5"),
        ("toNumber(3)", "3"),
        ("toNumber('3') + 1", "4"),
        ("toBool('true')", "true"),
        ("toBool('false')", "false"),
        ("toBool(true)", "true"),
        (
            "toJson({b: [1, 'x'], a: null})",
            "\"{\\\"a\\\":null,\\\"b\\\":[1,\\\"x\\\"]}\"",
        ),
        ("toJson('a<b&c')", "\"\\\"a<b&c\\\"\""),
        ("toJson('line\\nbreak')", "\"\\\"line\\\\nbreak\\\"\""),
        ("toJson(1.5)", "\"1.5\""),
        ("fromJson('{\"a\": [1, 2]}').a[1]", "2"),
        ("fromJson('3')", "3"),
        ("fromJson(toJson(data.nested)) == data.nested", "true"),
    ]);
    all_fail(&[
        (
            "toString([1])",
            "a list has no single text form; use join(...) or toJson(...)",
        ),
        ("toString({})", "a mapping has no single text form"),
        ("toNumber('abc')", "toNumber(...): \"abc\" is not a number"),
        ("toNumber('')", "is not a number"),
        ("toNumber('inf')", "is not a number"),
        (
            "toNumber(true)",
            "argument 1 of toNumber(...) must be a number or text holding one",
        ),
        (
            "toBool('yes')",
            "toBool(...): \"yes\" is neither \"true\" nor \"false\"",
        ),
        (
            "toBool(1)",
            "argument 1 of toBool(...) must be true, false, or the text of one",
        ),
        ("fromJson('{')", "fromJson(...):"),
        ("fromJson(1)", "argument 1 of fromJson(...) must be text"),
    ]);
}

#[test]
fn encoding() {
    all_equal(&[
        ("base64Encode('hello')", "\"aGVsbG8=\""),
        ("base64Encode('')", "\"\""),
        ("base64Decode('aGVsbG8=')", "\"hello\""),
        ("base64Decode(base64Encode('ünï'))", "\"ünï\""),
        ("urlEncode('a b&c=d/é')", "\"a%20b%26c%3Dd%2F%C3%A9\""),
        ("urlEncode('safe-_.~09azAZ')", "\"safe-_.~09azAZ\""),
        (
            "sha256('abc')",
            "\"ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad\"",
        ),
        (
            "sha256('')",
            "\"e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855\"",
        ),
        ("substr(sha256(data.env), 0, 8)", "\"6754af96\""),
    ]);
    all_fail(&[
        (
            "base64Decode('not base64!')",
            "base64Decode(...): the text is not base64",
        ),
        (
            "base64Decode('/w==')",
            "base64Decode(...): the decoded bytes are not text",
        ),
        (
            "base64Encode(1)",
            "argument 1 of base64Encode(...) must be text",
        ),
        ("sha256([1])", "argument 1 of sha256(...) must be text"),
    ]);
}

#[test]
fn networks() {
    all_equal(&[
        ("cidrHost('10.0.0.0/24', 5)", "\"10.0.0.5\""),
        ("cidrHost('10.0.0.0/24', 0)", "\"10.0.0.0\""),
        ("cidrHost('10.0.0.0/24', 255)", "\"10.0.0.255\""),
        ("cidrHost('10.0.0.0/24', -1)", "\"10.0.0.255\""),
        ("cidrHost('10.0.0.0/24', -2)", "\"10.0.0.254\""),
        // The address need not be the network's own.
        ("cidrHost('10.0.0.77/24', 1)", "\"10.0.0.1\""),
        ("cidrHost('10.1.2.3/32', 0)", "\"10.1.2.3\""),
        ("cidrHost('fd00::/64', 1)", "\"fd00::1\""),
        ("cidrSubnet('10.0.0.0/16', 8, 3)", "\"10.0.3.0/24\""),
        ("cidrSubnet('10.0.0.0/16', 0, 0)", "\"10.0.0.0/16\""),
        ("cidrSubnet('10.0.0.0/16', 4, 15)", "\"10.0.240.0/20\""),
        ("cidrSubnet('192.168.0.0/24', 2, 1)", "\"192.168.0.64/26\""),
        ("cidrSubnet('fd00::/48', 16, 10)", "\"fd00:0:0:a::/64\""),
        (
            "map(range(3), 'i', cidrSubnet('10.0.0.0/16', 8, i))",
            "[\"10.0.0.0/24\", \"10.0.1.0/24\", \"10.0.2.0/24\"]",
        ),
        ("cidrNetmask('10.0.0.0/16')", "\"255.255.0.0\""),
        ("cidrNetmask('10.0.0.0/0')", "\"0.0.0.0\""),
        ("cidrNetmask('10.0.0.0/32')", "\"255.255.255.255\""),
        ("cidrNetmask('10.0.0.0/20')", "\"255.255.240.0\""),
        ("cidrContains('10.0.0.0/16', '10.0.200.3')", "true"),
        ("cidrContains('10.0.0.0/16', '10.1.0.1')", "false"),
        ("cidrContains('0.0.0.0/0', '8.8.8.8')", "true"),
        ("cidrContains('fd00::/8', 'fd12::1')", "true"),
        ("cidrContains('10.0.0.0/8', 'fd12::1')", "false"),
    ]);
    all_fail(&[
        (
            "cidrHost('10.0.0.0', 1)",
            "\"10.0.0.0\" is not a network in CIDR form, such as 10.0.0.0/16",
        ),
        (
            "cidrHost('10.0.0.0/33', 1)",
            "is not a network in CIDR form",
        ),
        ("cidrHost('banana/8', 1)", "is not a network in CIDR form"),
        (
            "cidrHost('10.0.0.0/24', 256)",
            "cidrHost(...): 10.0.0.0/24 has no host number 256",
        ),
        ("cidrHost('10.0.0.0/24', -257)", "has no host number -257"),
        (
            "cidrHost('10.0.0.0/24', 'a')",
            "argument 2 of cidrHost(...) must be a whole number",
        ),
        (
            "cidrSubnet('10.0.0.0/16', 20, 0)",
            "cidrSubnet(...): 10.0.0.0/16 cannot be divided by 20 more bits",
        ),
        (
            "cidrSubnet('10.0.0.0/16', -1, 0)",
            "cannot be divided by -1 more bits",
        ),
        (
            "cidrSubnet('10.0.0.0/16', 9223372036854775807, 0)",
            "cannot be divided by",
        ),
        (
            "cidrSubnet('10.0.0.0/16', 2, 4)",
            "cidrSubnet(...): 2 more bits give subnets 0 to 3, not 4",
        ),
        (
            "cidrSubnet('10.0.0.0/16', 2, -1)",
            "give subnets 0 to 3, not -1",
        ),
        (
            "cidrNetmask('fd00::/8')",
            "cidrNetmask(...) is for IPv4 networks",
        ),
        (
            "cidrContains('10.0.0.0/8', 'nope')",
            "cidrContains(...): \"nope\" is not an address",
        ),
    ]);
}

#[test]
fn versions() {
    all_equal(&[
        ("semverCompare('1.2.3', '1.2.3')", "0"),
        ("semverCompare('1.2.3', '1.10.0')", "-1"),
        ("semverCompare('2.0.0', '1.99.99')", "1"),
        ("semverCompare('v1.2', '1.2.0')", "0"),
        ("semverCompare('1', '1.0.0')", "0"),
        ("semverCompare('1.0.0-rc1', '1.0.0')", "-1"),
        ("semverCompare('1.0.0', '1.0.0-rc1')", "1"),
        ("semverCompare('1.0.0+build5', '1.0.0')", "0"),
        ("semverSatisfies('1.6.2', '>=1.6')", "true"),
        ("semverSatisfies('1.6.2', '>=1.6 <2')", "true"),
        ("semverSatisfies('2.0.0', '>=1.6 <2')", "false"),
        ("semverSatisfies('1.5.9', '>=1.6 <2')", "false"),
        ("semverSatisfies('1.6.0', '1.6')", "true"),
        ("semverSatisfies('1.6.1', '=1.6')", "false"),
        ("semverSatisfies('1.6.1', '!=1.6')", "true"),
        ("semverSatisfies('1.6.1', '>1.6.1')", "false"),
        ("semverSatisfies('1.6.1', '<=1.6.1')", "true"),
        ("semverSatisfies('1.6.1', '')", "true"),
    ]);
    all_fail(&[
        (
            "semverCompare('one', '1.0')",
            "\"one\" is not a version, such as 1.4.2",
        ),
        ("semverCompare('1.2.3.4', '1.0')", "is not a version"),
        ("semverCompare('', '1.0')", "is not a version"),
        ("semverSatisfies('1.0', '>=x')", "\"x\" is not a version"),
        (
            "semverCompare(1, '1.0')",
            "argument 1 of semverCompare(...) must be text",
        ),
    ]);
}

#[test]
fn paths_are_worked_on_as_text() {
    all_equal(&[
        ("basename('/etc/nginx/nginx.conf')", "\"nginx.conf\""),
        ("basename('nginx.conf')", "\"nginx.conf\""),
        ("basename('/etc/nginx/')", "\"nginx\""),
        ("dirname('/etc/nginx/nginx.conf')", "\"/etc/nginx\""),
        ("dirname('nginx.conf')", "\".\""),
        ("dirname('/nginx.conf')", "\"/\""),
        ("dirname('/etc')", "\"/\""),
        ("dirname('/etc/')", "\"/\""),
        ("dirname('/')", "\"/\""),
        ("dirname('//')", "\"/\""),
        ("dirname('')", "\".\""),
        (
            "joinPath('/etc', 'nginx', 'nginx.conf')",
            "\"/etc/nginx/nginx.conf\"",
        ),
        ("joinPath('/etc/', '/nginx')", "\"/etc/nginx\""),
        ("joinPath('a')", "\"a\""),
        ("joinPath('a', '', 'b')", "\"a/b\""),
        ("joinPath('/srv', data.env, 'www')", "\"/srv/prod/www\""),
    ]);
    all_fail(&[(
        "joinPath('a', 1)",
        "argument 2 of joinPath(...) must be text",
    )]);
}

#[test]
fn the_environment_is_read_through_the_scope() {
    all_equal(&[
        ("env('HOME')", "\"/root\""),
        ("env('HOME', '/tmp')", "\"/root\""),
        ("env('EMPTY', 'x')", "\"\""),
        (
            "env('MERIDIAN_NO_SUCH_VARIABLE', 'fallback')",
            "\"fallback\"",
        ),
        ("env('MERIDIAN_NO_SUCH_VARIABLE', null)", "null"),
    ]);
    all_fail(&[
        (
            "env('MERIDIAN_NO_SUCH_VARIABLE')",
            "the environment variable MERIDIAN_NO_SUCH_VARIABLE is not set (give env(...) a second argument to fall back on)",
        ),
        ("env(1)", "argument 1 of env(...) must be text"),
    ]);
}

#[test]
fn a_selector_is_the_same_language_as_the_command_line() {
    all_equal(&[
        ("matchesSelector(data.labels, 'tier=web')", "true"),
        ("matchesSelector(data.labels, 'tier==web')", "true"),
        ("matchesSelector(data.labels, 'tier=db')", "false"),
        ("matchesSelector(data.labels, 'tier!=db')", "true"),
        ("matchesSelector(data.labels, 'tier!=web')", "false"),
        ("matchesSelector(data.labels, 'zone!=a')", "true"),
        ("matchesSelector(data.labels, 'tier')", "true"),
        ("matchesSelector(data.labels, 'zone')", "false"),
        ("matchesSelector(data.labels, '!zone')", "true"),
        ("matchesSelector(data.labels, '!tier')", "false"),
        ("matchesSelector(data.labels, 'tier=web,env=prod')", "true"),
        ("matchesSelector(data.labels, 'tier=web, env=dev')", "false"),
        ("matchesSelector({}, '!anything')", "true"),
        ("matchesSelector({n: 3}, 'n=3')", "true"),
    ]);
    all_fail(&[
        (
            "matchesSelector(data.labels, '')",
            "has an empty requirement",
        ),
        (
            "matchesSelector(data.labels, 'a=b,,c')",
            "has an empty requirement",
        ),
        (
            "matchesSelector(data.labels, '=web')",
            "\"=web\" is not a selector requirement",
        ),
        (
            "matchesSelector(data.labels, '!')",
            "is not a selector requirement",
        ),
        (
            "matchesSelector('x', 'a')",
            "argument 1 of matchesSelector(...) must be a mapping",
        ),
    ]);
}

/// Every function the library lists is exercised by a test in this file, so a
/// function cannot be added without one.
#[test]
fn every_function_is_tested() {
    let source = include_str!("functions.rs");
    let untested: Vec<&str> = function_names()
        .into_iter()
        .filter(|name| !source.contains(&format!("{name}(")))
        .collect();
    assert!(untested.is_empty(), "no test calls: {untested:?}");
    let names = function_names();
    let unique: BTreeSet<&&str> = names.iter().collect();
    assert_eq!(unique.len(), names.len(), "a function is listed twice");
}

/// A function given nothing it understands fails; it never panics.
#[test]
fn no_function_panics_on_arguments_of_the_wrong_type() {
    let samples = [
        "null",
        "true",
        "0",
        "-1",
        "1.5",
        "''",
        "'a'",
        "[]",
        "[1, 'a', null]",
        "{}",
        "{a: 1}",
        "9223372036854775807",
    ];
    for name in function_names() {
        if matches!(name, "map" | "filter" | "defined") {
            continue;
        }
        for a in samples {
            for b in samples {
                for call in [
                    format!("{name}({a})"),
                    format!("{name}({a}, {b})"),
                    format!("{name}({a}, {b}, {a})"),
                ] {
                    // Either outcome is fine; reaching the next line is the test.
                    let _ = eval(&call);
                }
            }
        }
    }
}

/// The reference page lists every function, so the documentation cannot fall
/// behind the library.
#[test]
fn every_function_is_in_the_reference() {
    let page = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../docs/reference/dsl/expressions.md"
    );
    let text = std::fs::read_to_string(page).expect("the reference page is there");
    let missing: Vec<&str> = function_names()
        .into_iter()
        .filter(|name| !text.contains(&format!("`{name}(")))
        .collect();
    assert!(
        missing.is_empty(),
        "docs/reference/dsl/expressions.md does not document: {missing:?}"
    );
}
