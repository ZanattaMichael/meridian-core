//! The failure causes reference is generated from `meridian_sdk::failure`,
//! and this test fails when the page differs. Record it again with
//! `UPDATE_GOLDEN=1` after a cause is added on purpose.

use std::fmt::Write as _;
use std::fs;

use meridian_plugin_testkit::repo_root;
use meridian_sdk::failure::Cause;

const PATH: &str = "docs/reference/failure-causes.md";

const INTRODUCTION: &str = "\
# Failure causes

The word a [resource failure](state.md#resource-failures) carries for why the
resource failed. This page is generated: `crates/meridian-cli/tests/failure_causes.rs`
writes it from `meridian_sdk::failure::Cause` and fails when it is out of date.

A word never changes and is never reused. A plugin picks the word from its
tool's structured output; a word a newer plugin sends that this `meridian`
does not know reads as `unknown`.

| Cause | Meaning |
|---|---|
";

fn page() -> String {
    let mut out = INTRODUCTION.to_owned();
    for cause in Cause::ALL {
        let _ = writeln!(out, "| `{}` | {} |", cause.as_str(), cause.meaning());
    }
    out
}

#[test]
fn the_documented_causes_are_the_ones_the_sdk_declares() {
    let path = repo_root().join(PATH);
    let generated = page();
    if std::env::var_os("UPDATE_GOLDEN").is_some() {
        fs::write(&path, &generated).unwrap();
    }
    let documented = fs::read_to_string(&path).unwrap_or_default();
    assert!(
        documented == generated,
        "{PATH} is out of date with meridian_sdk::failure::Cause; run the tests with UPDATE_GOLDEN=1 to record it again"
    );
}
