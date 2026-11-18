//! `meridian test`, run as a process against the fake target: a passing
//! compile case, a plan case, a refusal case, failures that name what went
//! wrong, the JSON object, the JUnit file and the test files `lint` leaves
//! alone.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use meridian_plugin_testkit::repo_root;

const SITE: &str = r#"apiVersion: meridian/v1
kind: Infrastructure
metadata:
  name: network
spec:
  target: fake
  resources:
    - id: vm
      type: file
      params:
        content: "${{ data.address }}"
  outputs:
    - name: address
      value: "${vm.content}"
---
apiVersion: meridian/v1
kind: ResourceSet
metadata:
  name: web
  labels:
    tier: web
spec:
  target: fake
  when: data.web
  targetHost:
    from: inline
    host: 203.0.113.10
  resources:
    - id: motd
      type: file
      params:
        content: "welcome\n"
"#;

const BROKEN: &str = r#"apiVersion: meridian/v1
kind: ResourceSet
metadata:
  name: broken
spec:
  target: fake
  targetHost:
    from: inline
    host: 203.0.113.10
  resources:
    - id: svc
      type: service
      params:
        enabled: true
"#;

const PASSING: &str = r#"apiVersion: meridian/v1
kind: Test
metadata:
  name: site
spec:
  documents: [../site.yaml]
  data: [../defaults.data.yaml]
  set:
    web: true
  cases:
    - name: both compile
      expect:
        network:
          outcome: compiled
          files:
            files/vm:
              equals: 10.0.0.5
        web:
          outcome: compiled
          files:
            files/motd:
              contains: [welcome]
              notContains: goodbye
              matches: "^wel"
      assert:
        - length(keys(stages)) == 2
        - stages.network.files["files/vm"] == data.address
        - stages.web.refusal == null
    - name: web is left out
      set: {web: false}
      expect:
        web: {outcome: left-out}
      assert:
        - expr: stages.web.condition == 'data.web'
          message: the condition that left it out is recorded
    - name: only the web tier
      select: tier=web
      assert:
        - keys(stages) == ['web']
    - name: plan creates everything
      run: plan
      expect:
        network:
          outcome: planned
          changes:
            - {action: create, resource: files/vm}
            - {resource: outputs/address}
        web:
          changeCount: 2
      assert:
        - all(map(stages.network.changes, 'c', c.action == 'create'))
        - "!stages.network.unknown"
---
apiVersion: meridian/v1
kind: Test
metadata:
  name: refusals
spec:
  documents: [../broken.yaml]
  cases:
    - name: a service is refused
      expect:
        broken:
          outcome: refused
          rule: unsupported-resource-type
          resource: svc
          message: service
      assert:
        - stages.broken.refusal.rule == 'unsupported-resource-type'
        - stages.broken.files == {}
"#;

const FAILING: &str = r#"apiVersion: meridian/v1
kind: Test
metadata:
  name: wrong
spec:
  documents: [../site.yaml]
  set: {address: 10.0.0.9, web: true}
  cases:
    - name: expects the wrong things
      expect:
        network:
          outcome: refused
          rule: no-such-rule
          files:
            files/vm: {equals: 10.0.0.5, contains: nothing}
            files/missing: {contains: x}
        nowhere: {outcome: compiled}
      assert:
        - expr: stages.web.outcome == 'refused'
          message: web should be refused
        - stages.nowhere.outcome == 'x'
        - stages.web.outcome
    - name: a plan with the wrong count
      run: plan
      expect:
        web:
          changeCount: 5
          changes: [{action: delete}]
    - name: passes all the same
      assert: ["stages.web.outcome == 'compiled'"]
"#;

fn scratch(test: &str) -> PathBuf {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("testing-{test}"));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(dir.join("tests")).unwrap();
    fs::write(dir.join("site.yaml"), SITE).unwrap();
    fs::write(dir.join("broken.yaml"), BROKEN).unwrap();
    fs::write(dir.join("defaults.data.yaml"), "address: 10.0.0.5\n").unwrap();
    dir
}

fn meridian(dir: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_meridian"))
        .env("MERIDIAN_ANNOTATIONS", "off")
        .args(args)
        .current_dir(dir)
        .env("MERIDIAN_PLUGIN_DIR", meridian_target_fake::plugin_dir())
        .env_remove("MERIDIAN_STATE_DIR")
        .output()
        .expect("meridian runs")
}

/// What was printed for a person, with a found file's path written with
/// `/` wherever the test runs, since Windows finds `tests\\site.test.yaml`.
/// JSON is read from the raw bytes instead: its escapes hold backslashes.
fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).replace('\\', "/")
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).replace('\\', "/")
}

fn raw_stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

#[test]
fn passing_cases_pass_and_are_found_under_tests() {
    let dir = scratch("passing");
    fs::write(dir.join("tests/site.test.yaml"), PASSING).unwrap();
    let out = meridian(&dir, &["test"]);
    assert!(out.status.success(), "{}{}", stdout(&out), stderr(&out));
    assert_eq!(
        stdout(&out),
        "tests/site.test.yaml (site)\n\
         \x20 ok    both compile (compile, 2 documents)\n\
         \x20 ok    web is left out (compile, 2 documents)\n\
         \x20 ok    only the web tier (compile, 1 document)\n\
         \x20 ok    plan creates everything (plan, 2 documents)\n\
         tests/site.test.yaml (refusals)\n\
         \x20 ok    a service is refused (compile, 1 document)\n\
         5 cases: 5 passed\n"
    );
    // Each case planned in a scratch directory of its own, and the
    // project's own state was not touched.
    assert!(dir
        .join(".meridian/tests/site/4-plan-creates-everything")
        .is_dir());
    assert!(!dir.join(".meridian/runs").exists());
    assert!(!dir.join(".meridian/state.json").exists());
}

#[test]
fn a_failing_case_says_what_went_wrong_and_the_rest_still_run() {
    let dir = scratch("failing");
    fs::write(dir.join("tests/wrong.test.yaml"), FAILING).unwrap();
    let out = meridian(&dir, &["test", "tests/wrong.test.yaml"]);
    assert_eq!(out.status.code(), Some(1));
    let text = stdout(&out);
    let expected = [
        "  FAIL  expects the wrong things (compile, 2 documents)\n",
        "        - document \"network\": expected outcome refused, was compiled\n",
        "        - document \"network\": expected a refusal under the rule no-such-rule, and the document was compiled\n",
        "        - document \"network\": no file \"files/missing\" was emitted (the files are: data.json, files/vm, outputs/address)\n",
        "        - document \"network\": file \"files/vm\" does not contain \"nothing\"\n",
        "        - document \"network\": file \"files/vm\" is not the text expected; it holds:\n          10.0.0.9\n",
        "        - document \"nowhere\": no such document in this case (the case has: network, web)\n",
        "        - web should be refused (assert stages.web.outcome == 'refused')\n",
        "        - assert stages.nowhere.outcome == 'x': stages.nowhere is not set (the case has no document called that, or the stage has no such field)\n",
        "        - assert stages.web.outcome: ",
        "  FAIL  a plan with the wrong count (plan, 2 documents)\n",
        "        - document \"web\": no change delete; the plan has: create data.json: 33 bytes; create files/motd: 8 bytes\n",
        "        - document \"web\": expected 5 changes, the plan has 2\n",
        "  ok    passes all the same (compile, 2 documents)\n",
        "3 cases: 1 passed, 2 failed\n",
    ];
    for line in expected {
        assert!(text.contains(line), "missing {line:?} in:\n{text}");
    }
}

#[test]
fn case_picks_by_name_and_json_and_junit_describe_the_run() {
    let dir = scratch("json");
    fs::write(dir.join("tests/site.test.yaml"), PASSING).unwrap();
    fs::write(dir.join("tests/wrong.test.yaml"), FAILING).unwrap();
    let out = meridian(
        &dir,
        &[
            "test",
            "--output",
            "json",
            "--case",
            "plan",
            "--junit",
            "report.xml",
        ],
    );
    assert_eq!(out.status.code(), Some(1), "{}", stderr(&out));
    let text = raw_stdout(&out);
    let value: serde_json::Value =
        serde_json::from_str(&text).unwrap_or_else(|e| panic!("{e}:\n{text}"));
    let schema_path = repo_root().join("schemas/output/test.schema.json");
    let schema: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&schema_path).unwrap()).unwrap();
    let validator = jsonschema::draft7::new(&schema).unwrap();
    let problems: Vec<String> = validator
        .iter_errors(&value)
        .map(|e| format!("{e} at {}", e.instance_path()))
        .collect();
    assert!(problems.is_empty(), "{problems:#?}\n{text}");
    assert_eq!(value["ok"], false);
    assert_eq!(value["summary"]["cases"], 2);
    assert_eq!(value["summary"]["passed"], 1);
    assert_eq!(value["summary"]["failed"], 1);
    let files = value["files"].as_array().unwrap();
    assert_eq!(files.len(), 3);
    assert_eq!(files[0]["name"], "site");
    assert_eq!(files[0]["cases"][0]["name"], "plan creates everything");
    assert_eq!(files[0]["cases"][0]["status"], "passed");
    assert_eq!(files[0]["cases"][0]["documents"][0]["outcome"], "planned");
    assert_eq!(files[1]["cases"].as_array().unwrap().len(), 0);
    assert_eq!(files[2]["cases"][0]["status"], "failed");
    assert!(files[2]["cases"][0]["failures"][0]
        .as_str()
        .unwrap()
        .starts_with("document \"web\": no change delete"));
    // The report for people went to standard error.
    assert!(stderr(&out).contains("report: wrote junit to report.xml"));
    let junit = fs::read_to_string(dir.join("report.xml")).unwrap();
    assert!(junit.starts_with("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<testsuites name=\"meridian test\" tests=\"2\" failures=\"1\" errors=\"0\">"), "{junit}");
    assert!(
        junit.contains("<testcase classname=\"site\" name=\"plan creates everything\" time=\""),
        "{junit}"
    );
    assert!(junit.contains("<failure message=\"document &quot;web&quot;: no change delete; the plan has: create data.json: 33 bytes; create files/motd: 8 bytes\">"), "{junit}");
    assert!(junit.contains("</failure>\n    </testcase>"), "{junit}");
}

#[test]
fn a_test_file_with_a_mistake_is_refused_before_anything_runs() {
    let dir = scratch("mistakes");
    let refused = |text: &str| {
        fs::write(dir.join("tests/t.test.yaml"), text).unwrap();
        let out = meridian(&dir, &["test"]);
        assert_eq!(out.status.code(), Some(1));
        assert_eq!(stdout(&out), "");
        stderr(&out)
    };
    let head = "apiVersion: meridian/v1\nkind: Test\nmetadata:\n  name: t\nspec:\n  documents: [../site.yaml]\n";
    let cases = [
        (
            "apiVersion: meridian/v1\nkind: ResourceSet\nmetadata:\n  name: t\nspec: {}\n",
            "error: tests/t.test.yaml: kind: expected \"Test\", which is what meridian test runs\n",
        ),
        (
            &format!("{head}  cases: []\n"),
            "error: tests/t.test.yaml: spec.cases: must hold at least one case\n",
        ),
        (
            &format!("{head}  cases:\n    - name: a\n"),
            "error: tests/t.test.yaml: spec.cases[0]: a case must expect or assert something; a case that checks nothing cannot fail\n",
        ),
        (
            &format!("{head}  cases:\n    - name: a\n      run: apply\n      assert: [\"1 == 1\"]\n"),
            "error: tests/t.test.yaml: spec.cases[0].run: expected \"compile\" or \"plan\", found \"apply\"\n",
        ),
        (
            &format!("{head}  cases:\n    - name: a\n      expect:\n        web: {{outcome: built}}\n"),
            "error: tests/t.test.yaml: spec.cases[0].expect.web.outcome: expected one of compiled, refused, left-out, planned, deferred, failed, found \"built\"\n",
        ),
        (
            &format!("{head}  cases:\n    - name: a\n      assert: [\"param.x == 1\"]\n"),
            "error: tests/t.test.yaml: spec.cases[0].assert[0]: unknown name \"param\" (an assertion can read stages.<document> and data)\n",
        ),
        (
            &format!("{head}  cases:\n    - name: a\n      assert: [\"1 +\"]\n"),
            "error: tests/t.test.yaml: spec.cases[0].assert[0]: ",
        ),
        (
            &format!("{head}  cases:\n    - name: a\n      assert: [\"1 == 1\"]\n    - name: a\n      assert: [\"1 == 1\"]\n"),
            "error: tests/t.test.yaml: spec.cases[1].name: the name \"a\" is used by an earlier case\n",
        ),
        (
            &format!("{head}  cases:\n    - name: a\n      expect:\n        web:\n          files:\n            x: {{matches: \"(\"}}\n"),
            "error: tests/t.test.yaml: spec.cases[0].expect.web.files.x.matches: ",
        ),
        (
            &format!("{head}  cases:\n    - name: a\n      expect:\n        web:\n          changes: [{{}}]\n"),
            "error: tests/t.test.yaml: spec.cases[0].expect.web.changes[0]: a change must name an action, a resource or both\n",
        ),
        (
            &format!("{head}  extra: 1\n  cases:\n    - name: a\n      assert: [\"1 == 1\"]\n"),
            "error: tests/t.test.yaml: spec.extra: unknown field (expected one of: documents, data, set, select, cases)\n",
        ),
    ];
    for (text, message) in cases {
        let err = refused(text);
        assert!(err.starts_with(message), "for:\n{text}\ngot:\n{err}");
    }
}

#[test]
fn a_document_that_does_not_load_is_an_error_of_the_case_not_of_the_command() {
    let dir = scratch("errors");
    fs::write(
        dir.join("tests/t.test.yaml"),
        "apiVersion: meridian/v1\nkind: Test\nmetadata:\n  name: t\nspec:\n  documents: [../nowhere.yaml]\n  cases:\n    - name: a\n      assert: [\"1 == 1\"]\n",
    )
    .unwrap();
    let out = meridian(&dir, &["test", "--output", "json"]);
    assert_eq!(out.status.code(), Some(1));
    let value: serde_json::Value = serde_json::from_str(&raw_stdout(&out)).unwrap();
    assert_eq!(value["ok"], false);
    assert_eq!(value["files"][0]["cases"][0]["status"], "error");
    assert!(value["files"][0]["cases"][0]["failures"][0]
        .as_str()
        .unwrap()
        .starts_with("the documents do not load: "));
    assert_eq!(value["summary"]["errors"], 1);
}

#[test]
fn no_test_files_and_no_matching_case_are_errors() {
    let dir = scratch("none");
    let out = meridian(&dir, &["test"]);
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(
        stderr(&out),
        "error: no test files: name them, or put *.test.yaml files under tests/\n"
    );
    fs::write(dir.join("tests/site.test.yaml"), PASSING).unwrap();
    let out = meridian(&dir, &["test", "--case", "nothing-is-called-this"]);
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(
        stderr(&out),
        "error: no case's name holds \"nothing-is-called-this\"\n"
    );
}

#[test]
fn lint_and_fmt_over_a_directory_leave_test_files_to_meridian_test() {
    let dir = scratch("lint");
    fs::write(dir.join("tests/site.test.yaml"), PASSING).unwrap();
    let out = meridian(&dir, &["lint", "tests"]);
    assert!(out.status.success(), "{}{}", stdout(&out), stderr(&out));
    let out = meridian(&dir, &["fmt", "--check", "tests"]);
    assert!(out.status.success(), "{}{}", stdout(&out), stderr(&out));
    assert!(!stdout(&out).contains("site.test.yaml"), "{}", stdout(&out));
}
