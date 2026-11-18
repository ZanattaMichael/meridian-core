//! Every error kind the core crates produce has a fixed code, and every code
//! is on the reference page. A code that leaves the page or changes its word
//! fails here, which is the point: the codes are part of the interface.

use std::collections::BTreeSet;

fn page() -> String {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../docs/reference/errors.md"
    );
    std::fs::read_to_string(path).expect("docs/reference/errors.md is there")
}

fn all_codes() -> Vec<&'static str> {
    let mut codes: Vec<&'static str> = Vec::new();
    codes.extend(meridian_expr::Kind::ALL.iter().map(|k| k.code()));
    codes.extend(
        meridian_orchestrator::Reason::ALL
            .iter()
            .map(|r| r.as_str()),
    );
    codes.extend(meridian_module::Kind::ALL.iter().map(|k| k.code()));
    codes.extend(meridian_rules::Kind::ALL.iter().map(|k| k.code()));
    codes.extend(meridian_resolve::Kind::ALL.iter().map(|k| k.code()));
    codes.extend(
        meridian_plugin_host::signature::Kind::ALL
            .iter()
            .map(|k| k.code()),
    );
    codes
}

#[test]
fn every_error_code_is_in_the_reference() {
    let text = page();
    let missing: Vec<&str> = all_codes()
        .into_iter()
        .filter(|code| !text.contains(&format!("| `{code}` |")))
        .collect();
    assert!(
        missing.is_empty(),
        "docs/reference/errors.md does not list: {missing:?}"
    );
}

#[test]
fn the_reference_lists_no_code_the_crates_do_not_have() {
    // Every row of a code table on the page must be a code some crate has,
    // so the page cannot drift ahead of the code either.
    let known: BTreeSet<&str> = all_codes().into_iter().collect();
    let text = page();
    let listed: Vec<&str> = text
        .lines()
        .filter_map(|line| line.strip_prefix("| `"))
        .filter_map(|rest| rest.split_once("` |").map(|(code, _)| code))
        .filter(|code| code.contains('-'))
        .collect();
    let unknown: Vec<&str> = listed
        .iter()
        .copied()
        .filter(|code| !known.contains(code))
        .collect();
    assert!(
        unknown.is_empty(),
        "docs/reference/errors.md lists codes no crate has: {unknown:?}"
    );
}

#[test]
fn codes_are_distinct_fixed_words() {
    let codes = all_codes();
    let distinct: BTreeSet<&str> = codes.iter().copied().collect();
    assert_eq!(
        distinct.len(),
        codes.len(),
        "a code is used twice: {codes:?}"
    );
    for code in codes {
        assert!(
            code.chars().all(|c| c.is_ascii_lowercase() || c == '-'),
            "{code} is not a lowercase word"
        );
    }
}

#[test]
fn an_expression_error_says_where() {
    let err = meridian_expr::Expr::parse("1 + nope(2)")
        .unwrap()
        .check(&meridian_expr::Names::new())
        .unwrap_err();
    assert_eq!(err.kind, meridian_expr::Kind::UnknownFunction);
    let span = err.span.expect("the call has a span");
    assert_eq!(&"1 + nope(2)"[span.start..span.end], "nope");
    assert_eq!(String::from(err.clone()), err.to_string());
}
