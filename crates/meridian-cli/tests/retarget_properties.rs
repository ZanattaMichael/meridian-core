//! Property tests for `meridian retarget`'s rewrite: whatever identifier a
//! tool reports and however the document is laid out, the moved document
//! reads back with exactly that identifier as `adopt`, on exactly the
//! resources the tool manages, and with every other line as it was.

use std::collections::BTreeMap;

use meridian_ir::{Data, Document, Resolution, Retarget};
use proptest::collection::vec;
use proptest::prelude::*;
use proptest::test_runner::FileFailurePersistence;

fn cases(count: u32) -> ProptestConfig {
    ProptestConfig {
        cases: count,
        failure_persistence: Some(Box::new(FileFailurePersistence::Direct(
            "tests/retarget_properties.proptest-regressions",
        ))),
        ..ProptestConfig::default()
    }
}

/// An identifier as a tool might report one: mostly ordinary, with the
/// characters that matter to YAML and to Meridian's own `${`.
fn identifier() -> impl Strategy<Value = String> {
    prop_oneof![
        vec(
            prop::sample::select(vec![
                "a", "Z", "0", "-", "/", ":", " ", "#", "\"", "'", "\\", "$", "{", "}", "${",
                "$${", "${{", "ü", "🎉", "\t", "\u{7}", "\u{85}", "\u{feff}", "\u{2028}", "[", "]",
                ",", "&", "*", "!", "|", ">", "%", "@", "`",
            ]),
            1..12,
        )
        .prop_map(|parts| parts.concat()),
        "\\PC{1,30}",
    ]
}

/// How the document is laid out.
#[derive(Clone, Debug)]
struct Layout {
    indent: usize,
    crlf: bool,
    id_first: bool,
    comment: bool,
    quoted_target: bool,
    final_newline: bool,
}

fn layout() -> impl Strategy<Value = Layout> {
    (
        prop::sample::select(vec![2usize, 3, 4]),
        any::<bool>(),
        any::<bool>(),
        any::<bool>(),
        any::<bool>(),
        any::<bool>(),
    )
        .prop_map(
            |(indent, crlf, id_first, comment, quoted_target, final_newline)| Layout {
                indent,
                crlf,
                id_first,
                comment,
                quoted_target,
                final_newline,
            },
        )
}

/// A document of `count` resources laid out as `layout` says.
fn document(layout: &Layout, count: usize) -> String {
    let i = " ".repeat(layout.indent);
    let target = if layout.quoted_target {
        "\"terraform\""
    } else {
        "terraform"
    };
    let mut lines = vec![
        "apiVersion: meridian/v1".to_owned(),
        "kind: Infrastructure".to_owned(),
        "metadata:".to_owned(),
        format!("{i}name: net"),
        "spec:".to_owned(),
        format!("{i}target: {target} # the tool"),
        format!("{i}resources:"),
    ];
    for n in 0..count {
        if layout.comment {
            lines.push(format!("{i}{i}# resource {n}"));
        }
        let (first, second) = (format!("id: r{n}"), "type: aws_instance".to_owned());
        let (first, second) = if layout.id_first {
            (first, second)
        } else {
            (second, first)
        };
        lines.push(format!("{i}{i}- {first}"));
        lines.push(format!("{i}{i}  {second}"));
    }
    let eol = if layout.crlf { "\r\n" } else { "\n" };
    let mut text = lines.join(eol);
    if layout.final_newline {
        text.push_str(eol);
    }
    text
}

fn adopts(text: &str) -> BTreeMap<String, String> {
    let docs = meridian_ir::parse("net.yaml", text.as_bytes())
        .unwrap_or_else(|e| panic!("the moved document does not read: {e}\n{text}"));
    let Resolution::Kept { document, .. } =
        meridian_ir::resolve(&docs[0], &Data::new()).expect("it resolves")
    else {
        panic!("the document was skipped");
    };
    let Document::Infrastructure(d) = *document else {
        panic!("not Infrastructure");
    };
    assert_eq!(d.spec.target, "opentofu");
    d.spec
        .resources
        .iter()
        .filter(|r| !r.adopt.is_empty())
        .map(|r| (r.id.clone(), r.adopt.clone()))
        .collect()
}

proptest! {
    #![proptest_config(cases(256))]

    #[test]
    fn every_identifier_reads_back_exactly_on_its_own_resource(
        layout in layout(),
        reported in vec(prop::option::of(identifier()), 1..5),
    ) {
        let text = document(&layout, reported.len());
        let managed: BTreeMap<String, String> = reported
            .iter()
            .enumerate()
            .filter_map(|(n, id)| id.clone().map(|id| (format!("r{n}"), id)))
            .collect();
        let found = BTreeMap::from([("net".to_owned(), managed.clone())]);
        let out = meridian_ir::retarget(
            "net.yaml",
            &text,
            &Retarget { to: "opentofu", adopt: true, identifiers: &found },
        )
        .unwrap();

        // What a plugin is handed: the identifier, with `${` in the literal
        // form `$${` the language and the tools share.
        let expected: BTreeMap<String, String> = managed
            .iter()
            .map(|(k, v)| (k.clone(), v.replace("${", "$${")))
            .collect();
        prop_assert_eq!(adopts(&out.text), expected);
        prop_assert_eq!(out.moved[0].adopted.len(), managed.len());

        // Every line but the target and the new `adopt` lines is as it was.
        let kept: Vec<&str> = out
            .text
            .split_inclusive('\n')
            .filter(|line| !line.trim_start().starts_with("adopt: "))
            .collect();
        let original: Vec<&str> = text.split_inclusive('\n').collect();
        prop_assert_eq!(kept.len(), original.len());
        // (A last line with no line ending may have gained one, before the
        // `adopt` line after it.)
        for (now, before) in kept.iter().zip(&original) {
            let now = if before.ends_with('\n') {
                now.to_string()
            } else {
                now.trim_end_matches(['\r', '\n']).to_owned()
            };
            if before.contains("target:") {
                prop_assert_eq!(now.replace("opentofu", "terraform"), *before);
            } else {
                prop_assert_eq!(&now, before);
            }
        }
        // A CRLF file has no bare line feed.
        if layout.crlf {
            prop_assert!(!out.text.replace("\r\n", "").contains('\n'));
        }
    }
}
