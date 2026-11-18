mod common;

use common::*;
use meridian_ir::{parse, parse_file, validate, Bundle, Document, IfMissing, Kind, Value};

#[test]
fn parses_a_valid_infrastructure_document() {
    let docs = parse("infra.yaml", VALID_INFRASTRUCTURE.as_bytes()).unwrap();
    assert_eq!(docs.len(), 1);
    let infra = docs[0]
        .as_infrastructure()
        .expect("an Infrastructure document");
    assert_eq!(infra.metadata.name, "web-vm");
    assert_eq!(infra.spec.target, "terraform");
    assert_eq!(infra.spec.resources.len(), 1);
    let r = &infra.spec.resources[0];
    assert_eq!(
        (r.id.as_str(), r.r#type.as_str()),
        ("web_vm", "compute_instance")
    );
    assert_eq!(r.params["image"], Value::from("ubuntu-22.04"));
    let out = infra.output("web_vm_ip").expect("output web_vm_ip");
    assert_eq!(out.value, "${web_vm.public_ip}");
}

#[test]
fn parses_a_valid_resource_set() {
    let docs = parse("rs.yaml", VALID_RESOURCE_SET.as_bytes()).unwrap();
    let rs = docs[0].as_resource_set().expect("a ResourceSet document");
    assert_eq!(
        rs.spec.target_host.as_ref().map(|t| t.r#ref.as_str()),
        Some("web-vm")
    );
    assert_eq!(rs.spec.resources.len(), 3);
    for (i, r) in rs.spec.resources.iter().enumerate() {
        assert_eq!(r.index, i, "resource {} index", r.id);
        assert_ne!(r.pos.line, 0, "resource {} has no source line", r.id);
    }
    let conf = &rs.spec.resources[1];
    assert_eq!(conf.depends_on.len(), 1);
    assert_eq!(conf.depends_on[0].resource, "nginx_pkg");
    assert_eq!(conf.depends_on[0].if_missing, IfMissing::Error);
    assert_eq!(conf.notifies.len(), 1);
    assert_eq!(conf.notifies[0].action, "restart");
    assert_ne!(
        conf.notifies[0].pos.line, 0,
        "notify edge has no source line"
    );
}

/// Positions are what an author follows to find a mistake, so they are pinned
/// exactly: 1-based, and pointing at the first key of the thing they locate.
#[test]
fn positions_point_at_the_first_key_of_each_element() {
    let docs = parse("rs.yaml", VALID_RESOURCE_SET.as_bytes()).unwrap();
    let rs = docs[0].as_resource_set().unwrap();
    assert_eq!((rs.pos.line, rs.pos.column), (1, 1));
    let th = rs.spec.target_host.as_ref().unwrap();
    assert_eq!((th.pos.line, th.pos.column), (8, 5));
    let conf = &rs.spec.resources[1];
    assert_eq!((conf.pos.line, conf.pos.column), (17, 7));
    assert_eq!(
        (conf.depends_on[0].pos.line, conf.depends_on[0].pos.column),
        (22, 11)
    );
    assert_eq!(
        (conf.notifies[0].pos.line, conf.notifies[0].pos.column),
        (25, 11)
    );
}

#[test]
fn json_parses_to_the_same_document_as_yaml() {
    let json = r#"{
  "apiVersion": "meridian/v1",
  "kind": "ResourceSet",
  "metadata": {"name": "web-server-baseline"},
  "spec": {
    "target": "ansible",
    "resources": [
      {"id": "nginx_pkg", "type": "package", "state": "present"},
      {"id": "nginx_svc", "type": "service", "dependsOn": [{"resource": "nginx_pkg"}]}
    ]
  }
}"#;
    let docs = parse("rs.json", json.as_bytes()).unwrap();
    let rs = docs[0].as_resource_set().unwrap();
    assert_eq!(rs.spec.resources.len(), 2);
    assert_eq!(rs.spec.resources[1].depends_on[0].resource, "nginx_pkg");
    assert_eq!(rs.spec.resources[1].index, 1);
    validate(&docs[0]).unwrap();
}

#[test]
fn a_json_array_is_a_document_stream() {
    let stream = r#"[
  {"apiVersion":"meridian/v1","kind":"Infrastructure","metadata":{"name":"a"},
   "spec":{"target":"terraform","resources":[{"id":"x","type":"compute_instance"}],
           "outputs":[{"name":"ip","value":"${x.ip}"}]}},
  {"apiVersion":"meridian/v1","kind":"ResourceSet","metadata":{"name":"b"},
   "spec":{"target":"ansible","targetHost":{"from":"infrastructure","ref":"a","output":"ip"},
           "resources":[{"id":"y","type":"package"}]}}
]"#;
    let docs = parse("bundle.json", stream.as_bytes()).unwrap();
    assert_eq!(docs.len(), 2);
    Bundle::new(docs).validate().unwrap();
}

#[test]
fn malformed_yaml_is_located_not_a_panic() {
    let broken = "apiVersion: meridian/v1\nkind: ResourceSet\nmetadata:\n  name: broken\n spec:\n   target: ansible\n";
    let err = parse("broken.yaml", broken.as_bytes()).unwrap_err();
    assert_ne!(err.first().unwrap().pos.line, 0, "error has no line: {err}");
    assert!(
        err.to_string().contains("broken.yaml"),
        "error does not name the file: {err}"
    );
}

#[test]
fn malformed_json_is_located() {
    let broken = "{\n  \"apiVersion\": \"meridian/v1\",\n  \"kind\": \"ResourceSet\",,\n}";
    let err = parse("broken.json", broken.as_bytes()).unwrap_err();
    assert_eq!(err.first().unwrap().pos.line, 3, "{err}");
}

#[test]
fn an_unknown_api_version_or_kind_is_explicit() {
    for (name, doc, want) in [
        (
            "future version",
            VALID_RESOURCE_SET.replacen("meridian/v1", "meridian/v9", 1),
            "unsupported apiVersion",
        ),
        (
            "missing",
            VALID_RESOURCE_SET.replacen("apiVersion: meridian/v1\n", "", 1),
            "apiVersion: missing required field",
        ),
        (
            "unknown kind",
            VALID_RESOURCE_SET.replacen("kind: ResourceSet", "kind: Playbook", 1),
            "unknown kind",
        ),
    ] {
        let err = parse("doc.yaml", doc.as_bytes()).unwrap_err();
        assert!(
            err.to_string().contains(want),
            "{name}: error = {err}, want it to contain {want:?}"
        );
    }
}

/// KYAML, the strict YAML subset kubectl can print (`kubectl get -o kyaml`):
/// a `---` header, `{}` and `[]` flow collections one entry per line,
/// trailing commas, every string double-quoted. It is YAML, so it parses to
/// the same document as the block-style twin; this pins that.
#[test]
fn a_kyaml_document_reads_the_same_as_its_block_twin() {
    let block = "\
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
    host: web01
  resources:
    - id: nginx
      type: package
      params:
        name: nginx
    - id: conf
      type: file
      params:
        path: /etc/nginx/nginx.conf
        mode: \"0644\"
        content: |
          worker_processes 2;
      dependsOn:
        - resource: nginx
";
    let kyaml = "\
---
{
  apiVersion: \"meridian/v1\",
  kind: \"ResourceSet\",
  metadata: {
    labels: {
      tier: \"web\",
    },
    name: \"web\",
  },
  spec: {
    resources: [{
      id: \"nginx\",
      params: {
        name: \"nginx\",
      },
      type: \"package\",
    }, {
      dependsOn: [{
        resource: \"nginx\",
      }],
      id: \"conf\",
      params: {
        content: \"worker_processes 2;\\n\",
        mode: \"0644\",
        path: \"/etc/nginx/nginx.conf\",
      },
      type: \"file\",
    }],
    target: \"ansible\",
    targetHost: {
      from: \"inline\",
      host: \"web01\",
    },
  },
}
";
    let from_block = parse("block.yaml", block.as_bytes()).unwrap();
    let from_kyaml = parse("kyaml.yaml", kyaml.as_bytes()).unwrap();
    assert_eq!(from_block.len(), 1);
    let (b, k) = (
        from_block[0].as_resource_set().unwrap(),
        from_kyaml[0].as_resource_set().unwrap(),
    );
    assert_eq!(b.metadata, k.metadata);
    assert_eq!(b.spec.target, k.spec.target);
    let host = |t: &Option<meridian_ir::TargetHost>| {
        t.as_ref()
            .map(|h| (h.from.clone(), h.host.clone(), h.r#ref.clone()))
    };
    assert_eq!(host(&b.spec.target_host), host(&k.spec.target_host));
    assert_eq!(b.spec.resources.len(), k.spec.resources.len());
    for (rb, rk) in b.spec.resources.iter().zip(&k.spec.resources) {
        assert_eq!(
            (&rb.id, &rb.r#type, &rb.params),
            (&rk.id, &rk.r#type, &rk.params)
        );
        assert_eq!(rb.depends_on.len(), rk.depends_on.len());
    }
    // A stream of several KYAML documents is a YAML stream like any other.
    let stream = format!("{kyaml}{}", kyaml.replace("name: \"web\"", "name: \"api\""));
    let docs = parse("stream.yaml", stream.as_bytes()).unwrap();
    assert_eq!(docs.len(), 2);
    assert_eq!(docs[1].name(), "api");
}

/// `!!str` is the one tag a document may carry. Any other tag is refused
/// with its position, never ignored: an ignored `!!int "12"` would read as
/// the text `12`, the opposite of what was written.
#[test]
fn a_tag_other_than_str_is_refused() {
    for (tag, written) in [
        ("!!int \"12\"", "!!int"),
        ("!!float 1", "!!float"),
        ("!!bool yes", "!!bool"),
        ("!!binary aGk=", "!!binary"),
        ("!custom x", "!custom"),
        ("!!set {a, b}", "!!set"),
        ("!!seq [1]", "!!seq"),
    ] {
        let yaml = format!(
            "apiVersion: meridian/v1\nkind: ResourceSet\nmetadata: {{name: a}}\nspec:\n  target: ansible\n  \
             resources:\n    - id: a\n      type: file\n      params:\n        tagged: {tag}\n"
        );
        let err = parse("doc.yaml", yaml.as_bytes()).expect_err(tag);
        let e = err.first().unwrap();
        assert!(
            e.msg
                .contains(&format!("the tag {written} is not supported")),
            "{tag}: {}",
            e.msg
        );
        assert_eq!((e.pos.line, e.pos.column), (10, 17), "{tag}");
    }
}

/// A leading zero means octal, as a file mode is written; `08` and `09` are
/// not octal and no number was meant, so they stay text.
#[test]
fn digits_with_a_leading_zero_that_are_not_octal_are_text() {
    let p = params(concat!(
        "        mode: 0644\n",
        "        build: 08\n",
        "        day: 09\n",
    ));
    assert_eq!(p["mode"], Value::Int(0o644));
    assert_eq!(p["build"], Value::from("08"));
    assert_eq!(p["day"], Value::from("09"));
}

#[test]
fn an_unknown_field_is_an_error() {
    let yaml = VALID_RESOURCE_SET.replacen(
        "      type: package",
        "      type: package\n      typo: nope",
        1,
    );
    let json = r#"{"apiVersion":"meridian/v1","kind":"ResourceSet","metadata":{"name":"a"},
                   "spec":{"target":"ansible","resources":[{"id":"x","type":"package","typo":"nope"}]}}"#;
    for (name, doc) in [("yaml", yaml.as_str()), ("json", json)] {
        let err = parse(&format!("doc.{name}"), doc.as_bytes())
            .expect_err("unknown fields must not be silently dropped");
        assert!(
            err.to_string().contains("typo"),
            "{name}: error = {err}, want it to name the field"
        );
    }
    // In YAML the diagnostic lands on the offending key itself.
    let err = parse("doc.yaml", yaml.as_bytes()).unwrap_err();
    let e = err.first().unwrap();
    assert_eq!(
        (e.pos.line, e.pos.column, e.path.as_str()),
        (14, 7, "spec.resources[0]"),
        "{err}"
    );
}

#[test]
fn parses_multi_document_yaml() {
    let docs = parse("bundle.yaml", bundle_text().as_bytes()).unwrap();
    assert_eq!(docs.len(), 2);
    assert_eq!(
        (docs[0].kind(), docs[1].kind()),
        (Kind::Infrastructure, Kind::ResourceSet)
    );
    assert_eq!(docs[1].position().doc_idx, 1);
    // The second document starts after the first, so its line must be larger.
    assert!(docs[1].position().line > docs[0].position().line);
}

#[test]
fn parse_file_reads_from_disk() {
    let dir = std::env::temp_dir().join(format!("meridian-ir-test-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("infra.yaml");
    std::fs::write(&path, VALID_INFRASTRUCTURE).unwrap();
    let docs = parse_file(&path).unwrap();
    assert_eq!(docs[0].name(), "web-vm");
    let missing = parse_file(dir.join("absent.yaml")).expect_err("a missing file is an error");
    assert!(
        missing.to_string().contains("cannot read file"),
        "{missing}"
    );
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn parsing_is_deterministic() {
    let text = bundle_text();
    let first = parse("bundle.yaml", text.as_bytes()).unwrap();
    for run in 0..50 {
        let again = parse("bundle.yaml", text.as_bytes()).unwrap();
        assert_eq!(again, first, "run {run} differs");
    }
}

fn params(yaml_params: &str) -> std::collections::BTreeMap<String, Value> {
    let doc = format!(
        "apiVersion: meridian/v1\nkind: ResourceSet\nmetadata: {{name: a}}\nspec:\n  target: ansible\n  resources:\n    - id: a\n      type: file\n      params:\n{yaml_params}"
    );
    match parse_one(&doc) {
        Document::ResourceSet(mut rs) => std::mem::take(&mut rs.spec.resources[0].params),
        Document::Infrastructure(_) => unreachable!(),
    }
}

/// A param keeps the type the author wrote. These are the cases where YAML
/// libraries disagree. All but one are pinned to what the Go implementation
/// did; the exception is a date, which Go turned into a timestamp and this
/// core leaves as the text the author wrote.
#[test]
fn params_keep_their_scalar_types() {
    let p = params(concat!(
        "        port: 8080\n",
        "        mode: 0644\n",
        "        quoted_mode: \"0644\"\n",
        "        ratio: 0.5\n",
        "        enabled: true\n",
        "        legacy_bool: yes\n",
        "        nothing: ~\n",
        "        empty:\n",
        "        version: 1.2.3\n",
        "        date: 2001-12-14\n",
        "        tagged: !!str 42\n",
        "        text: |\n          line one\n          line two\n",
        "        list: [1, two, 3.0]\n",
        "        nested:\n          b: 2\n          a: 1\n",
    ));
    assert_eq!(p["port"], Value::Int(8080));
    assert_eq!(p["mode"], Value::Int(0o644));
    assert_eq!(p["quoted_mode"], Value::from("0644"));
    assert_eq!(p["ratio"], Value::Float(0.5));
    assert_eq!(p["enabled"], Value::Bool(true));
    assert_eq!(p["legacy_bool"], Value::from("yes"));
    assert_eq!(p["nothing"], Value::Null);
    assert_eq!(p["empty"], Value::Null);
    assert_eq!(p["version"], Value::from("1.2.3"));
    assert_eq!(p["date"], Value::from("2001-12-14"));
    assert_eq!(p["tagged"], Value::from("42"));
    assert_eq!(p["text"], Value::from("line one\nline two\n"));
    assert_eq!(
        p["list"],
        Value::List(vec![Value::Int(1), Value::from("two"), Value::Float(3.0)])
    );
    let Value::Map(nested) = &p["nested"] else {
        panic!("nested is not a map")
    };
    assert_eq!(
        nested.keys().collect::<Vec<_>>(),
        ["a", "b"],
        "map keys are ordered"
    );
}

#[test]
fn anchors_aliases_and_merge_keys_are_expanded() {
    let p = params(concat!(
        "        defaults: &defaults\n          owner: root\n          mode: 0644\n",
        "        file:\n          <<: *defaults\n          mode: 0600\n",
        "        again: *defaults\n",
    ));
    let Value::Map(file) = &p["file"] else {
        panic!("file is not a map")
    };
    assert_eq!(file["owner"], Value::from("root"));
    assert_eq!(
        file["mode"],
        Value::Int(0o600),
        "a key written in the mapping wins over a merged one"
    );
    assert_eq!(p["again"], p["defaults"]);
}

#[test]
fn a_scalar_field_takes_a_yaml_number_as_text_but_json_is_typed() {
    let doc = parse_one(
        "apiVersion: meridian/v1\nkind: ResourceSet\nmetadata: {name: 123}\nspec:\n  target: ansible\n  resources:\n    - {id: a, type: package}\n",
    );
    assert_eq!(doc.name(), "123");

    let json = r#"{"apiVersion":"meridian/v1","kind":"ResourceSet","metadata":{"name":123},
                   "spec":{"target":"ansible","resources":[{"id":"a","type":"package"}]}}"#;
    let err = parse("doc.json", json.as_bytes()).unwrap_err();
    assert!(
        err.to_string()
            .contains("metadata.name: expected a string, found a number"),
        "{err}"
    );
}

#[test]
fn wrong_shapes_and_repeated_keys_are_reported_together() {
    let doc = concat!(
        "apiVersion: meridian/v1\nkind: ResourceSet\nmetadata: {name: a}\nspec:\n  target: ansible\n",
        "  resources:\n    - id: a\n      id: b\n      type: [package]\n      dependsOn: nginx\n",
    );
    let err = parse("doc.yaml", doc.as_bytes()).unwrap_err();
    let text = err.to_string();
    assert_eq!(err.len(), 3, "{text}");
    for want in [
        "doc.yaml:8:7: spec.resources[0].id: field is already set at line 7",
        "doc.yaml:9:13: spec.resources[0].type: expected a string, found a list",
        "doc.yaml:10:18: spec.resources[0].dependsOn: expected a list, found a string",
    ] {
        assert!(
            text.contains(want),
            "error = {text}\nwant it to contain {want:?}"
        );
    }
}

#[test]
fn hostile_input_is_refused_not_a_crash() {
    for (name, input, want) in [
        ("empty", String::new(), "no documents found"),
        (
            "comment only",
            "# nothing here\n".to_owned(),
            "no documents found",
        ),
        (
            "not a mapping",
            "- a\n- b\n".to_owned(),
            "the document must be a mapping, found a list",
        ),
        ("invalid utf-8", String::new(), "not valid UTF-8"),
        ("deep nesting", "[".repeat(100_000), ""),
        (
            "alias expansion",
            {
                let mut s = String::from("a0: &a0 [x, x, x, x, x, x, x, x, x, x]\n");
                for i in 1..12 {
                    let p = i - 1;
                    s.push_str(&format!("a{i}: &a{i} [*a{p}, *a{p}, *a{p}, *a{p}, *a{p}, *a{p}, *a{p}, *a{p}, *a{p}, *a{p}]\n"));
                }
                s
            },
            "document is too large",
        ),
        ("unknown alias", "a: *nope\n".to_owned(), ""),
    ] {
        let bytes = if name == "invalid utf-8" {
            vec![b'a', b':', b' ', 0xff, 0xfe]
        } else {
            input.into_bytes()
        };
        let err = parse("in.yaml", &bytes).expect_err(name);
        assert!(
            err.to_string().contains(want),
            "{name}: error = {err}, want it to contain {want:?}"
        );
    }
}

/// A YAML flow mapping opens with `{` exactly as JSON does. It is not JSON,
/// and it must still parse.
#[test]
fn a_flow_style_yaml_document_is_not_mistaken_for_broken_json() {
    let doc = "{apiVersion: meridian/v1, kind: ResourceSet, metadata: {name: a}, spec: {target: ansible, resources: [{id: a, type: package}]}}\n";
    let docs = parse("flow.yaml", doc.as_bytes()).unwrap();
    assert_eq!(docs[0].name(), "a");
    // Read as YAML, so it has positions, which JSON would not.
    assert_eq!(docs[0].resources()[0].pos.line, 1);
}

#[test]
fn parse_value_reads_a_free_form_document() {
    use meridian_ir::parse_value;
    let v = parse_value("data.yaml", b"b: [1, two]\na: {x: 0.5}\n").unwrap();
    assert_eq!(v.canonical(), r#"{"a":{"x":0.5},"b":[1,"two"]}"#);
    assert_eq!(
        parse_value("data.json", br#"{"n": 1, "f": 1.0, "s": "x"}"#)
            .unwrap()
            .canonical(),
        r#"{"f":1,"n":1,"s":"x"}"#
    );
    assert_eq!(
        parse_value("empty.yaml", b"# nothing\n").unwrap(),
        Value::Null
    );
    let err = parse_value("two.yaml", b"a: 1\n---\nb: 2\n")
        .unwrap_err()
        .to_string();
    assert!(
        err.contains("two.yaml[doc 1]:3:1: expected a single document"),
        "{err}"
    );
    let err = parse_value("dup.yaml", b"a: 1\na: 2\n")
        .unwrap_err()
        .to_string();
    assert!(
        err.contains("dup.yaml:2:1: a: key is already defined at line 1"),
        "{err}"
    );
}

/// The reader refuses a document nested past its limit, and does so on a
/// stack as small as the smallest it runs on (1 MiB: a Windows main thread).
#[test]
fn deep_nesting_is_refused_on_a_small_stack_not_overflowed() {
    for depth in [70usize, 128, 5_000] {
        let flow = format!("{}{}", "[".repeat(depth), "]".repeat(depth));
        let block: String = (0..depth)
            .map(|i| format!("{}a:\n", " ".repeat(i)))
            .collect();
        for text in [flow, block] {
            let error = std::thread::Builder::new()
                .stack_size(1 << 20)
                .spawn(move || {
                    meridian_ir::parse_value("deep.yaml", text.as_bytes())
                        .expect_err("nesting this deep is refused")
                        .to_string()
                })
                .unwrap()
                .join()
                .expect("the reader returned and did not overflow the stack");
            assert!(
                error.contains("nested too deeply") || error.contains("recursion limit"),
                "depth {depth}: {error}"
            );
        }
    }
    // What a real document needs is far inside the limit.
    let text = format!("{}{}", "[".repeat(40), "]".repeat(40));
    assert!(meridian_ir::parse_value("ok.yaml", text.as_bytes()).is_ok());
}
