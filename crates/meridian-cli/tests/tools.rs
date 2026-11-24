//! `meridian fmt`, `lint`, `schema` and `lsp`, run as a process against
//! real plugin binaries.

#![cfg(unix)]

use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use meridian_plugin_testkit::{plugin_binary, repo_root};
use serde_json::{json, Value};

const WEB: &str = "\
apiVersion: meridian/v1
kind: ResourceSet
metadata:
  name: web
spec:
  target: ansible
  targetHost:
    from: inline
    host: web01
  resources:
    - id: nginx_pkg
      type: package
      params:
        name: nginx
    - id: nginx_conf
      type: file
      params:
        path: /etc/nginx.conf
        content: x
        mode: \"0644\"
      dependsOn:
        - resource: nginx_pkg
";

fn scratch(test: &str) -> PathBuf {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR"))
        .join("tools")
        .join(test);
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

fn plugin_dir() -> PathBuf {
    plugin_binary("meridian-target-ansible")
        .parent()
        .unwrap()
        .to_path_buf()
}

fn meridian(dir: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_meridian"))
        .env("MERIDIAN_ANNOTATIONS", "off")
        .args(args)
        .current_dir(dir)
        .env("MERIDIAN_PLUGIN_DIR", plugin_dir())
        .output()
        .expect("meridian runs")
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

#[test]
fn fmt_rewrites_files_and_check_only_reports() {
    let dir = scratch("fmt");
    let messy = WEB
        .replace(
            "kind: ResourceSet\nmetadata:\n  name: web\n",
            "metadata:\n    name: web\nkind: ResourceSet\n",
        )
        .replace(
            "    - id: nginx_pkg\n      type: package\n",
            "    - type: package\n      id: nginx_pkg\n",
        );
    fs::write(dir.join("web.yaml"), &messy).unwrap();
    fs::write(dir.join("tidy.yaml"), WEB).unwrap();
    // Other YAML in the directory is nobody's Meridian document.
    fs::write(
        dir.join("compose.yaml"),
        "services:\n    web: {image: nginx}\n",
    )
    .unwrap();

    let check = meridian(&dir, &["fmt", "--check", "."]);
    assert!(!check.status.success());
    assert_eq!(stdout(&check), "./web.yaml\n");
    assert!(
        stderr(&check).contains("1 file not formatted"),
        "{}",
        stderr(&check)
    );
    assert_eq!(fs::read_to_string(dir.join("web.yaml")).unwrap(), messy);

    let run = meridian(&dir, &["fmt", "."]);
    assert!(run.status.success(), "{}", stderr(&run));
    assert_eq!(
        stdout(&run),
        "formatted ./web.yaml\n1 file formatted, 1 already formatted\n"
    );
    assert_eq!(fs::read_to_string(dir.join("web.yaml")).unwrap(), WEB);
    assert_eq!(
        fs::read_to_string(dir.join("compose.yaml")).unwrap(),
        "services:\n    web: {image: nginx}\n"
    );
    assert!(meridian(&dir, &["fmt", "--check", "."]).status.success());

    // --keep-order fixes the layout and leaves the fields where they are.
    fs::write(dir.join("web.yaml"), &messy).unwrap();
    assert!(meridian(&dir, &["fmt", "--keep-order", "web.yaml"])
        .status
        .success());
    let kept = fs::read_to_string(dir.join("web.yaml")).unwrap();
    assert!(
        kept.starts_with("apiVersion: meridian/v1\nmetadata:\n  name: web\nkind: ResourceSet\n"),
        "{kept}"
    );
}

#[test]
fn fmt_reads_standard_input_and_refuses_what_does_not_parse() {
    let dir = scratch("fmt-stdin");
    let mut child = Command::new(env!("CARGO_BIN_EXE_meridian"))
        .env("MERIDIAN_ANNOTATIONS", "off")
        .args(["fmt", "-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(b"kind: ResourceSet\napiVersion:   meridian/v1\n")
        .unwrap();
    let out = child.wait_with_output().unwrap();
    assert_eq!(stdout(&out), "apiVersion: meridian/v1\nkind: ResourceSet\n");

    fs::write(
        dir.join("broken.yaml"),
        "apiVersion: meridian/v1\nspec: [1, 2\n",
    )
    .unwrap();
    let out = meridian(&dir, &["fmt", "broken.yaml"]);
    assert!(!out.status.success());
    assert!(stderr(&out).contains("does not parse"), "{}", stderr(&out));
    assert_eq!(
        fs::read_to_string(dir.join("broken.yaml")).unwrap(),
        "apiVersion: meridian/v1\nspec: [1, 2\n"
    );
}

#[test]
fn lint_reports_what_the_compiler_the_plugin_and_the_rules_say() {
    let dir = scratch("lint");
    fs::write(dir.join("web.yaml"), WEB).unwrap();
    let clean = meridian(&dir, &["lint", "web.yaml"]);
    assert!(
        clean.status.success(),
        "{}{}",
        stdout(&clean),
        stderr(&clean)
    );
    assert_eq!(
        stdout(&clean),
        "1 document in 1 file checked, nothing to report\n"
    );

    let bad = WEB
        .replace("mode: \"0644\"", "mode: 0644\n        colour: blue")
        .replace(
            "- resource: nginx_pkg",
            "- resource: nginx_pkg\n        - resource: nginx_pkg",
        );
    fs::write(dir.join("web.yaml"), &bad).unwrap();
    let out = meridian(&dir, &["lint", "web.yaml"]);
    assert!(!out.status.success());
    assert_eq!(
        stdout(&out),
        "web.yaml:20:9: error: param \"mode\" of a file must be text, not a number; write it quoted, so it is not read as a number: \"420\" [invalid-param]\n\
         web.yaml:20:15: warning: mode 0644 is read as a number, not as permissions; write it as a quoted string [unquoted-mode]\n\
         web.yaml:21:9: error: resource type \"file\" does not take param \"colour\" on target \"ansible\" (it takes: content, group, mode, owner, path, target) [unsupported-param]\n\
         web.yaml:24:21: info: \"nginx_pkg\" is already listed [redundant-dependency]\n\
         2 errors, 1 warning, 1 note\n"
    );

    // One JSON object per line for tools. The plugin's refusal comes first,
    // then the lint rule that explains the same mistake.
    let out = meridian(&dir, &["lint", "--json", "web.yaml"]);
    let text = stdout(&out);
    let mut lines = text
        .lines()
        .map(|l| serde_json::from_str::<Value>(l).unwrap());
    let first = lines.next().unwrap();
    assert_eq!(first["code"], "invalid-param");
    let second = lines.next().unwrap();
    assert_eq!(second["code"], "unquoted-mode");
    assert_eq!(
        (second["line"].as_u64(), second["column"].as_u64()),
        (Some(20), Some(15))
    );
    assert_eq!(second["fixable"], true);

    // --fix applies what is safe; the error is still the author's to fix.
    let out = meridian(&dir, &["lint", "--fix", "web.yaml"]);
    assert!(!out.status.success());
    assert!(stdout(&out).contains("fixed 1 finding"), "{}", stdout(&out));
    assert!(!stdout(&out).contains("unquoted-mode"));
    assert!(fs::read_to_string(dir.join("web.yaml"))
        .unwrap()
        .contains("mode: \"0644\""));

    // Warnings fail only when asked to. Two packages that manage the same
    // thing compile, with a warning.
    fs::write(
        dir.join("web.yaml"),
        WEB.replace(
            "    - id: nginx_conf",
            "    - id: nginx_again\n      type: package\n      params:\n        name: nginx\n    - id: nginx_conf",
        ),
    )
    .unwrap();
    assert!(meridian(&dir, &["lint", "web.yaml"]).status.success());
    assert!(!meridian(&dir, &["lint", "--strict", "web.yaml"])
        .status
        .success());
}

#[test]
fn lint_checks_each_file_of_a_directory_on_its_own() {
    // Two files that both name a document `web` are two workflows, not a
    // clash.
    let dir = scratch("lint-dir");
    fs::create_dir_all(dir.join("a")).unwrap();
    fs::create_dir_all(dir.join("b")).unwrap();
    fs::write(dir.join("a/web.yaml"), WEB).unwrap();
    fs::write(dir.join("b/web.yaml"), WEB).unwrap();
    let out = meridian(&dir, &["lint", "."]);
    assert!(out.status.success(), "{}", stdout(&out));
    assert_eq!(
        stdout(&out),
        "2 documents in 2 files checked, nothing to report\n"
    );
}

#[test]
fn every_example_passes_lint_with_the_real_plugins() {
    let mut command = Command::new(env!("CARGO_BIN_EXE_meridian"));
    command.env("MERIDIAN_ANNOTATIONS", "off");
    command.arg("lint").arg(repo_root().join("examples"));
    // Every plugin, each from wherever it was built.
    for entry in fs::read_dir(repo_root().join("plugins")).unwrap() {
        let name = entry.unwrap().file_name().to_string_lossy().into_owned();
        let binary = plugin_binary(&format!("meridian-target-{name}"));
        command.arg("--plugin-dir").arg(binary.parent().unwrap());
    }
    let out = command.output().unwrap();
    let text = stdout(&out);
    assert!(out.status.success(), "{text}{}", stderr(&out));
    assert!(!text.contains(": error:"), "{text}");
    assert!(!text.contains("no target plugins"), "{text}");
}

#[test]
fn schema_is_generated_from_the_language_and_the_loaded_plugins() {
    let dir = scratch("schema");
    let out = meridian(&dir, &["schema", "--target", "ansible"]);
    assert!(out.status.success(), "{}", stderr(&out));
    let schema: Value = serde_json::from_str(&stdout(&out)).unwrap();
    assert_eq!(schema["$schema"], "http://json-schema.org/draft-07/schema#");
    let resource = &schema["definitions"]["Resource.ansible"];
    assert_eq!(
        resource["allOf"][1]["properties"]["type"]["anyOf"][0]["enum"],
        json!([
            "cron",
            "exec",
            "file",
            "group",
            "package",
            "packageRepository",
            "service",
            "user"
        ])
    );
    assert!(schema["definitions"].get("Resource.terraform").is_none());

    let out = meridian(&dir, &["schema", "--target", "nope"]);
    assert!(!out.status.success());
    assert!(
        stderr(&out).contains("no plugin provides target \"nope\""),
        "{}",
        stderr(&out)
    );

    let out = meridian(&dir, &["schema", "-o", "meridian.schema.json"]);
    assert!(out.status.success());
    assert!(stdout(&out).is_empty());
    let written: Value =
        serde_json::from_str(&fs::read_to_string(dir.join("meridian.schema.json")).unwrap())
            .unwrap();
    assert!(written["definitions"].get("Resource.ansible").is_some());
}

/// Every plugin, compiled in, as a catalog.
fn every_plugin() -> meridian_tooling::catalog::Catalog {
    let mut registry = meridian_plugin_host::Registry::new();
    for emitter in meridian_all_targets::emitters() {
        registry.register(emitter).unwrap();
    }
    meridian_tooling::catalog::Catalog::from_registry(&registry)
}

fn example_documents() -> Vec<(String, Value)> {
    let mut found = Vec::new();
    let mut dirs: Vec<PathBuf> = fs::read_dir(repo_root().join("examples"))
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect();
    dirs.sort();
    for dir in dirs.into_iter().filter(|d| d.is_dir()) {
        for file in fs::read_dir(&dir).unwrap() {
            let file = file.unwrap().path();
            if file.extension().is_none_or(|e| e != "yaml") {
                continue;
            }
            let name = file.to_string_lossy().into_owned();
            let text = fs::read(&file).unwrap();
            // A data file for `--data` sits beside some examples; it is
            // YAML, and not a Meridian document.
            if !meridian_tooling::check::is_meridian(&String::from_utf8_lossy(&text)) {
                continue;
            }
            for (i, doc) in meridian_ir::parse_stream(&name, &text)
                .unwrap()
                .iter()
                .enumerate()
            {
                let json: Value = serde_json::from_str(&doc.canonical()).unwrap();
                found.push((format!("{name}[{i}]"), json));
            }
        }
    }
    found
}

#[test]
fn every_example_is_valid_against_the_exported_schema() {
    let schema = meridian_tooling::jsonschema::export(&every_plugin(), None);
    let validator = jsonschema::draft7::new(&schema).expect("the export is a valid schema");
    let documents = example_documents();
    assert!(documents.len() > 20);
    for (name, document) in &documents {
        let problems: Vec<String> = validator
            .iter_errors(document)
            .map(|e| format!("{} at {}", e, e.instance_path()))
            .collect();
        assert!(problems.is_empty(), "{name}: {problems:#?}");
    }
}

#[test]
fn the_exported_schema_refuses_what_the_compiler_refuses() {
    let schema = meridian_tooling::jsonschema::export(&every_plugin(), None);
    let validator = jsonschema::draft7::new(&schema).unwrap();
    let good: Value = serde_json::from_str(
        &meridian_ir::parse_stream("web.yaml", WEB.as_bytes()).unwrap()[0].canonical(),
    )
    .unwrap();
    assert!(validator.is_valid(&good));

    let broken = |change: &dyn Fn(&mut Value)| {
        let mut doc = good.clone();
        change(&mut doc);
        !validator.is_valid(&doc)
    };
    // The language: an unknown field, a missing one, a kind that is not one.
    assert!(broken(
        &|d| d["spec"]["resources"][0]["stat"] = json!("present")
    ));
    assert!(broken(&|d| {
        d["spec"]["resources"][0]
            .as_object_mut()
            .unwrap()
            .remove("id");
    }));
    assert!(broken(&|d| d["kind"] = json!("Nonsense")));
    assert!(broken(&|d| d["spec"]["outputs"] = json!([])));
    // The target: a type, a param and a state it does not take.
    assert!(broken(&|d| d["spec"]["target"] = json!("ansibel")));
    assert!(broken(
        &|d| d["spec"]["resources"][0]["type"] = json!("cron")
    ));
    assert!(broken(
        &|d| d["spec"]["resources"][0]["params"]["colour"] = json!("blue")
    ));
    assert!(broken(
        &|d| d["spec"]["resources"][0]["state"] = json!("restarted")
    ));
    assert!(broken(&|d| {
        d["spec"]["resources"][0]["params"]
            .as_object_mut()
            .unwrap()
            .remove("name");
    }));
}

/// A language client over a child process's standard input and output.
struct Client {
    child: std::process::Child,
    input: std::process::ChildStdin,
    output: BufReader<std::process::ChildStdout>,
}

impl Client {
    fn send(&mut self, message: Value) {
        let body = message.to_string();
        write!(self.input, "Content-Length: {}\r\n\r\n{body}", body.len()).unwrap();
        self.input.flush().unwrap();
    }

    fn receive(&mut self) -> Value {
        let mut length = 0;
        loop {
            let mut header = String::new();
            assert!(
                self.output.read_line(&mut header).unwrap() > 0,
                "the server closed"
            );
            let header = header.trim();
            if header.is_empty() {
                break;
            }
            if let Some(value) = header.strip_prefix("Content-Length:") {
                length = value.trim().parse().unwrap();
            }
        }
        let mut body = vec![0; length];
        self.output.read_exact(&mut body).unwrap();
        serde_json::from_slice(&body).unwrap()
    }

    /// The next message for which `wanted` holds.
    fn until(&mut self, wanted: impl Fn(&Value) -> bool) -> Value {
        loop {
            let message = self.receive();
            if wanted(&message) {
                return message;
            }
        }
    }
}

#[test]
fn lsp_serves_an_editor_over_standard_io_with_a_real_plugin() {
    let mut child = Command::new(env!("CARGO_BIN_EXE_meridian"))
        .env("MERIDIAN_ANNOTATIONS", "off")
        .args(["lsp", "--plugin-dir"])
        .arg(plugin_dir())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .unwrap();
    let mut client = Client {
        input: child.stdin.take().unwrap(),
        output: BufReader::new(child.stdout.take().unwrap()),
        child,
    };
    client.send(json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize", "params": { "capabilities": {} } }));
    let reply = client.until(|m| m["id"] == 1);
    assert_eq!(reply["result"]["serverInfo"]["name"], "meridian");
    client.send(json!({ "jsonrpc": "2.0", "method": "initialized", "params": {} }));

    let uri = "file:///work/web.yaml";
    let text = WEB.replace("type: package", "type: pakage");
    client.send(json!({
        "jsonrpc": "2.0",
        "method": "textDocument/didOpen",
        "params": { "textDocument": { "uri": uri, "languageId": "yaml", "version": 1, "text": text } }
    }));
    let published = client.until(|m| m["method"] == "textDocument/publishDiagnostics");
    let found = &published["params"]["diagnostics"];
    assert_eq!(found.as_array().unwrap().len(), 1, "{found:#}");
    assert_eq!(found[0]["code"], "unsupported-resource-type");
    assert_eq!(found[0]["source"], "ansible");

    // The types on offer come from the plugin process.
    client.send(json!({
        "jsonrpc": "2.0", "id": 2, "method": "textDocument/completion",
        "params": { "textDocument": { "uri": uri }, "position": { "line": 11, "character": 14 } }
    }));
    let reply = client.until(|m| m["id"] == 2);
    let labels: Vec<&str> = reply["result"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["label"].as_str().unwrap())
        .collect();
    assert_eq!(
        labels,
        [
            "<namespace>.<collection>.<module>",
            "cron",
            "exec",
            "file",
            "group",
            "package",
            "packageRepository",
            "service",
            "user"
        ]
    );

    client.send(json!({ "jsonrpc": "2.0", "id": 3, "method": "shutdown", "params": null }));
    client.until(|m| m["id"] == 3);
    client.send(json!({ "jsonrpc": "2.0", "method": "exit", "params": null }));
    assert!(client.child.wait().unwrap().success());
}

/// A copy of the `meridian` binary in an empty directory of its own, so the
/// directory beside the binary, which the server also searches, holds no
/// plugins; and a copy of the Ansible plugin in a second directory, which
/// is the one each test points the server at.
fn isolated(test: &str) -> (PathBuf, PathBuf) {
    let dir = scratch(test);
    let exe = Path::new(env!("CARGO_BIN_EXE_meridian"));
    let bin = dir.join("bin");
    fs::create_dir_all(&bin).unwrap();
    let meridian = bin.join(exe.file_name().unwrap());
    fs::copy(exe, &meridian).unwrap();
    let plugins = dir.join("plugins");
    fs::create_dir_all(&plugins).unwrap();
    let built = plugin_binary("meridian-target-ansible");
    fs::copy(&built, plugins.join(built.file_name().unwrap())).unwrap();
    (meridian, plugins)
}

/// Starts `meridian lsp` from `exe` with `args`, initializes it with
/// `options`, and returns the client and every `window/showMessage` the
/// server sent while starting.
fn lsp(exe: &Path, args: &[&str], options: Value) -> (Client, Vec<String>) {
    let mut child = Command::new(exe)
        .arg("lsp")
        .args(args)
        .env_remove("MERIDIAN_PLUGIN_DIR")
        .env_remove("MERIDIAN_PLUGIN_SIGNATURES")
        .env_remove("MERIDIAN_TRUSTED_KEYS")
        .env_remove("MERIDIAN_LSP_TRUST_EDITOR_PLUGIN_DIRS")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .unwrap();
    let mut client = Client {
        input: child.stdin.take().unwrap(),
        output: BufReader::new(child.stdout.take().unwrap()),
        child,
    };
    client.send(json!({
        "jsonrpc": "2.0", "id": 1, "method": "initialize",
        "params": { "capabilities": {}, "initializationOptions": options }
    }));
    let reply = client.until(|m| m["id"] == 1);
    assert_eq!(reply["result"]["serverInfo"]["name"], "meridian");
    client.send(json!({ "jsonrpc": "2.0", "method": "initialized", "params": {} }));
    // Opening a document makes the server publish diagnostics, which come
    // after every message it showed while starting. The document has a
    // resource type only the plugin can find fault with.
    let text = WEB.replace("type: package", "type: pakage");
    client.send(json!({
        "jsonrpc": "2.0",
        "method": "textDocument/didOpen",
        "params": { "textDocument": { "uri": "file:///work/web.yaml", "languageId": "yaml", "version": 1, "text": text } }
    }));
    let mut shown = Vec::new();
    loop {
        let message = client.receive();
        if message["method"] == "window/showMessage" {
            shown.push(message["params"]["message"].as_str().unwrap().to_owned());
        } else if message["method"] == "textDocument/publishDiagnostics" {
            let codes: Vec<&str> = message["params"]["diagnostics"]
                .as_array()
                .unwrap()
                .iter()
                .filter_map(|d| d["code"].as_str())
                .collect();
            shown.push(format!("diagnostics: {}", codes.join(" ")));
            return (client, shown);
        }
    }
}

/// Whether the Ansible target was loaded: only the plugin knows that
/// `pakage` is not one of its resource types.
fn loaded_ansible(shown: &[String]) -> bool {
    let diagnostics = shown.last().unwrap();
    assert!(diagnostics.starts_with("diagnostics:"), "{shown:?}");
    diagnostics.contains("unsupported-resource-type")
}

fn stop(mut client: Client) {
    client.send(json!({ "jsonrpc": "2.0", "id": 9, "method": "shutdown", "params": null }));
    client.until(|m| m["id"] == 9);
    client.send(json!({ "jsonrpc": "2.0", "method": "exit", "params": null }));
    assert!(client.child.wait().unwrap().success());
}

fn keygen(exe: &Path, dir: &Path) -> (PathBuf, PathBuf) {
    let secret = dir.join("test.key");
    let public = dir.join("test.pub");
    let made = Command::new(exe)
        .args(["signing", "keygen", "--secret"])
        .arg(&secret)
        .arg("--public")
        .arg(&public)
        .output()
        .unwrap();
    assert!(made.status.success(), "{}", stderr(&made));
    (secret, public)
}

fn sign(exe: &Path, secret: &Path, plugins: &Path) {
    let binary = plugins.join(
        plugin_binary("meridian-target-ansible")
            .file_name()
            .unwrap(),
    );
    let signed = Command::new(exe)
        .args(["signing", "sign", "--key"])
        .arg(secret)
        .arg(&binary)
        .output()
        .unwrap();
    assert!(signed.status.success(), "{}", stderr(&signed));
}

/// `meridian lsp` checks plugin signatures the way `lint` does: the same
/// `--plugin-signatures` policy, the same trusted keys, and a plugin that
/// does not pass is reported to the editor instead of started.
#[test]
fn lsp_checks_plugin_signatures_under_the_same_policy_as_lint() {
    let (exe, plugins) = isolated("lsp_signatures");
    let dir = plugins.to_str().unwrap();

    let (client, shown) = lsp(
        &exe,
        &["--plugin-dir", dir, "--plugin-signatures", "require"],
        json!({}),
    );
    assert!(!loaded_ansible(&shown), "{shown:?}");
    assert!(shown.iter().any(|m| m.contains("not signed")), "{shown:?}");
    stop(client);

    let (client, shown) = lsp(
        &exe,
        &["--plugin-dir", dir, "--plugin-signatures", "warn"],
        json!({}),
    );
    assert!(loaded_ansible(&shown), "{shown:?}");
    assert!(shown.iter().any(|m| m.contains("not signed")), "{shown:?}");
    stop(client);

    let (secret, public) = keygen(&exe, plugins.parent().unwrap());
    sign(&exe, &secret, &plugins);
    let (client, shown) = lsp(
        &exe,
        &[
            "--plugin-dir",
            dir,
            "--plugin-signatures",
            "require",
            "--trusted-key",
            public.to_str().unwrap(),
        ],
        json!({}),
    );
    assert!(loaded_ansible(&shown), "{shown:?}");
    assert!(!shown.iter().any(|m| m.contains("signed")), "{shown:?}");
    stop(client);
}

/// A plugin directory the editor names in its settings is not the user's
/// own command line, so plugins in it must be signed by a trusted key,
/// whatever the policy, unless the server was started trusting the editor.
#[test]
fn lsp_loads_plugins_from_editor_directories_only_when_signed_or_trusted() {
    let (exe, plugins) = isolated("lsp_editor_dirs");
    let options = json!({ "pluginDirs": [plugins.to_str().unwrap()] });

    let (client, shown) = lsp(&exe, &[], options.clone());
    assert!(!loaded_ansible(&shown), "{shown:?}");
    assert!(
        shown
            .iter()
            .any(|m| m.contains("not signed") && m.contains("--trust-editor-plugin-dirs")),
        "{shown:?}"
    );
    stop(client);

    let (client, shown) = lsp(&exe, &["--plugin-signatures", "off"], options.clone());
    assert!(!loaded_ansible(&shown), "{shown:?}");
    stop(client);

    let (client, shown) = lsp(&exe, &["--trust-editor-plugin-dirs"], options.clone());
    assert!(loaded_ansible(&shown), "{shown:?}");
    stop(client);

    let (secret, public) = keygen(&exe, plugins.parent().unwrap());
    sign(&exe, &secret, &plugins);
    let (client, shown) = lsp(
        &exe,
        &["--trusted-key", public.to_str().unwrap()],
        options.clone(),
    );
    assert!(loaded_ansible(&shown), "{shown:?}");
    assert!(!shown.iter().any(|m| m.contains("signed")), "{shown:?}");
    stop(client);

    // A key the server was not told to trust is no better than no signature.
    let (client, shown) = lsp(&exe, &[], options);
    assert!(!loaded_ansible(&shown), "{shown:?}");
    assert!(
        shown
            .iter()
            .any(|m| m.contains("--trust-editor-plugin-dirs")),
        "{shown:?}"
    );
    stop(client);
}

/// The schema checked in under `schemas/` is the one `meridian schema`
/// writes with every plugin loaded, so an editor can be pointed at the file
/// on GitHub. Record it again with `UPDATE_GOLDEN=1` after the language or a
/// plugin's resource types change on purpose.
#[test]
fn the_published_schema_is_current() {
    let path = repo_root().join("schemas/meridian-v1.schema.json");
    let schema = meridian_tooling::jsonschema::export(&every_plugin(), None);
    let mut expected = serde_json::to_string_pretty(&schema).unwrap();
    expected.push('\n');
    if std::env::var_os("UPDATE_GOLDEN").is_some() {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, &expected).unwrap();
    }
    let found = fs::read_to_string(&path).unwrap_or_default();
    assert!(
        found == expected,
        "schemas/meridian-v1.schema.json is out of date; run \
         `UPDATE_GOLDEN=1 cargo test -p meridian-cli --test tools the_published_schema_is_current`"
    );
}

#[test]
fn fmt_leaves_yaml_that_is_not_meridian_alone() {
    let dir = scratch("fmt-foreign");
    let foreign = "on:\n    push:\n        branches: [main]\n";
    fs::write(dir.join("workflow.yml"), foreign).unwrap();
    let out = meridian(&dir, &["fmt", "workflow.yml"]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(stderr(&out).contains("skipped workflow.yml: not a Meridian document"));
    assert_eq!(
        fs::read_to_string(dir.join("workflow.yml")).unwrap(),
        foreign
    );
    let out = meridian(&dir, &["fmt", "--check", "workflow.yml"]);
    assert!(out.status.success(), "{}", stderr(&out));
}

#[test]
fn fmt_and_lint_read_a_file_that_starts_with_a_byte_order_mark() {
    let dir = scratch("fmt-bom");
    let marked = format!(
        "\u{feff}{}",
        WEB.replace("\n  resources:", "\n  resources:   ")
    );
    fs::write(dir.join("web.yaml"), &marked).unwrap();
    let out = meridian(&dir, &["fmt", "--check", "web.yaml"]);
    assert_eq!(out.status.code(), Some(1), "{}", stderr(&out));
    assert_eq!(stdout(&out), "web.yaml\n");
    assert!(!stderr(&out).contains("skipped"), "{}", stderr(&out));
    // One standard layout: the mark goes with the rest of the layout.
    let out = meridian(&dir, &["fmt", "web.yaml"]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(fs::read_to_string(dir.join("web.yaml")).unwrap(), WEB);

    fs::write(dir.join("web.yaml"), format!("\u{feff}{WEB}")).unwrap();
    let out = meridian(&dir, &["lint", "web.yaml"]);
    assert!(out.status.success(), "{}{}", stdout(&out), stderr(&out));
    assert_eq!(
        stdout(&out),
        "1 document in 1 file checked, nothing to report\n"
    );
}

/// The examples are what people copy, so they are kept in the standard
/// layout: `meridian fmt --check examples` passes.
#[test]
fn every_example_is_formatted() {
    let root = repo_root();
    let out = meridian(&root, &["fmt", "--check", "examples"]);
    assert!(
        out.status.success(),
        "run `meridian fmt examples`:\n{}{}",
        stdout(&out),
        stderr(&out)
    );
}
