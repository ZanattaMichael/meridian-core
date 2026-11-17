//! Interpolation: what is an interpolation and what is passed through, what
//! a placed value becomes, and every way one can be wrong.

use std::collections::BTreeMap;

use meridian_expr::{has_interpolation, Names, Scope, Template};
use meridian_value::Value;

fn text(s: &str) -> Value {
    Value::String(s.to_owned())
}

fn map(entries: &[(&str, Value)]) -> Value {
    Value::Map(
        entries
            .iter()
            .map(|(k, v)| ((*k).to_owned(), v.clone()))
            .collect(),
    )
}

fn scope() -> Scope {
    Scope::new().with(
        "data",
        map(&[
            ("env", text("prod")),
            ("replicas", Value::Int(3)),
            ("ratio", Value::Float(0.5)),
            ("tls", Value::Bool(true)),
            ("ports", Value::List(vec![Value::Int(80), Value::Int(443)])),
            ("site", map(&[("name", text("shop"))])),
            ("nothing", Value::Null),
            ("shell", text("${HOME}/bin")),
            ("brace", text("a}}b")),
        ]),
    )
}

fn eval(source: &str) -> Result<Value, String> {
    Template::parse(source)?.eval(&scope())
}

fn literal(source: &str) -> Result<Value, String> {
    Template::parse(source)?.eval_literal(&scope())
}

#[test]
fn text_without_an_interpolation_is_left_byte_for_byte() {
    for source in [
        "",
        "plain",
        "${web.public_ip}",
        "$${HOME}",
        "{{ ansible_hostname }}",
        "%{ if x }y%{ endif }",
        "a } b }} c",
        "$ {{ x }}",
        "${ { x } }",
        "price: $5",
    ] {
        assert!(!has_interpolation(source), "{source}");
        let template = Template::parse(source).unwrap();
        assert!(template.is_literal(), "{source}");
        assert_eq!(template.eval(&scope()).unwrap(), text(source), "{source}");
        assert_eq!(
            template.eval_literal(&scope()).unwrap(),
            text(source),
            "{source}"
        );
    }
}

#[test]
fn an_escaped_opening_is_not_an_interpolation_and_is_left_as_written() {
    // The plugin's own rule for an escaped reference turns it into the
    // literal, so the core must not touch it.
    assert!(!has_interpolation("$${{ data.env }}"));
    assert_eq!(eval("$${{ data.env }}").unwrap(), text("$${{ data.env }}"));
    // An escaped one beside a real one.
    assert!(has_interpolation("$${{ a }} ${{ data.env }}"));
    assert_eq!(
        eval("$${{ a }} ${{ data.env }}").unwrap(),
        text("$${{ a }} prod")
    );
}

#[test]
fn a_value_that_is_only_an_interpolation_keeps_its_type() {
    assert_eq!(eval("${{ data.replicas }}").unwrap(), Value::Int(3));
    assert_eq!(eval("${{data.replicas}}").unwrap(), Value::Int(3));
    assert_eq!(eval("${{ data.ratio }}").unwrap(), Value::Float(0.5));
    assert_eq!(eval("${{ data.tls }}").unwrap(), Value::Bool(true));
    assert_eq!(eval("${{ data.env }}").unwrap(), text("prod"));
    assert_eq!(
        eval("${{ data.ports }}").unwrap(),
        Value::List(vec![Value::Int(80), Value::Int(443)])
    );
    assert_eq!(
        eval("${{ data.site }}").unwrap(),
        map(&[("name", text("shop"))])
    );
    assert_eq!(eval("${{ data.nothing }}").unwrap(), Value::Null);
    assert_eq!(
        eval("${{ data.tls ? [1, 2] : [] }}").unwrap(),
        Value::List(vec![Value::Int(1), Value::Int(2)])
    );
}

#[test]
fn surrounding_space_makes_it_text() {
    // Only a value that is nothing but the interpolation keeps its type.
    assert_eq!(eval(" ${{ data.replicas }}").unwrap(), text(" 3"));
    assert_eq!(eval("${{ data.replicas }} ").unwrap(), text("3 "));
}

#[test]
fn an_interpolation_inside_text_is_placed_as_text() {
    assert_eq!(
        eval("${{ data.site.name }}-${{ data.env }}").unwrap(),
        text("shop-prod")
    );
    assert_eq!(
        eval("replicas=${{ data.replicas }} tls=${{ data.tls }} ratio=${{ data.ratio }}").unwrap(),
        text("replicas=3 tls=true ratio=0.5")
    );
    assert_eq!(
        eval("ports: ${{ join(data.ports, ',') }}").unwrap(),
        text("ports: 80,443")
    );
    assert_eq!(
        eval("${{ upper(data.env) }}/${{ data.replicas * 2 }}").unwrap(),
        text("PROD/6")
    );
}

#[test]
fn what_cannot_be_text_is_refused_inside_text() {
    let err = eval("ports: ${{ data.ports }}").unwrap_err();
    assert!(
        err.contains("cannot be placed inside text") && err.contains("join(...)"),
        "{err}"
    );
    assert!(err.contains("${{ data.ports }}"), "{err}");
    let err = eval("site: ${{ data.site }}").unwrap_err();
    assert!(err.contains("cannot be placed inside text"), "{err}");
    let err = eval("x${{ data.nothing }}").unwrap_err();
    assert!(err.contains("null") && err.contains("??"), "{err}");
    assert_eq!(eval("x${{ data.nothing ?? 'd' }}").unwrap(), text("xd"));
}

#[test]
fn the_end_of_an_expression_is_found_past_quotes_and_brackets() {
    assert_eq!(
        eval("${{ {a: {b: 1}}.a.b }}").unwrap(),
        Value::Int(1),
        "a mapping literal ends in two braces of its own"
    );
    assert_eq!(
        eval("${{ {a: {b: 1}} }}").unwrap(),
        map(&[("a", map(&[("b", Value::Int(1))]))])
    );
    assert_eq!(eval("${{ '}}' }}").unwrap(), text("}}"));
    assert_eq!(eval("${{ \"}}\" }}!").unwrap(), text("}}!"));
    assert_eq!(eval("${{ 'it\\'s }}' }}").unwrap(), text("it's }}"));
    assert_eq!(eval("${{ data.brace }}").unwrap(), text("a}}b"));
    assert_eq!(eval("${{ [1, [2]][1][0] }}").unwrap(), Value::Int(2));
    assert_eq!(
        eval("a ${{ 1 }} b ${{ 2 }} c }} d").unwrap(),
        text("a 1 b 2 c }} d")
    );
}

#[test]
fn an_interpolation_that_cannot_be_read_says_why() {
    let err = Template::parse("a ${{ data.env").unwrap_err();
    assert!(err.contains("never closed"), "{err}");
    let err = Template::parse("a ${{ data.env } b").unwrap_err();
    assert!(err.contains("never closed"), "{err}");
    let err = Template::parse("${{ }}").unwrap_err();
    assert!(err.contains("empty interpolation"), "{err}");
    let err = Template::parse("${{}}").unwrap_err();
    assert!(err.contains("empty interpolation"), "{err}");
    let err = Template::parse("x ${{ data.env == }} y").unwrap_err();
    assert!(err.starts_with("in ${{ data.env == }}: "), "{err}");
    let err = Template::parse("${{ nope( }}").unwrap_err();
    assert!(err.starts_with("in ${{ nope( }}: "), "{err}");
}

#[test]
fn names_and_functions_are_checked_without_data() {
    let names = Names::new().allow("data").hint("reads data.<key>");
    let ok = Template::parse("${{ data.a }}-${{ upper(data.b) }}").unwrap();
    assert!(ok.check(&names).is_ok());
    let err = Template::parse("a-${{ param.x }}")
        .unwrap()
        .check(&names)
        .unwrap_err();
    assert!(
        err.contains("in ${{ param.x }}") && err.contains("unknown name \"param\""),
        "{err}"
    );
    let err = Template::parse("${{ uper(data.a) }}")
        .unwrap()
        .check(&names)
        .unwrap_err();
    assert!(err.contains("unknown function \"uper\""), "{err}");
    // Literal text is never checked as anything.
    assert!(Template::parse("{{ nope }} ${nope.x}")
        .unwrap()
        .check(&names)
        .is_ok());
}

#[test]
fn a_failure_names_the_interpolation_it_came_from() {
    let err = eval("a ${{ data.env }} b ${{ data.missing }}").unwrap_err();
    assert!(err.starts_with("in ${{ data.missing }}: "), "{err}");
    assert!(err.contains("data.missing is not set"), "{err}");
    let err = eval("${{ data.env + 1 }}").unwrap_err();
    assert!(err.starts_with("in ${{ data.env + 1 }}: "), "{err}");
}

#[test]
fn reads_reports_each_root_with_the_key_written_after_it() {
    let reads = Template::parse(
        "${{ var.name }} ${{ cond['is_prod'] ? data.a.b : var[data.k] }} ${{ map(data.l, 'var', var) }}",
    )
    .unwrap()
    .reads();
    let want: Vec<(String, Option<String>)> = [
        ("cond", Some("is_prod")),
        ("data", Some("a")),
        ("data", Some("k")),
        ("data", Some("l")),
        ("var", None),
        ("var", Some("name")),
    ]
    .into_iter()
    .map(|(root, key)| (root.to_owned(), key.map(str::to_owned)))
    .collect();
    assert_eq!(reads.into_iter().collect::<Vec<_>>(), want);
    assert!(Template::parse("no interpolation")
        .unwrap()
        .reads()
        .is_empty());
}

#[test]
fn a_placed_value_can_be_made_literal_so_data_never_becomes_a_reference() {
    // What the author wrote stays a reference; what data supplied does not.
    assert_eq!(
        literal("${web.ip} ${{ data.shell }}").unwrap(),
        text("${web.ip} $${HOME}/bin")
    );
    assert_eq!(literal("${{ data.shell }}").unwrap(), text("$${HOME}/bin"));
    // Without it, the value is placed exactly as it is.
    assert_eq!(eval("${{ data.shell }}").unwrap(), text("${HOME}/bin"));
    // However deep the value is.
    let scope = Scope::new().with(
        "data",
        map(&[(
            "deep",
            Value::List(vec![map(&[("cmd", text("echo ${X}"))]), Value::Int(1)]),
        )]),
    );
    assert_eq!(
        Template::parse("${{ data.deep }}")
            .unwrap()
            .eval_literal(&scope)
            .unwrap(),
        Value::List(vec![map(&[("cmd", text("echo $${X}"))]), Value::Int(1)])
    );
    // Types other than text are untouched.
    assert_eq!(literal("${{ data.replicas }}").unwrap(), Value::Int(3));
    assert_eq!(literal("n=${{ data.replicas }}").unwrap(), text("n=3"));
}

#[test]
fn a_placed_value_cannot_join_with_its_neighbour_into_a_reference() {
    let scope = Scope::new().with(
        "data",
        map(&[
            ("dollar", text("$")),
            ("price", text("cost $")),
            ("open", text("{web.id}")),
            ("two", text("$$")),
            ("empty", text("")),
            ("n", Value::Int(5)),
        ]),
    );
    let literal = |source: &str| {
        Template::parse(source)
            .unwrap()
            .eval_literal(&scope)
            .unwrap()
    };
    // A value that ends in a dollar, before text that opens a brace.
    assert_eq!(literal("${{ data.dollar }}{web.id}"), text("$${web.id}"));
    assert_eq!(
        literal("${{ data.price }}{web.id}"),
        text("cost $${web.id}")
    );
    // Text cannot end in a dollar right before an interpolation: that is
    // the escaped opening, which is left as written.
    assert_eq!(literal("x$${{ data.open }}"), text("x$${{ data.open }}"));
    // Two values, neither of which holds a reference alone.
    assert_eq!(
        literal("${{ data.dollar }}${{ data.open }}"),
        text("$${web.id}")
    );
    // The same answer as making the joined text literal in one go.
    assert_eq!(literal("${{ data.two }}{a}"), text("$$${a}"));
    // An empty value between a dollar and a brace the author wrote is the
    // author's own reference: data supplied nothing of it.
    assert_eq!(literal("${{ data.empty }}${web.id}"), text("${web.id}"));
    // A value that ends in a dollar directly before a reference the author
    // wrote would make that reference literal text. Nothing spells "a
    // dollar, then a reference", so it is refused and not handed over.
    for source in [
        "${{ data.dollar }}${web.id}",
        "${{ data.price }}${web.id} each",
        "${{ data.two }}${web.id}",
        "a ${{ data.dollar }}${{ data.empty }}${web.id}",
    ] {
        let refused = Template::parse(source)
            .unwrap()
            .eval_literal(&scope)
            .expect_err(source);
        assert!(
            refused.contains("${web.id}") && refused.contains("the reference would be lost"),
            "{source}: {refused}"
        );
    }
    // Anything between the two keeps both.
    assert_eq!(literal("${{ data.dollar }} ${web.id}"), text("$ ${web.id}"));
    // A literal the author escaped is not a reference to lose.
    assert_eq!(literal("${{ data.dollar }}$${HOME}"), text("$$${HOME}"));
    // Where no plugin reads the result, there is no reference to keep.
    assert_eq!(
        Template::parse("${{ data.dollar }}${web.id}")
            .unwrap()
            .eval(&scope)
            .unwrap(),
        text("$${web.id}")
    );
    // A dollar or a brace that joins nothing is left alone.
    assert_eq!(literal("${{ data.dollar }} {a}"), text("$ {a}"));
    assert_eq!(literal("${{ data.dollar }}${{ data.n }}"), text("$5"));
    assert_eq!(literal("{${{ data.dollar }}}"), text("{$}"));
    // Placed as it is, when literal placement is not asked for.
    assert_eq!(
        Template::parse("${{ data.dollar }}{web.id}")
            .unwrap()
            .eval(&scope)
            .unwrap(),
        text("${web.id}")
    );
}

#[test]
fn a_placed_value_cannot_open_an_expression_the_target_declared() {
    use meridian_value::PlacedEscape;
    // What Ansible declares: Jinja's three openings, anywhere in a value.
    let jinja = [
        PlacedEscape::anywhere("{{", "{{ '{{' }}"),
        PlacedEscape::anywhere("{%", "{{ '{%' }}"),
        PlacedEscape::anywhere("{#", "{{ '{#' }}"),
    ];
    // What ARM declares: `[` at the start of a value, written `[[`.
    let arm = [PlacedEscape::at_start("[", "[[")];
    let scope = Scope::new().with(
        "data",
        map(&[
            ("motd", text("hello {{ lookup('pipe','id') }}")),
            ("tag", text("{% raw %}x{# c #}")),
            ("expr", text("[resourceGroup().id]")),
            (
                "list",
                Value::List(vec![text("[listKeys(x)]"), text("plain")]),
            ),
            ("open", text("{")),
            ("close", text("{ x }}")),
            ("bracket", text("[")),
            ("shell", text("${HOME}/bin")),
        ]),
    );
    let with = |source: &str, rules: &[PlacedEscape]| {
        Template::parse(source)
            .unwrap()
            .eval_literal_with(&scope, rules)
            .unwrap()
    };

    // The whole value, and inside longer text.
    assert_eq!(
        with("${{ data.motd }}", &jinja),
        text("hello {{ '{{' }} lookup('pipe','id') }}")
    );
    assert_eq!(
        with("motd: ${{ data.motd }}!", &jinja),
        text("motd: hello {{ '{{' }} lookup('pipe','id') }}!")
    );
    // Each opening once: what `{%` is written as is not escaped again.
    assert_eq!(
        with("${{ data.tag }}", &jinja),
        text("{{ '{%' }} raw %}x{{ '{#' }} c #}")
    );
    // What the author wrote stays an expression.
    assert_eq!(
        with("{{ inventory_hostname }} ${{ data.open }}", &jinja),
        text("{{ inventory_hostname }} {")
    );
    // Two halves, one placed, cannot meet as an opening.
    assert_eq!(with("{${{ data.close }}", &jinja), text("{{ '{{' }} x }}"));
    assert_eq!(
        with("${{ data.open }}{ x }}", &jinja),
        text("{{ '{{' }} x }}")
    );

    // An opening that counts only at the start counts only there.
    assert_eq!(
        with("${{ data.expr }}", &arm),
        text("[[resourceGroup().id]")
    );
    assert_eq!(with("${{ data.bracket }}x]", &arm), text("[[x]"));
    assert_eq!(
        with("id: ${{ data.expr }}", &arm),
        text("id: [resourceGroup().id]")
    );
    assert_eq!(with("[${{ data.bracket }}]", &arm), text("[[]"));
    // However deep the value is: each string is a value of its own.
    assert_eq!(
        with("${{ data.list }}", &arm),
        Value::List(vec![text("[[listKeys(x)]"), text("plain")])
    );
    // Meridian's own escape still applies alongside.
    assert_eq!(with("${{ data.shell }}", &jinja), text("$${HOME}/bin"));
}

#[test]
fn sole_source_is_the_expression_when_the_text_is_nothing_else() {
    let sole = |s: &str| Template::parse(s).unwrap().sole_source().map(str::to_owned);
    assert_eq!(sole("${{ data.a == 1 }}"), Some("data.a == 1".to_owned()));
    assert_eq!(sole("x ${{ data.a }}"), None);
    assert_eq!(sole("${{ data.a }}${{ data.b }}"), None);
    assert_eq!(sole("plain"), None);
}

#[test]
fn the_same_text_over_the_same_scope_always_gives_the_same_value() {
    let scope = scope().with_env(BTreeMap::new());
    let template =
        Template::parse("${{ data.site.name }}-${{ sha256(data.env) }}-${{ data.ports[0] }}")
            .unwrap();
    let first = template.eval(&scope).unwrap();
    for _ in 0..20 {
        assert_eq!(template.eval(&scope).unwrap(), first);
    }
}
