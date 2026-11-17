//! Property tests for the core: the reader, the condition language, the
//! graph, references, selectors and the hierarchy resolver.
//!
//! An example-based test says "this input gives that output". These say what
//! must hold for *every* input, and let the machine look for one that breaks
//! it: thousands of generated documents, expressions and graphs per run. Three
//! kinds of property are used:
//!
//! - **nothing panics**: whatever bytes arrive, the answer is a value or an
//!   error, never a crash;
//! - **round trips**: what one half writes, the other half reads back;
//! - **a model agrees**: a second, deliberately naive implementation written
//!   here gives the same answer as the real one.
//!
//! A failure prints the smallest input that still fails, and proptest records
//! it in `properties.proptest-regressions` beside this file so that it is
//! tried first on every later run. Commit that file with the fix.

use std::collections::{BTreeMap, BTreeSet};

use meridian_graph::Graph;
use meridian_ir::{
    parse, parse_value, prune, Bundle, Condition, Dependency, Document, IfMissing, Notification,
    Resource, Value,
};
use meridian_orchestrator::Selector;
use meridian_resolve::{resolve, Hierarchy, KeyRule, MapLoader, Scope, Strategy as Merge};
use meridian_sdk::expr::{self, Part};
use meridian_yaml::Yaml;
use proptest::collection::{btree_map, vec};
use proptest::prelude::*;
use proptest::test_runner::FileFailurePersistence;

/// How many inputs a property is tried with, and where an input that broke
/// one is written down.
fn cases(count: u32) -> ProptestConfig {
    ProptestConfig {
        cases: count,
        failure_persistence: Some(Box::new(FileFailurePersistence::Direct(
            "tests/properties.proptest-regressions",
        ))),
        ..ProptestConfig::default()
    }
}

// ---------------------------------------------------------------------------
// Generators
// ---------------------------------------------------------------------------

/// A name as a document writes one: an id, a key, a label.
fn name() -> impl Strategy<Value = String> {
    "[a-z][a-z0-9_]{0,7}"
}

/// Text chosen to upset a reader or a writer: what YAML gives a meaning to,
/// what looks like another type, and whatever else Unicode holds.
fn awkward_text() -> impl Strategy<Value = String> {
    prop_oneof![
        4 => "[ -~]{0,24}",
        2 => "\\PC{0,16}",
        1 => "[a-z:#\\-\\[\\]{},&*!|>'\"%@` \\n\\t]{0,16}",
        1 => prop::sample::select(vec![
            "", " ", "~", "null", "Null", "true", "False", "yes", "no", "on", "off", "0644",
            "0x1F", "0o17", "1_000", "1e3", ".inf", "-.inf", ".nan", "2026-11-17", "- a", "a: b",
            "a #b", "#a", "|", ">", "? a", "[a]", "{a: b}", "&a", "*a", "!!str a", "%a", "@a",
            "`a`", "'a'", "\"a\"", "a\nb", "a\n", "\na", "a\tb", " a", "a ", "---", "...", "-",
            "${a.b}", "$${a}", "-1", "+1", "1.", ".5", "0", "-0", "9223372036854775808",
            "\u{feff}a", "\u{2028}", "a\r\nb", "\u{0}", "\u{7f}", "\u{85}",
        ])
        .prop_map(str::to_owned),
    ]
    // Text that opens an interpolation is an expression to be decided, not
    // text a document carries; `tests/interpolation.rs` covers it. Here the
    // opening is written the way a document writes a literal one.
    .prop_map(|text| text.replace(INTERPOLATION, LITERAL_INTERPOLATION))
}

const INTERPOLATION: &str = concat!("$", "{{");
const LITERAL_INTERPOLATION: &str = concat!("$$", "{{");

fn scalar() -> impl Strategy<Value = Value> {
    prop_oneof![
        Just(Value::Null),
        any::<bool>().prop_map(Value::Bool),
        any::<i64>().prop_map(Value::Int),
        (-1_000_000i64..1_000_000).prop_map(Value::Int),
        ((i64::MAX as u64 + 1)..=u64::MAX).prop_map(Value::Uint),
        // Floats with a fractional part that survives being written in
        // decimal: a whole-numbered float is read back as an integer, which
        // `same` below allows for.
        (-1_000_000i64..1_000_000, 1u32..1000).prop_map(|(whole, thousandths)| Value::Float(
            whole as f64 + f64::from(thousandths) / 1000.0
        )),
        any::<f64>()
            .prop_filter("finite", |f| f.is_finite())
            .prop_map(Value::Float),
        awkward_text().prop_map(Value::String),
    ]
}

/// Any value a document can carry under `params` or in a data file.
fn value() -> impl Strategy<Value = Value> {
    scalar().prop_recursive(4, 48, 6, |inner| {
        prop_oneof![
            vec(inner.clone(), 0..6).prop_map(Value::List),
            btree_map(prop_oneof![name(), awkward_text()], inner, 0..6).prop_map(Value::Map),
        ]
    })
}

fn mapping() -> impl Strategy<Value = BTreeMap<String, Value>> {
    btree_map(name(), value(), 0..6)
}

fn to_yaml(value: &Value) -> Yaml {
    match value {
        Value::Null => Yaml::Null,
        Value::Bool(b) => Yaml::Bool(*b),
        Value::Int(n) => Yaml::Int(*n),
        Value::Uint(n) => Yaml::Uint(*n),
        Value::Float(f) => Yaml::Float(*f),
        Value::String(s) => Yaml::Str(s.clone()),
        Value::List(items) => Yaml::Seq(items.iter().map(to_yaml).collect()),
        Value::Map(map) => Yaml::Map(map.iter().map(|(k, v)| (k.clone(), to_yaml(v))).collect()),
        // The strategies never make one; a document cannot write a secret's value.
        Value::Secret(_) => Yaml::Str(meridian_value::SENSITIVE.to_owned()),
    }
}

/// Equal, where a number is the same number however it was written: `1.0`
/// comes back from text as the integer `1`.
fn same(a: &Value, b: &Value) -> bool {
    fn number(v: &Value) -> Option<f64> {
        match v {
            Value::Int(n) => Some(*n as f64),
            Value::Uint(n) => Some(*n as f64),
            Value::Float(n) => Some(*n),
            _ => None,
        }
    }
    match (a, b) {
        (Value::List(x), Value::List(y)) => {
            x.len() == y.len() && x.iter().zip(y).all(|(p, q)| same(p, q))
        }
        (Value::Map(x), Value::Map(y)) => {
            x.len() == y.len() && x.iter().zip(y).all(|((k, p), (l, q))| k == l && same(p, q))
        }
        _ => match (number(a), number(b)) {
            (Some(x), Some(y)) => x == y,
            _ => a == b,
        },
    }
}

// ---------------------------------------------------------------------------
// Values: YAML and canonical JSON round trips
// ---------------------------------------------------------------------------

proptest! {
    #![proptest_config(cases(512))]

    /// What Meridian's YAML writer writes, Meridian's reader reads back as
    /// the same value. Every target that emits YAML (Ansible, Salt,
    /// cloud-init, Kubernetes, Compose, ...) leans on this: a param must
    /// reach the tool as the value the author wrote, whatever is in it.
    #[test]
    fn yaml_written_is_yaml_read(map in mapping()) {
        let original = Value::Map(map);
        let text = meridian_yaml::write(&to_yaml(&original));
        let read = parse_value("written.yaml", text.as_bytes())
            .map_err(|e| TestCaseError::fail(format!("the writer's output does not parse: {e}\n{text}")))?;
        prop_assert!(same(&original, &read), "wrote:\n{text}\nread back: {read:?}");
    }

    /// The same for one awkward string in each place a string can stand: a
    /// mapping value, a list item, and a key.
    #[test]
    fn an_awkward_string_survives_every_position(text in awkward_text()) {
        let original = Value::Map(BTreeMap::from([
            ("value".to_owned(), Value::String(text.clone())),
            ("list".to_owned(), Value::List(vec![Value::String(text.clone())])),
            ("nested".to_owned(), Value::Map(BTreeMap::from([(
                "deep".to_owned(),
                Value::List(vec![Value::Map(BTreeMap::from([(
                    "inner".to_owned(),
                    Value::String(text.clone()),
                )]))]),
            )]))),
        ]));
        let written = meridian_yaml::write(&to_yaml(&original));
        let read = parse_value("written.yaml", written.as_bytes())
            .map_err(|e| TestCaseError::fail(format!("{e}\n{written}")))?;
        prop_assert_eq!(&original, &read, "wrote:\n{}", written);
    }

    /// The canonical JSON a value crosses the plugin boundary in decodes to
    /// the same value, and two equal values always encode to the same bytes.
    #[test]
    fn canonical_json_round_trips(original in value()) {
        let encoded = original.canonical();
        let decoded = meridian_value::decode_value(encoded.as_bytes())
            .map_err(TestCaseError::fail)?;
        prop_assert!(same(&original, &decoded), "{encoded} decoded to {decoded:?}");
        prop_assert_eq!(&encoded, &original.clone().canonical());
        // Once through the wire, a value is a fixed point.
        prop_assert_eq!(&decoded.canonical(), &meridian_value::decode_value(decoded.canonical().as_bytes()).unwrap().canonical());
    }

    /// A value given as JSON and the same value given as YAML are one value.
    #[test]
    fn json_and_yaml_read_to_the_same_value(map in mapping()) {
        let original = Value::Map(map);
        let from_json = parse_value("data.json", original.canonical().as_bytes())
            .map_err(|e| TestCaseError::fail(e.to_string()))?;
        let yaml = meridian_yaml::write(&to_yaml(&original));
        let from_yaml = parse_value("data.yaml", yaml.as_bytes())
            .map_err(|e| TestCaseError::fail(e.to_string()))?;
        prop_assert!(same(&from_json, &from_yaml), "json {from_json:?}\nyaml {from_yaml:?}");
    }
}

// ---------------------------------------------------------------------------
// The reader never panics
// ---------------------------------------------------------------------------

const VALID: &str = "\
apiVersion: meridian/v1
kind: ResourceSet
metadata:
  name: web
  labels:
    tier: web
spec:
  target: ansible
  targetHost:
    from: inline
    host: web01.example.com
  resources:
    - id: pkg
      type: package
      params:
        name: nginx
    - id: conf
      type: file
      state: present
      params:
        path: /etc/nginx/nginx.conf
        content: \"worker_processes 4;\\n\"
        mode: \"0644\"
      when: \"data.env == 'prod'\"
      dependsOn:
        - resource: pkg
          ifMissing: skip
      notifies:
        - resource: svc
          action: restart
    - id: svc
      type: service
      state: running
      params: {name: nginx, enabled: true}
      runtimeWhen: \"1 == 1\"
---
apiVersion: meridian/v1
kind: Infrastructure
metadata:
  name: machine
spec:
  target: terraform
  resources:
    - id: web
      type: aws_instance
      params:
        tags: &tags {Name: web}
        more: *tags
  outputs:
    - name: address
      value: ${web.public_ip}
";

/// One edit to a document: cut bytes out, write bytes in, or double a slice.
#[derive(Clone, Debug)]
enum Edit {
    Delete { at: usize, len: usize },
    Insert { at: usize, bytes: Vec<u8> },
    Repeat { at: usize, len: usize },
}

fn edit() -> impl Strategy<Value = Edit> {
    let fragment = prop_oneof![
        vec(any::<u8>(), 1..6),
        prop::sample::select(vec![
            ": ",
            "- ",
            "\n",
            "  ",
            "\t",
            "#",
            "&a ",
            "*a",
            "<<: ",
            "{",
            "}",
            "[",
            "]",
            "\"",
            "'",
            "|\n",
            ">\n",
            "---\n",
            "...\n",
            "!!",
            "? ",
            ",",
            "\u{feff}",
            "\r\n",
            "${",
            "$${",
            "when: ",
            "dependsOn:",
            "id: pkg",
            "kind: Nope",
            "apiVersion: v2",
            "null",
            "~",
        ])
        .prop_map(|s| s.as_bytes().to_vec()),
    ];
    prop_oneof![
        (any::<usize>(), 1usize..40).prop_map(|(at, len)| Edit::Delete { at, len }),
        (any::<usize>(), fragment).prop_map(|(at, bytes)| Edit::Insert { at, bytes }),
        (any::<usize>(), 1usize..60).prop_map(|(at, len)| Edit::Repeat { at, len }),
    ]
}

fn apply(mut text: Vec<u8>, edits: &[Edit]) -> Vec<u8> {
    for edit in edits {
        if text.is_empty() {
            break;
        }
        match edit {
            Edit::Delete { at, len } => {
                let at = at % text.len();
                let end = (at + len).min(text.len());
                text.drain(at..end);
            }
            Edit::Insert { at, bytes } => {
                let at = at % (text.len() + 1);
                text.splice(at..at, bytes.iter().copied());
            }
            Edit::Repeat { at, len } => {
                let at = at % text.len();
                let end = (at + len).min(text.len());
                let slice = text[at..end].to_vec();
                text.splice(end..end, slice);
            }
        }
    }
    text
}

/// Whatever the reader says about some bytes, it says it without panicking,
/// and an error is one a person can act on: it has a message.
fn read_without_panicking(bytes: &[u8]) -> Result<(), TestCaseError> {
    match parse("fuzz.yaml", bytes) {
        Ok(documents) => {
            // What parsed can be validated and built without a panic too.
            let bundle = Bundle::new(documents.clone());
            if bundle.validate().is_ok() {
                for document in &documents {
                    let _ = Graph::from_document(document)
                        .and_then(|g| g.topo_sort().map(|order| order.len()));
                    let _ = prune(document.resources(), &BTreeMap::new());
                }
            }
        }
        Err(errors) => {
            prop_assert!(!errors.is_empty(), "an error list with nothing in it");
            for error in errors.iter() {
                prop_assert!(!error.to_string().is_empty());
            }
        }
    }
    let _ = parse_value("fuzz.yaml", bytes);
    Ok(())
}

proptest! {
    #![proptest_config(cases(1500))]

    #[test]
    fn the_reader_survives_arbitrary_bytes(bytes in vec(any::<u8>(), 0..400)) {
        read_without_panicking(&bytes)?;
    }

    #[test]
    fn the_reader_survives_text_made_of_yaml_and_json_punctuation(
        text in "[a-z0-9 \\n\\t:#\\-\\[\\]{},&*!|>'\"%@`?~.$<=\\\\]{0,300}"
    ) {
        read_without_panicking(text.as_bytes())?;
    }

    /// The most searching input is one that is nearly right: a real document
    /// with a few bytes cut, added or doubled.
    #[test]
    fn the_reader_survives_a_damaged_document(edits in vec(edit(), 1..6)) {
        read_without_panicking(&apply(VALID.as_bytes().to_vec(), &edits))?;
    }

    /// Deep nesting is refused or read; it does not overflow the stack.
    #[test]
    fn the_reader_survives_deep_nesting(depth in 1usize..3000, flow in any::<bool>()) {
        let text = if flow {
            format!("{}{}", "[".repeat(depth), "]".repeat(depth))
        } else {
            (0..depth).map(|i| format!("{}a:\n", " ".repeat(i))).collect()
        };
        read_without_panicking(text.as_bytes())?;
    }
}

#[test]
fn the_document_the_fuzzer_starts_from_is_valid() {
    let documents = parse("valid.yaml", VALID.as_bytes()).unwrap();
    assert_eq!(documents.len(), 2);
    Bundle::new(documents).validate().unwrap();
}

// ---------------------------------------------------------------------------
// Documents: YAML and JSON are one language
// ---------------------------------------------------------------------------

/// A generated document, as the plain facts a reader should find in it.
#[derive(Clone, Debug)]
struct Sketch {
    name: String,
    labels: BTreeMap<String, String>,
    resources: Vec<SketchResource>,
}

#[derive(Clone, Debug)]
struct SketchResource {
    r#type: String,
    state: String,
    params: BTreeMap<String, Value>,
    /// Indices of earlier resources, so the document has no cycle.
    depends_on: Vec<(usize, Option<bool>)>,
    notifies: Vec<(usize, String)>,
    runtime_when: String,
}

fn sketch() -> impl Strategy<Value = Sketch> {
    let resource = (
        name(),
        prop_oneof![Just(String::new()), name()],
        btree_map(name(), value(), 0..4),
        vec((any::<prop::sample::Index>(), any::<Option<bool>>()), 0..3),
        vec((any::<prop::sample::Index>(), name()), 0..2),
        prop_oneof![Just(String::new()), "[ -~]{1,12}"]
            .prop_map(|text| text.replace(INTERPOLATION, LITERAL_INTERPOLATION)),
    );
    (
        name(),
        btree_map(name(), "[a-z0-9]{0,6}", 0..3),
        vec(resource, 1..7),
    )
        .prop_map(|(name, labels, raw)| {
            let mut resources = Vec::new();
            for (i, (r#type, state, params, deps, notes, runtime_when)) in
                raw.into_iter().enumerate()
            {
                let earlier = |index: &prop::sample::Index| (i > 0).then(|| index.index(i));
                let mut depends_on: Vec<(usize, Option<bool>)> = deps
                    .iter()
                    .filter_map(|(index, skip)| earlier(index).map(|j| (j, *skip)))
                    .collect();
                depends_on.sort();
                depends_on.dedup_by_key(|(j, _)| *j);
                let notifies = notes
                    .iter()
                    .filter_map(|(index, action)| earlier(index).map(|j| (j, action.clone())))
                    .collect();
                resources.push(SketchResource {
                    r#type,
                    state,
                    params,
                    depends_on,
                    notifies,
                    // A runtimeWhen that is only spaces is no condition.
                    runtime_when: runtime_when.trim().to_owned(),
                });
            }
            Sketch {
                name,
                labels,
                resources,
            }
        })
}

impl Sketch {
    fn id(i: usize) -> String {
        format!("r{i}")
    }

    /// The document as a value, which either writer can then write.
    fn document(&self) -> Value {
        let text = |s: &str| Value::String(s.to_owned());
        let map = |pairs: Vec<(&str, Value)>| {
            Value::Map(pairs.into_iter().map(|(k, v)| (k.to_owned(), v)).collect())
        };
        let resources = self
            .resources
            .iter()
            .enumerate()
            .map(|(i, r)| {
                let mut fields = vec![("id", text(&Sketch::id(i))), ("type", text(&r.r#type))];
                if !r.state.is_empty() {
                    fields.push(("state", text(&r.state)));
                }
                if !r.params.is_empty() {
                    fields.push(("params", Value::Map(r.params.clone())));
                }
                if !r.depends_on.is_empty() {
                    let entries = r
                        .depends_on
                        .iter()
                        .map(|(j, skip)| {
                            let mut entry = vec![("resource", text(&Sketch::id(*j)))];
                            match skip {
                                Some(true) => entry.push(("ifMissing", text("skip"))),
                                Some(false) => entry.push(("ifMissing", text("error"))),
                                None => {}
                            }
                            map(entry)
                        })
                        .collect();
                    fields.push(("dependsOn", Value::List(entries)));
                }
                if !r.notifies.is_empty() {
                    let entries = r
                        .notifies
                        .iter()
                        .map(|(j, action)| {
                            map(vec![
                                ("resource", text(&Sketch::id(*j))),
                                ("action", text(action)),
                            ])
                        })
                        .collect();
                    fields.push(("notifies", Value::List(entries)));
                }
                if !r.runtime_when.is_empty() {
                    fields.push(("runtimeWhen", text(&r.runtime_when)));
                }
                map(fields)
            })
            .collect();
        let mut metadata = vec![("name", text(&self.name))];
        if !self.labels.is_empty() {
            metadata.push((
                "labels",
                Value::Map(
                    self.labels
                        .iter()
                        .map(|(k, v)| (k.clone(), text(v)))
                        .collect(),
                ),
            ));
        }
        map(vec![
            ("apiVersion", text("meridian/v1")),
            ("kind", text("ResourceSet")),
            ("metadata", map(metadata)),
            (
                "spec",
                map(vec![
                    ("target", text("ansible")),
                    (
                        "targetHost",
                        map(vec![("from", text("inline")), ("host", text("web01"))]),
                    ),
                    ("resources", Value::List(resources)),
                ]),
            ),
        ])
    }
}

/// A resource with everything that says where it was written taken off.
fn unplaced(resource: &Resource) -> Resource {
    let mut resource = resource.clone();
    resource.pos = Default::default();
    for dep in &mut resource.depends_on {
        dep.pos = Default::default();
    }
    for note in &mut resource.notifies {
        note.pos = Default::default();
    }
    resource
}

proptest! {
    #![proptest_config(cases(400))]

    /// A document reads the same whether it is given as YAML or as JSON, it
    /// holds what was put in it, and it validates and sorts: every resource
    /// after the ones it depends on, with ties in the order they were
    /// written.
    #[test]
    fn a_document_is_the_same_document_in_yaml_and_in_json(sketch in sketch()) {
        let document = sketch.document();
        let yaml = meridian_yaml::write(&to_yaml(&document));
        let json = document.canonical();

        let fail = |what: &str, e: &dyn std::fmt::Display, text: &str| {
            TestCaseError::fail(format!("{what}: {e}\n{text}"))
        };
        let from_yaml = parse("doc.yaml", yaml.as_bytes()).map_err(|e| fail("yaml", &e, &yaml))?;
        let from_json = parse("doc.json", json.as_bytes()).map_err(|e| fail("json", &e, &json))?;
        prop_assert_eq!(from_yaml.len(), 1);
        prop_assert_eq!(from_json.len(), 1);
        let (Document::ResourceSet(a), Document::ResourceSet(b)) = (&from_yaml[0], &from_json[0])
        else {
            return Err(TestCaseError::fail("not a ResourceSet"));
        };

        prop_assert_eq!(&a.metadata, &b.metadata);
        prop_assert_eq!(&a.metadata.name, &sketch.name);
        prop_assert_eq!(&a.metadata.labels, &sketch.labels);
        prop_assert_eq!(a.spec.resources.len(), sketch.resources.len());
        for (i, (x, y)) in a.spec.resources.iter().zip(&b.spec.resources).enumerate() {
            let want = &sketch.resources[i];
            // Numbers aside (JSON has one kind, YAML three), the two are equal.
            let mut x = unplaced(x);
            let mut y = unplaced(y);
            prop_assert!(same(&Value::Map(x.params.clone()), &Value::Map(y.params.clone())));
            prop_assert!(same(&Value::Map(x.params.clone()), &Value::Map(want.params.clone())),
                "resource {i}: wrote {:?}\nread {:?}\n{yaml}", want.params, x.params);
            x.params.clear();
            y.params.clear();
            prop_assert_eq!(&x, &y);
            prop_assert_eq!(&x.id, &Sketch::id(i));
            prop_assert_eq!(x.index, i);
            prop_assert_eq!(&x.r#type, &want.r#type);
            prop_assert_eq!(&x.state, &want.state);
            prop_assert_eq!(&x.runtime_when, &want.runtime_when);
            let deps: Vec<String> = x.depends_on.iter().map(|d| d.resource.clone()).collect();
            let want_deps: Vec<String> = want.depends_on.iter().map(|(j, _)| Sketch::id(*j)).collect();
            prop_assert_eq!(deps, want_deps);
        }

        Bundle::new(from_yaml.clone()).validate().map_err(|e| fail("validate", &e, &yaml))?;
        let graph = Graph::from_document(&from_yaml[0]).map_err(|e| fail("graph", &e, &yaml))?;
        let order = graph.topo_sort().map_err(|e| fail("sort", &e, &yaml))?;
        // Every dependency points backwards, so the order written is already
        // a valid one, and with ties broken by position it is the only answer.
        let written: Vec<String> = (0..sketch.resources.len()).map(Sketch::id).collect();
        prop_assert_eq!(order, written);
    }
}

// ---------------------------------------------------------------------------
// The graph
// ---------------------------------------------------------------------------

fn resource(id: usize, depends_on: &[usize]) -> Resource {
    Resource {
        id: format!("r{id}"),
        r#type: "exec".to_owned(),
        depends_on: depends_on
            .iter()
            .map(|d| Dependency {
                resource: format!("r{d}"),
                ..Dependency::default()
            })
            .collect(),
        ..Resource::default()
    }
}

/// Resources with no cycle among them, in an order that says nothing about
/// their dependencies: each has a hidden rank, and depends only on resources
/// of lower rank.
fn acyclic() -> impl Strategy<Value = Vec<Vec<usize>>> {
    (2usize..14)
        .prop_flat_map(|n| {
            (
                Just((0..n).collect::<Vec<usize>>()).prop_shuffle(),
                vec(vec(any::<prop::sample::Index>(), 0..4), n),
            )
        })
        .prop_map(|(rank, picks)| {
            let n = rank.len();
            (0..n)
                .map(|i| {
                    let lower: Vec<usize> = (0..n).filter(|j| rank[*j] < rank[i]).collect();
                    let mut deps: Vec<usize> = if lower.is_empty() {
                        Vec::new()
                    } else {
                        picks[i]
                            .iter()
                            .map(|p| lower[p.index(lower.len())])
                            .collect()
                    };
                    deps.sort_unstable();
                    deps.dedup();
                    deps
                })
                .collect()
        })
}

/// The sort as its documentation states it, written the slow way: again and
/// again, take the first resource in declaration order whose dependencies
/// have all been taken.
fn slow_sort(deps: &[Vec<usize>]) -> Vec<usize> {
    let mut order = Vec::new();
    let mut taken = vec![false; deps.len()];
    while order.len() < deps.len() {
        let next = (0..deps.len())
            .find(|&i| !taken[i] && deps[i].iter().all(|d| taken[*d]))
            .expect("no cycle");
        taken[next] = true;
        order.push(next);
    }
    order
}

proptest! {
    #![proptest_config(cases(600))]

    #[test]
    fn the_sort_puts_every_dependency_first_and_breaks_ties_by_position(deps in acyclic()) {
        let resources: Vec<Resource> =
            deps.iter().enumerate().map(|(i, d)| resource(i, d)).collect();
        let graph = Graph::build(&resources).unwrap();
        let order: Vec<String> = graph.topo_sort().unwrap().into_iter().map(str::to_owned).collect();

        prop_assert_eq!(order.len(), deps.len());
        let place: BTreeMap<&str, usize> =
            order.iter().enumerate().map(|(at, id)| (id.as_str(), at)).collect();
        prop_assert_eq!(place.len(), deps.len(), "an id appears twice");
        for (i, d) in deps.iter().enumerate() {
            for dep in d {
                let (before, after) = (format!("r{dep}"), format!("r{i}"));
                prop_assert!(place[before.as_str()] < place[after.as_str()]);
            }
        }
        let expected: Vec<String> = slow_sort(&deps).iter().map(|i| format!("r{i}")).collect();
        prop_assert_eq!(&order, &expected);

        // The same resources again give the same order, and so does the
        // graph asked twice.
        let again = Graph::build(&resources).unwrap();
        prop_assert_eq!(again.topo_sort().unwrap(), graph.topo_sort().unwrap());
        for (i, d) in deps.iter().enumerate() {
            let mut got: Vec<String> =
                graph.dependencies(&format!("r{i}")).into_iter().map(str::to_owned).collect();
            got.sort();
            let mut want: Vec<String> = d.iter().map(|j| format!("r{j}")).collect();
            want.sort();
            prop_assert_eq!(got, want);
        }
    }

    /// Close any path of dependencies back on itself and the sort refuses,
    /// naming a cycle that really is one: it ends where it starts and every
    /// step along it is a dependency the document declares.
    #[test]
    fn a_cycle_is_refused_and_the_cycle_named_is_real(
        deps in acyclic(),
        from in any::<prop::sample::Index>(),
    ) {
        let mut deps = deps;
        // Find a resource with a dependency, walk down to a resource with
        // none, and make that one depend on where the walk began.
        let with_deps: Vec<usize> = (0..deps.len()).filter(|i| !deps[*i].is_empty()).collect();
        prop_assume!(!with_deps.is_empty());
        let start = with_deps[from.index(with_deps.len())];
        let mut at = start;
        while let Some(&next) = deps[at].first() {
            at = next;
        }
        deps[at].push(start);

        let resources: Vec<Resource> =
            deps.iter().enumerate().map(|(i, d)| resource(i, d)).collect();
        let graph = Graph::build(&resources).unwrap();
        let error = graph.topo_sort().expect_err("the graph has a cycle");
        prop_assert!(error.cycle.len() >= 3, "{error}");
        prop_assert_eq!(error.cycle.first(), error.cycle.last());
        let declared: BTreeSet<(String, String)> = deps
            .iter()
            .enumerate()
            .flat_map(|(i, d)| d.iter().map(move |j| (format!("r{j}"), format!("r{i}"))))
            .collect();
        for step in error.cycle.windows(2) {
            let forwards = declared.contains(&(step[0].clone(), step[1].clone()));
            let backwards = declared.contains(&(step[1].clone(), step[0].clone()));
            prop_assert!(forwards || backwards, "{} -> {} is not a declared dependency: {error}", step[0], step[1]);
        }
    }

    /// Notifications order nothing: a loop of them is legal, and adding them
    /// leaves the order as it was.
    #[test]
    fn notifications_never_change_the_order(
        deps in acyclic(),
        notes in vec((any::<prop::sample::Index>(), any::<prop::sample::Index>()), 0..8),
    ) {
        let plain: Vec<Resource> =
            deps.iter().enumerate().map(|(i, d)| resource(i, d)).collect();
        let mut noisy = plain.clone();
        for (from, to) in &notes {
            let (from, to) = (from.index(deps.len()), to.index(deps.len()));
            if from != to {
                noisy[from].notifies.push(Notification {
                    resource: format!("r{to}"),
                    action: "restart".to_owned(),
                    ..Notification::default()
                });
            }
        }
        let a = Graph::build(&plain).unwrap();
        let b = Graph::build(&noisy).unwrap();
        prop_assert_eq!(a.topo_sort().unwrap(), b.topo_sort().unwrap());
    }
}

// ---------------------------------------------------------------------------
// Conditions
// ---------------------------------------------------------------------------

/// A condition, as this file understands the language: the model the real
/// parser and evaluator are compared with.
///
/// It models the core of the language and not all of it: literals, paths
/// into the data, `defined()`, `!`, `&&`, `||`, `==` and `!=`. The
/// expression engine behind `when` has more operators and a library of
/// functions, which its own crate tests; what is held here is that the
/// engine still decides this core the way it always has.
#[derive(Clone, Debug)]
enum Model {
    Literal(Value),
    Path(Vec<String>),
    Defined(Vec<String>),
    Not(Box<Model>),
    And(Box<Model>, Box<Model>),
    Or(Box<Model>, Box<Model>),
    Equal(Box<Model>, Box<Model>),
    NotEqual(Box<Model>, Box<Model>),
}

const KEYS: &[&str] = &[
    "env", "count", "on", "off", "region", "name", "zones", "none",
];

fn path() -> impl Strategy<Value = Vec<String>> {
    vec(prop::sample::select(KEYS), 1..4)
        .prop_map(|keys| keys.into_iter().map(str::to_owned).collect())
}

fn model() -> impl Strategy<Value = Model> {
    let literal = prop_oneof![
        Just(Value::Null),
        any::<bool>().prop_map(Value::Bool),
        (-50i64..50).prop_map(Value::Int),
        (-50i64..50).prop_map(|n| Value::Float(n as f64 + 0.5)),
        // Text that opens an interpolation is refused in a condition, which
        // `tests/interpolation.rs` covers; here it would only be noise.
        prop_oneof!["[a-z]{0,5}", "[ -~]{0,8}"]
            .prop_filter("opens an interpolation", |text| {
                !text.contains(INTERPOLATION)
            })
            .prop_map(Value::String),
    ];
    let leaf = prop_oneof![
        3 => literal.prop_map(Model::Literal),
        4 => path().prop_map(Model::Path),
        2 => path().prop_map(Model::Defined),
    ];
    leaf.prop_recursive(5, 40, 2, |inner| {
        let pair = (inner.clone(), inner.clone());
        prop_oneof![
            inner.clone().prop_map(|a| Model::Not(Box::new(a))),
            pair.clone()
                .prop_map(|(a, b)| Model::And(Box::new(a), Box::new(b))),
            pair.clone()
                .prop_map(|(a, b)| Model::Or(Box::new(a), Box::new(b))),
            pair.clone()
                .prop_map(|(a, b)| Model::Equal(Box::new(a), Box::new(b))),
            pair.prop_map(|(a, b)| Model::NotEqual(Box::new(a), Box::new(b))),
        ]
    })
}

/// Data shaped so that a generated path often finds something, sometimes
/// finds nothing, and sometimes walks into a value that has no keys.
fn condition_data() -> impl Strategy<Value = BTreeMap<String, Value>> {
    let leaf = prop_oneof![
        Just(Value::Null),
        any::<bool>().prop_map(Value::Bool),
        (-50i64..50).prop_map(Value::Int),
        (-50i64..50).prop_map(|n| Value::Float(n as f64 + 0.5)),
        "[a-z]{0,5}".prop_map(Value::String),
        Just(Value::List(vec![Value::Int(1)])),
    ];
    let tree = leaf.prop_recursive(3, 20, 4, |inner| {
        btree_map(
            prop::sample::select(KEYS).prop_map(str::to_owned),
            inner,
            0..4,
        )
        .prop_map(Value::Map)
    });
    btree_map(
        prop::sample::select(KEYS).prop_map(str::to_owned),
        tree,
        0..6,
    )
}

impl Model {
    /// How tightly the outermost operator binds: `||` least, then `&&`, then
    /// `==`, then `!` and the leaves.
    fn binding(&self) -> u8 {
        match self {
            Model::Or(..) => 1,
            Model::And(..) => 2,
            Model::Equal(..) | Model::NotEqual(..) => 3,
            _ => 4,
        }
    }

    /// The condition as text, with only the parentheses the grammar needs,
    /// so that the parser's own precedence is what is being tested.
    fn text(&self) -> String {
        let inside = |child: &Model, at_least: u8| {
            if child.binding() < at_least {
                format!("({})", child.text())
            } else {
                child.text()
            }
        };
        match self {
            Model::Literal(Value::String(s)) => {
                format!("'{}'", s.replace('\\', "\\\\").replace('\'', "\\'"))
            }
            Model::Literal(Value::Float(f)) => format!("{f:?}"),
            Model::Literal(other) => other.canonical(),
            Model::Path(keys) => format!("data.{}", keys.join(".")),
            Model::Defined(keys) => format!("defined(data.{})", keys.join(".")),
            Model::Not(a) => format!("!{}", inside(a, 4)),
            // `a && b && c` groups from the left; grouping from the right
            // decides the same, so a right-hand `&&` needs no parentheses.
            Model::And(a, b) => format!("{} && {}", inside(a, 2), inside(b, 2)),
            Model::Or(a, b) => format!("{} || {}", inside(a, 1), inside(b, 1)),
            // A comparison does not chain: each side is a `!` or a leaf.
            Model::Equal(a, b) => format!("{} == {}", inside(a, 4), inside(b, 4)),
            Model::NotEqual(a, b) => format!("{} != {}", inside(a, 4), inside(b, 4)),
        }
    }

    /// `Err` wherever the documentation says a condition cannot be decided.
    fn value(&self, data: &BTreeMap<String, Value>) -> Result<Value, ()> {
        fn find<'a>(
            data: &'a BTreeMap<String, Value>,
            keys: &[String],
        ) -> Result<Option<&'a Value>, ()> {
            let Some(mut current) = data.get(&keys[0]) else {
                return Ok(None);
            };
            for key in &keys[1..] {
                match current {
                    Value::Map(map) => match map.get(key) {
                        Some(next) => current = next,
                        None => return Ok(None),
                    },
                    // Asking a string for a key is a mistake, not "unset".
                    _ => return Err(()),
                }
            }
            Ok(Some(current))
        }
        Ok(match self {
            Model::Literal(v) => v.clone(),
            Model::Path(keys) => find(data, keys)?.ok_or(())?.clone(),
            Model::Defined(keys) => Value::Bool(find(data, keys)?.is_some()),
            Model::Not(a) => Value::Bool(!a.truth(data)?),
            Model::And(a, b) => Value::Bool(a.truth(data)? && b.truth(data)?),
            Model::Or(a, b) => Value::Bool(a.truth(data)? || b.truth(data)?),
            Model::Equal(a, b) => Value::Bool(same_scalar(&a.value(data)?, &b.value(data)?)),
            Model::NotEqual(a, b) => Value::Bool(!same_scalar(&a.value(data)?, &b.value(data)?)),
        })
    }

    fn truth(&self, data: &BTreeMap<String, Value>) -> Result<bool, ()> {
        match self.value(data)? {
            Value::Bool(b) => Ok(b),
            _ => Err(()),
        }
    }
}

/// `==` in the condition language: numbers by value, everything else as
/// itself, and nothing reaches inside a list or a mapping.
fn same_scalar(a: &Value, b: &Value) -> bool {
    let number = |v: &Value| match v {
        Value::Int(n) => Some(*n as f64),
        Value::Uint(n) => Some(*n as f64),
        Value::Float(n) => Some(*n),
        _ => None,
    };
    match (number(a), number(b)) {
        (Some(x), Some(y)) => x == y,
        _ => a == b,
    }
}

proptest! {
    #![proptest_config(cases(3000))]

    /// The real condition language and the model in this file decide every
    /// generated condition the same way against every generated data set:
    /// the same yes, the same no, and an error in the same places. This is
    /// what "a condition means the same on every target" rests on, since
    /// conditions are decided here, once, before any target sees a document.
    #[test]
    fn a_condition_is_decided_as_the_documented_language_says(
        model in model(),
        data in condition_data(),
    ) {
        let text = model.text();
        let condition = Condition::parse(&text)
            .map_err(|e| TestCaseError::fail(format!("{text:?} does not parse: {e}")))?;
        let real = condition.evaluate(&data);
        let expected = model.truth(&data);
        match (&real, &expected) {
            (Ok(a), Ok(b)) => prop_assert_eq!(a, b, "{}", text),
            (Err(_), Err(())) => {}
            _ => prop_assert!(false, "{text:?} over {data:?}: decided {real:?}, the model says {expected:?}"),
        }
        // Deciding twice decides the same, and parsing twice parses the same.
        prop_assert_eq!(&real, &condition.evaluate(&data));
        prop_assert_eq!(&condition, &Condition::parse(&text).unwrap());
        // Spaces around the whole condition, and parentheses around it, mean nothing.
        let wrapped = Condition::parse(&format!("  ( {text} )\t")).unwrap();
        prop_assert_eq!(wrapped.evaluate(&data).ok(), real.ok());
    }

    /// De Morgan and double negation hold wherever both sides can be decided.
    #[test]
    fn the_laws_of_logic_hold(a in model(), b in model(), data in condition_data()) {
        let decide = |text: String| Condition::parse(&text).ok().and_then(|c| c.evaluate(&data).ok());
        let (a, b) = (format!("({})", a.text()), format!("({})", b.text()));
        if let (Some(x), Some(y)) = (decide(a.clone()), decide(b.clone())) {
            prop_assert_eq!(decide(format!("!({a} && {b})")), Some(!(x && y)));
            prop_assert_eq!(decide(format!("!{a} || !{b}")), Some(!x || !y));
            prop_assert_eq!(decide(format!("!!{a}")), Some(x));
            prop_assert_eq!(decide(format!("{a} == {b}")), Some(x == y));
            prop_assert_eq!(decide(format!("{a} != {b}")), Some(x != y));
        }
    }

    #[test]
    fn a_condition_parser_survives_anything(text in "\\PC{0,60}") {
        if let Ok(condition) = Condition::parse(&text) {
            let _ = condition.evaluate(&BTreeMap::new());
        }
    }

    #[test]
    fn a_condition_parser_survives_its_own_tokens_in_any_order(
        tokens in vec(prop::sample::select(vec![
            "(", ")", ".", "!", "&&", "||", "==", "!=", "data", "defined", "true", "false",
            "null", "env", "1", "-1", "1.5", "'a'", "\"b\"", "'", "\"", "\\", "&", "|", "=", "<",
            ">", "facts", " ", "-", "1e9", "0x1f", "data.env", "data.", ".env",
        ]), 0..14)
    ) {
        let text = tokens.concat();
        if let Ok(condition) = Condition::parse(&text) {
            let _ = condition.evaluate(&BTreeMap::new());
        }
        let spaced = tokens.join(" ");
        if let Ok(condition) = Condition::parse(&spaced) {
            let _ = condition.evaluate(&BTreeMap::new());
        }
    }
}

// ---------------------------------------------------------------------------
// `when`: removing resources and their edges
// ---------------------------------------------------------------------------

#[derive(Clone, Debug)]
struct Conditional {
    /// `None` is no `when`; otherwise what it decides.
    when: Option<bool>,
    depends_on: Vec<(usize, bool)>,
    notifies: Vec<(usize, bool)>,
}

fn conditionals() -> impl Strategy<Value = Vec<Conditional>> {
    (1usize..9).prop_flat_map(|n| {
        let edge = || vec((0..n, any::<bool>()), 0..3);
        vec(
            (any::<Option<bool>>(), edge(), edge()).prop_map(|(when, depends_on, notifies)| {
                Conditional {
                    when,
                    depends_on,
                    notifies,
                }
            }),
            n,
        )
    })
}

proptest! {
    #![proptest_config(cases(1500))]

    /// What `when` does to a document, in full: a resource whose condition
    /// is false is gone; an edge another resource holds to it is dropped
    /// when that edge says `ifMissing: skip` and refuses the document
    /// otherwise; everything else is kept as written, in order, with no
    /// condition left on it.
    #[test]
    fn pruning_removes_exactly_what_the_conditions_say(sketch in conditionals()) {
        let policy = |skip: bool| if skip { IfMissing::Skip } else { IfMissing::Unset };
        let resources: Vec<Resource> = sketch
            .iter()
            .enumerate()
            .map(|(i, c)| Resource {
                id: format!("r{i}"),
                r#type: "exec".to_owned(),
                index: i,
                when: match c.when {
                    None => String::new(),
                    // Decided from data, as a real condition is.
                    Some(true) => "data.yes && !data.no".to_owned(),
                    Some(false) => "data.no || !data.yes".to_owned(),
                },
                depends_on: c
                    .depends_on
                    .iter()
                    .map(|(j, skip)| Dependency {
                        resource: format!("r{j}"),
                        if_missing: policy(*skip),
                        ..Dependency::default()
                    })
                    .collect(),
                notifies: c
                    .notifies
                    .iter()
                    .map(|(j, skip)| Notification {
                        resource: format!("r{j}"),
                        action: "run".to_owned(),
                        if_missing: policy(*skip),
                        ..Notification::default()
                    })
                    .collect(),
                ..Resource::default()
            })
            .collect();
        let data = BTreeMap::from([
            ("yes".to_owned(), Value::Bool(true)),
            ("no".to_owned(), Value::Bool(false)),
        ]);

        let removed = |j: usize| sketch[j].when == Some(false);
        let must_refuse = sketch.iter().enumerate().any(|(i, c)| {
            !removed(i)
                && c.depends_on
                    .iter()
                    .chain(&c.notifies)
                    .any(|(j, skip)| removed(*j) && !skip)
        });

        match prune(&resources, &data) {
            Err(error) => {
                prop_assert!(must_refuse, "refused without cause: {error}");
                prop_assert!(error.to_string().contains("ifMissing: skip"), "{error}");
            }
            Ok(kept) => {
                prop_assert!(!must_refuse, "an edge to a removed resource was let through");
                let want: Vec<String> = (0..sketch.len())
                    .filter(|i| !removed(*i))
                    .map(|i| format!("r{i}"))
                    .collect();
                let got: Vec<String> = kept.iter().map(|r| r.id.clone()).collect();
                prop_assert_eq!(&got, &want);
                for resource in &kept {
                    let i: usize = resource.id[1..].parse().unwrap();
                    prop_assert!(resource.when.is_empty());
                    let deps: Vec<String> =
                        resource.depends_on.iter().map(|d| d.resource.clone()).collect();
                    let want_deps: Vec<String> = sketch[i]
                        .depends_on
                        .iter()
                        .filter(|(j, _)| !removed(*j))
                        .map(|(j, _)| format!("r{j}"))
                        .collect();
                    prop_assert_eq!(deps, want_deps);
                    let notes: Vec<String> =
                        resource.notifies.iter().map(|n| n.resource.clone()).collect();
                    let want_notes: Vec<String> = sketch[i]
                        .notifies
                        .iter()
                        .filter(|(j, _)| !removed(*j))
                        .map(|(j, _)| format!("r{j}"))
                        .collect();
                    prop_assert_eq!(notes, want_notes);
                }
                // Pruning what is already unconditional changes nothing.
                prop_assert_eq!(&prune(&kept, &data).unwrap(), &kept);
            }
        }

        // With no data at all, a document that has a condition cannot be
        // decided, and one without any is returned as it is.
        let undecided = prune(&resources, &BTreeMap::new());
        if sketch.iter().all(|c| c.when.is_none()) {
            prop_assert_eq!(undecided.unwrap(), resources);
        } else {
            prop_assert!(undecided.is_err());
        }
    }
}

// ---------------------------------------------------------------------------
// References
// ---------------------------------------------------------------------------

proptest! {
    #![proptest_config(cases(1500))]

    #[test]
    fn splitting_text_into_references_survives_anything(text in "\\PC{0,80}") {
        let _ = expr::parse(&text);
        let _ = expr::has_expr(&text);
        let _ = expr::is_path(&text);
        let (name, rest) = expr::head(&text);
        prop_assert_eq!(format!("{name}{rest}"), text);
    }

    #[test]
    fn splitting_survives_dollars_and_braces(text in "[${}a. ]{0,40}") {
        match expr::parse(&text) {
            Ok(parts) => {
                // Text between references is never empty and never doubled up.
                for pair in parts.windows(2) {
                    prop_assert!(!matches!(pair, [Part::Text(_), Part::Text(_)]), "{parts:?}");
                }
                for part in &parts {
                    if let Part::Text(t) = part {
                        prop_assert!(!t.is_empty());
                    }
                }
            }
            Err(why) => prop_assert!(why.contains("never closed"), "{why}"),
        }
    }

    /// Text and references laid end to end split back into the same pieces,
    /// and text that holds no `${` is one piece of text, untouched.
    #[test]
    fn a_string_splits_back_into_the_pieces_it_was_built_from(
        pieces in vec(
            prop_oneof![
                "[a-z /:=_.,-]{1,10}".prop_map(Part::Text),
                "[a-z][a-z0-9_]{0,5}\\.[a-z][a-z0-9_]{0,5}".prop_map(Part::Expr),
            ],
            0..8,
        )
    ) {
        let mut text = String::new();
        let mut expected: Vec<Part> = Vec::new();
        for piece in &pieces {
            match piece {
                Part::Text(t) => {
                    text.push_str(t);
                    match expected.last_mut() {
                        Some(Part::Text(last)) => last.push_str(t),
                        _ => expected.push(Part::Text(t.clone())),
                    }
                }
                Part::Expr(e) => {
                    text.push_str(&format!("${{{e}}}"));
                    expected.push(Part::Expr(e.clone()));
                }
            }
        }
        prop_assert_eq!(expr::parse(&text).unwrap(), expected.clone());
        prop_assert_eq!(expr::has_expr(&text), expected.iter().any(|p| matches!(p, Part::Expr(_))));
        // The same text with every reference escaped holds no reference.
        let escaped = text.replace("${", "$${");
        prop_assert!(!expr::has_expr(&escaped));
        let literal: String = expr::parse(&escaped)
            .unwrap()
            .into_iter()
            .map(|p| match p {
                Part::Text(t) => t,
                Part::Expr(e) => panic!("an escaped reference was read as {e}"),
            })
            .collect();
        prop_assert_eq!(literal, text);
    }
}

// ---------------------------------------------------------------------------
// Selectors
// ---------------------------------------------------------------------------

#[derive(Clone, Debug)]
enum Requirement {
    Is(String, String),
    IsNot(String, String),
    Set(String),
    Unset(String),
}

fn requirement() -> impl Strategy<Value = Requirement> {
    let key = || prop::sample::select(vec!["tier", "env", "owner", "zone"]).prop_map(str::to_owned);
    let value =
        || prop::sample::select(vec!["web", "db", "prod", "dev", ""]).prop_map(str::to_owned);
    prop_oneof![
        (key(), value()).prop_map(|(k, v)| Requirement::Is(k, v)),
        (key(), value()).prop_map(|(k, v)| Requirement::IsNot(k, v)),
        key().prop_map(Requirement::Set),
        key().prop_map(Requirement::Unset),
    ]
}

impl Requirement {
    fn text(&self, double: bool) -> String {
        match self {
            Requirement::Is(k, v) if double => format!("{k}=={v}"),
            Requirement::Is(k, v) => format!("{k}={v}"),
            Requirement::IsNot(k, v) => format!("{k}!={v}"),
            Requirement::Set(k) => k.clone(),
            Requirement::Unset(k) => format!("!{k}"),
        }
    }

    fn holds(&self, labels: &BTreeMap<String, String>) -> bool {
        match self {
            Requirement::Is(k, v) => labels.get(k) == Some(v),
            Requirement::IsNot(k, v) => labels.get(k) != Some(v),
            Requirement::Set(k) => labels.contains_key(k),
            Requirement::Unset(k) => !labels.contains_key(k),
        }
    }
}

proptest! {
    #![proptest_config(cases(1500))]

    /// A selector selects a document exactly when every requirement holds,
    /// however the requirements are spaced, spelt (`=` or `==`) or split
    /// across several `-l` options.
    #[test]
    fn a_selector_selects_exactly_when_every_requirement_holds(
        requirements in vec(requirement(), 1..5),
        labels in btree_map(
            prop::sample::select(vec!["tier", "env", "owner", "zone"]).prop_map(str::to_owned),
            prop::sample::select(vec!["web", "db", "prod", "dev", ""]).prop_map(str::to_owned),
            0..4,
        ),
        double in any::<bool>(),
        split in any::<prop::sample::Index>(),
    ) {
        let expected = requirements.iter().all(|r| r.holds(&labels));
        let texts: Vec<String> = requirements.iter().map(|r| r.text(double)).collect();

        let joined = Selector::parse(&texts.join(",")).map_err(TestCaseError::fail)?;
        prop_assert_eq!(joined.matches(&labels), expected, "{}", texts.join(","));
        prop_assert!(!joined.is_all());

        let spaced = Selector::parse(&format!(" {} ", texts.join(" , "))).map_err(TestCaseError::fail)?;
        prop_assert_eq!(spaced.matches(&labels), expected);

        // `-l a,b -l c` is `-l a,b,c`.
        let at = split.index(texts.len());
        let mut combined = Selector::all();
        for part in [&texts[..at], &texts[at..]] {
            if !part.is_empty() {
                combined = combined.and(Selector::parse(&part.join(",")).unwrap());
            }
        }
        prop_assert_eq!(combined.matches(&labels), expected);
    }

    #[test]
    fn a_selector_parser_survives_anything(text in "\\PC{0,40}", labels in btree_map(name(), name(), 0..3)) {
        if let Ok(selector) = Selector::parse(&text) {
            let _ = selector.matches(&labels);
        }
        prop_assert!(Selector::all().matches(&labels));
    }
}

// ---------------------------------------------------------------------------
// The hierarchy resolver
// ---------------------------------------------------------------------------

const LAYERS: &[&str] = &["common.yaml", "env.yaml", "role.yaml", "node.yaml"];

fn layers() -> impl Strategy<Value = Vec<Option<BTreeMap<String, Value>>>> {
    let key = || prop::sample::select(vec!["a", "b", "c", "d"]).prop_map(str::to_owned);
    let leaf = prop_oneof![
        (-9i64..9).prop_map(Value::Int),
        "[a-z]{0,3}".prop_map(Value::String),
        any::<bool>().prop_map(Value::Bool),
    ];
    let tree = leaf.prop_recursive(3, 16, 3, move |inner| {
        btree_map(
            prop::sample::select(vec!["x", "y", "z"]).prop_map(str::to_owned),
            inner,
            0..3,
        )
        .prop_map(Value::Map)
    });
    vec(
        prop::option::weighted(0.8, btree_map(key(), tree, 0..4)),
        LAYERS.len(),
    )
}

/// Deep merge as its documentation states it: two mappings merge key by key,
/// and anything else is replaced whole by the higher layer.
fn deep(base: &Value, over: &Value) -> Value {
    match (base, over) {
        (Value::Map(base), Value::Map(over)) => {
            let mut out = base.clone();
            for (key, value) in over {
                let merged = match base.get(key) {
                    Some(existing) => deep(existing, value),
                    None => value.clone(),
                };
                out.insert(key.clone(), merged);
            }
            Value::Map(out)
        }
        _ => over.clone(),
    }
}

proptest! {
    #![proptest_config(cases(800))]

    /// The resolver against a model of each strategy: with first-found the
    /// highest layer that sets a key supplies it whole; with deep-merge the
    /// layers fold together from the lowest up. Either way a missing layer
    /// is an empty one, every key any layer sets is in the result, and the
    /// same inputs always give the same answer.
    #[test]
    fn layers_resolve_as_each_strategy_is_documented(layers in layers(), deep_merge in any::<bool>()) {
        let loader = MapLoader(
            LAYERS
                .iter()
                .zip(&layers)
                .filter_map(|(path, data)| data.clone().map(|d| ((*path).to_owned(), d)))
                .collect(),
        );
        let mut hierarchy = Hierarchy::new(LAYERS.iter().copied());
        if deep_merge {
            hierarchy.default = KeyRule::new(Merge::DeepMerge);
        }
        let resolved = resolve(&hierarchy, &Scope::new(), &loader)
            .map_err(|e| TestCaseError::fail(e.to_string()))?;

        let mut expected: BTreeMap<String, Value> = BTreeMap::new();
        for layer in layers.iter().flatten() {
            for (key, value) in layer {
                let merged = match expected.get(key) {
                    Some(lower) if deep_merge => deep(lower, value),
                    _ => value.clone(),
                };
                expected.insert(key.clone(), merged);
            }
        }
        prop_assert_eq!(&resolved.data, &expected);

        prop_assert_eq!(resolved.layers.len(), LAYERS.len());
        for ((report, path), data) in resolved.layers.iter().zip(LAYERS).zip(&layers) {
            prop_assert_eq!(&report.path, path);
            prop_assert_eq!(report.found, data.is_some());
            let keys: Vec<String> = data.iter().flat_map(|d| d.keys().cloned()).collect();
            prop_assert_eq!(&report.keys, &keys);
        }

        prop_assert_eq!(&resolve(&hierarchy, &Scope::new(), &loader).unwrap(), &resolved);
        // Merging a layer with itself changes nothing.
        let doubled = MapLoader(
            loader.0.iter().flat_map(|(path, data)| {
                [(path.clone(), data.clone()), (format!("again-{path}"), data.clone())]
            }).collect(),
        );
        let twice = Hierarchy {
            paths: LAYERS.iter().flat_map(|p| [(*p).to_owned(), format!("again-{p}")]).collect(),
            ..hierarchy.clone()
        };
        prop_assert_eq!(&resolve(&twice, &Scope::new(), &doubled).unwrap().data, &resolved.data);
    }

    /// unique-array-merge keeps the first of each item, lowest layer first,
    /// and nothing else.
    #[test]
    fn arrays_merge_without_repeats_and_in_layer_order(
        lists in vec(prop::option::weighted(0.8, vec(0i64..6, 0..5)), LAYERS.len())
    ) {
        let loader = MapLoader(
            LAYERS
                .iter()
                .zip(&lists)
                .filter_map(|(path, list)| {
                    list.as_ref().map(|items| {
                        let items = Value::List(items.iter().copied().map(Value::Int).collect());
                        ((*path).to_owned(), BTreeMap::from([("list".to_owned(), items)]))
                    })
                })
                .collect(),
        );
        let mut hierarchy = Hierarchy::new(LAYERS.iter().copied());
        hierarchy.keys.insert("list".to_owned(), KeyRule::new(Merge::UniqueArrayMerge));
        let resolved = resolve(&hierarchy, &Scope::new(), &loader).unwrap();

        let mut expected = Vec::new();
        for item in lists.iter().flatten().flatten() {
            if !expected.contains(item) {
                expected.push(*item);
            }
        }
        match resolved.data.get("list") {
            None => prop_assert!(lists.iter().all(Option::is_none)),
            Some(Value::List(items)) => {
                let got: Vec<i64> = items.iter().map(|v| match v {
                    Value::Int(n) => *n,
                    other => panic!("{other:?}"),
                }).collect();
                prop_assert_eq!(got, expected);
            }
            Some(other) => prop_assert!(false, "{other:?}"),
        }
    }
}
