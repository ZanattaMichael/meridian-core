//! Property tests for interpolation: whatever text the author writes around
//! an interpolation and whatever text the data supplies, what a plugin reads
//! afterwards is the author's text and references with the data's text
//! between them, as text.
//!
//! The model is a few lines: read each piece the author wrote the way a
//! plugin reads it, and put the data's values between the pieces as plain
//! text. The real implementation has to reach the same reading by escaping,
//! which is where the mistakes can be.

use meridian_expr::{Scope, Template};
use meridian_sdk::expr::{parse, Part};
use meridian_value::Value;
use proptest::collection::vec;
use proptest::prelude::*;
use proptest::test_runner::FileFailurePersistence;

fn cases(count: u32) -> ProptestConfig {
    ProptestConfig {
        cases: count,
        failure_persistence: Some(Box::new(FileFailurePersistence::Direct(
            "tests/placement_properties.proptest-regressions",
        ))),
        ..ProptestConfig::default()
    }
}

/// Text made of the characters that matter here, and a few that do not.
fn awkward() -> impl Strategy<Value = String> {
    vec(
        prop::sample::select(vec!["$", "{", "}", "a", ".", " ", "$$", "${", "}}", "b"]),
        0..7,
    )
    .prop_map(|parts| parts.concat())
}

/// One piece of a value: text the author wrote, or a value from data.
#[derive(Clone, Debug)]
enum Piece {
    Author(String),
    Data(String),
}

fn pieces() -> impl Strategy<Value = Vec<Piece>> {
    vec(
        prop_oneof![
            // The author's text is whole: every reference it opens it also
            // closes, and it opens no interpolation of its own.
            awkward()
                .prop_filter("the author's text can be read", |text| {
                    !text.contains("{{") && parse(text).is_ok()
                })
                .prop_map(Piece::Author),
            awkward().prop_map(Piece::Data),
        ],
        1..6,
    )
    .prop_map(|pieces| {
        // Two pieces of the author's text side by side are one piece.
        let mut merged: Vec<Piece> = Vec::new();
        for piece in pieces {
            match (merged.last_mut(), piece) {
                (Some(Piece::Author(last)), Piece::Author(next)) => last.push_str(&next),
                (_, piece) => merged.push(piece),
            }
        }
        merged
    })
    .prop_filter("the author's text can be read", |pieces| {
        pieces.iter().all(|piece| match piece {
            // An interpolation closes at the first `}}`, and text may not
            // end in a dollar before one: that is the escaped opening.
            Piece::Author(text) => {
                parse(text).is_ok() && !text.contains("{{") && !text.ends_with('$')
            }
            Piece::Data(_) => true,
        })
    })
}

/// The value as a document writes it, and the data it reads.
fn written(pieces: &[Piece]) -> (String, Scope) {
    let mut source = String::new();
    let mut data = std::collections::BTreeMap::new();
    for (index, piece) in pieces.iter().enumerate() {
        match piece {
            Piece::Author(text) => source.push_str(text),
            Piece::Data(text) => {
                source.push_str(&format!("{}{{ data.k{index} }}}}", "${"));
                data.insert(format!("k{index}"), Value::String(text.clone()));
            }
        }
    }
    (source, Scope::new().with("data", Value::Map(data)))
}

/// What a plugin reads, with neighbouring text joined.
fn reading(parts: Vec<Part>) -> Vec<Part> {
    let mut joined: Vec<Part> = Vec::new();
    for part in parts {
        match (joined.last_mut(), part) {
            (Some(Part::Text(last)), Part::Text(next)) => last.push_str(&next),
            (_, Part::Text(text)) if text.is_empty() => {}
            (_, part) => joined.push(part),
        }
    }
    joined
}

/// The model: the author's pieces read one by one, the data's as text.
fn model(pieces: &[Piece]) -> Vec<Part> {
    let mut parts = Vec::new();
    for piece in pieces {
        match piece {
            Piece::Author(text) => parts.extend(parse(text).unwrap()),
            Piece::Data(text) => parts.push(Part::Text(text.clone())),
        }
    }
    reading(parts)
}

/// Whether the one join that is refused occurs: a value from data that ends
/// in a dollar, directly before a reference the author wrote, with at most
/// empty values between them.
fn loses_a_reference(pieces: &[Piece]) -> bool {
    let filled: Vec<&Piece> = pieces
        .iter()
        .filter(|piece| !matches!(piece, Piece::Author(t) | Piece::Data(t) if t.is_empty()))
        .collect();
    filled.windows(2).any(|pair| {
        matches!(
            (pair[0], pair[1]),
            (Piece::Data(before), Piece::Author(after))
                if before.ends_with('$') && after.starts_with("${")
        )
    })
}

proptest! {
    #![proptest_config(cases(4000))]

    /// The heart of it: a plugin reads exactly the author's references, and
    /// every character the data supplied is text.
    #[test]
    fn a_plugin_reads_the_authors_references_and_the_datas_text(pieces in pieces()) {
        // A value that is one interpolation and nothing else keeps its type
        // and is covered below; here there is text to join.
        let (source, scope) = written(&pieces);
        let template = Template::parse(&source);
        prop_assert!(template.is_ok(), "{source:?}: {template:?}");
        let decided = template.unwrap().eval_literal(&scope);

        if loses_a_reference(&pieces) {
            let refused = decided.clone().err().map(String::from).unwrap_or_default();
            prop_assert!(
                refused.contains("the reference would be lost"),
                "{source:?} gave {decided:?}"
            );
            return Ok(());
        }
        let Ok(Value::String(given)) = decided else {
            return Err(TestCaseError::fail(format!("{source:?} gave {decided:?}")));
        };
        let read = parse(&given);
        prop_assert!(read.is_ok(), "{source:?} was given as {given:?}, which no plugin can read");
        prop_assert_eq!(
            reading(read.unwrap()),
            model(&pieces),
            "{:?} was given as {:?}",
            source,
            given
        );
    }

    /// Text from data alone, whatever it holds, is read back as that text
    /// and nothing else.
    #[test]
    fn a_value_from_data_alone_is_read_back_as_its_text(text in awkward()) {
        let scope = Scope::new().with(
            "data",
            Value::Map([("v".to_owned(), Value::String(text.clone()))].into()),
        );
        let source = format!("{}{{ data.v }}}}", "${");
        let given = Template::parse(&source).unwrap().eval_literal(&scope).unwrap();
        let Value::String(given) = given else {
            return Err(TestCaseError::fail(format!("{given:?}")));
        };
        let expected = if text.is_empty() { Vec::new() } else { vec![Part::Text(text.clone())] };
        prop_assert_eq!(reading(parse(&given).unwrap()), expected, "given {:?}", given);
    }

    /// Placement that no plugin reads (a label, a host, a variable) changes
    /// nothing: the pieces are joined as they are.
    #[test]
    fn placement_nobody_reads_joins_the_pieces_unchanged(pieces in pieces()) {
        let (source, scope) = written(&pieces);
        let decided = Template::parse(&source).unwrap().eval(&scope).unwrap();
        let plain: String = pieces
            .iter()
            .map(|piece| match piece {
                Piece::Author(text) | Piece::Data(text) => text.as_str(),
            })
            .collect();
        prop_assert_eq!(decided, Value::String(plain));
    }

    /// Deciding a value is a function of the document and the data: the
    /// same twice.
    #[test]
    fn deciding_a_value_twice_gives_the_same_answer(pieces in pieces()) {
        let (source, scope) = written(&pieces);
        let template = Template::parse(&source).unwrap();
        prop_assert_eq!(template.eval_literal(&scope), template.eval_literal(&scope));
    }
}
