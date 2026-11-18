//! The server, spoken to the way an editor speaks to it: JSON-RPC messages
//! over a connection, with real plugins loaded.

use std::time::Duration;

use lsp_server::{Connection, Message, Notification, Request, RequestId, Response};
use meridian_lsp::{serve, Plugins};
use meridian_plugin_host::Registry;
use serde_json::{json, Value};

const URI: &str = "file:///work/web.yaml";

const DOC: &str = "\
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
        mode: 0644
      dependsOn:
        - resource: nginx_pkg
";

struct Editor {
    connection: Connection,
    server: Option<std::thread::JoinHandle<()>>,
    next: i32,
}

impl Editor {
    fn start() -> Editor {
        Editor::start_in(None)
    }

    /// Starts the server with a workspace folder, so it reads the tool
    /// schemas cached under it.
    fn start_in(root: Option<&std::path::Path>) -> Editor {
        let (server, client) = Connection::memory();
        let handle = std::thread::spawn(move || {
            let mut registry = Registry::new();
            registry
                .register(Box::new(meridian_target_ansible::Ansible))
                .unwrap();
            registry
                .register(Box::new(meridian_target_terraform::Terraform))
                .unwrap();
            serve(&server, Plugins::Loaded(registry)).expect("the server runs");
        });
        let mut editor = Editor {
            connection: client,
            server: Some(handle),
            next: 0,
        };
        let mut params = json!({ "capabilities": {} });
        if let Some(root) = root {
            let uri = lsp_types::Url::from_directory_path(root).unwrap();
            params["workspaceFolders"] = json!([{ "uri": uri, "name": "project" }]);
        }
        let reply = editor.request("initialize", params);
        assert_eq!(reply["serverInfo"]["name"], "meridian");
        assert_eq!(reply["capabilities"]["hoverProvider"], true);
        editor.notify("initialized", json!({}));
        editor
    }

    fn notify(&self, method: &str, params: Value) {
        self.connection
            .sender
            .send(Message::Notification(Notification::new(
                method.to_owned(),
                params,
            )))
            .unwrap();
    }

    fn receive(&self) -> Message {
        self.connection
            .receiver
            .recv_timeout(Duration::from_secs(20))
            .expect("the server answers")
    }

    /// Sends a request and returns its result, skipping notifications.
    fn request(&mut self, method: &str, params: Value) -> Value {
        self.next += 1;
        let id = RequestId::from(self.next);
        self.connection
            .sender
            .send(Message::Request(Request::new(
                id.clone(),
                method.to_owned(),
                params,
            )))
            .unwrap();
        loop {
            if let Message::Response(Response {
                id: got,
                result,
                error,
            }) = self.receive()
            {
                assert_eq!(got, id);
                assert!(error.is_none(), "{method}: {error:?}");
                return result.unwrap_or(Value::Null);
            }
        }
    }

    /// The next diagnostics the server publishes.
    fn diagnostics(&self) -> Value {
        loop {
            if let Message::Notification(n) = self.receive() {
                if n.method == "textDocument/publishDiagnostics" {
                    return n.params;
                }
            }
        }
    }

    fn open(&self, text: &str) -> Value {
        self.notify(
            "textDocument/didOpen",
            json!({ "textDocument": { "uri": URI, "languageId": "yaml", "version": 1, "text": text } }),
        );
        self.diagnostics()
    }

    fn change(&self, text: &str) {
        self.notify(
            "textDocument/didChange",
            json!({ "textDocument": { "uri": URI, "version": 2 }, "contentChanges": [{ "text": text }] }),
        );
    }

    fn at(line: u32, character: u32) -> Value {
        json!({ "textDocument": { "uri": URI }, "position": { "line": line, "character": character } })
    }

    fn stop(mut self) {
        self.request("shutdown", Value::Null);
        self.notify("exit", Value::Null);
        self.server.take().unwrap().join().unwrap();
    }
}

#[test]
fn an_editing_session() {
    let mut editor = Editor::start();

    // Opening publishes the plugin's refusal of the unquoted mode and the
    // lint about it, which carries the fix.
    let published = editor.open(DOC);
    assert_eq!(published["uri"], URI);
    let found = published["diagnostics"].as_array().unwrap();
    assert_eq!(found.len(), 2, "{found:#?}");
    assert_eq!(found[0]["code"], "invalid-param");
    assert_eq!(found[0]["source"], "ansible");
    assert_eq!(found[0]["severity"], 1);
    assert_eq!(found[1]["code"], "unquoted-mode");
    assert_eq!(found[1]["source"], "meridian-lint");
    assert_eq!(found[1]["severity"], 2);
    assert_eq!(
        found[1]["range"]["start"],
        json!({ "line": 19, "character": 14 })
    );
    let found = &found[1..];

    // The quick fix quotes it.
    let actions = editor.request(
        "textDocument/codeAction",
        json!({
            "textDocument": { "uri": URI },
            "range": found[0]["range"],
            "context": { "diagnostics": [] }
        }),
    );
    assert_eq!(actions[0]["title"], "Quote the mode: \"0644\"");
    assert_eq!(actions[0]["edit"]["changes"][URI][0]["newText"], "\"0644\"");

    // Another compile error replaces them once the text changes.
    editor.change(&DOC.replace("mode: 0644", "mode: \"0644\"\n        colour: blue"));
    let found = editor.diagnostics()["diagnostics"].clone();
    assert_eq!(found.as_array().unwrap().len(), 1, "{found:#?}");
    assert_eq!(found[0]["code"], "unsupported-param");
    assert_eq!(found[0]["source"], "ansible");
    assert_eq!(found[0]["severity"], 1);

    // Fixing the text clears the diagnostics.
    editor.change(&DOC.replace("mode: 0644", "mode: \"0644\""));
    assert_eq!(editor.diagnostics()["diagnostics"], json!([]));

    // Completion: the params `file` still lacks, from the plugin.
    editor.change(&DOC.replace("        mode: 0644\n", "        mode: \"0644\"\n        \n"));
    assert_eq!(editor.diagnostics()["diagnostics"], json!([]));
    let items = editor.request("textDocument/completion", Editor::at(20, 8));
    let labels: Vec<&str> = items
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["label"].as_str().unwrap())
        .collect();
    assert_eq!(labels, ["group", "owner"]);
    assert_eq!(items[0]["textEdit"]["newText"], "group: $0");
    assert_eq!(items[0]["insertTextFormat"], 2);

    // Hover on the type documents it.
    let hover = editor.request("textDocument/hover", Editor::at(15, 14));
    let shown = hover["contents"]["value"].as_str().unwrap();
    assert!(
        shown.contains("resource type of target `ansible`"),
        "{shown}"
    );

    // The dependency leads to the resource it names.
    let target = editor.request("textDocument/definition", Editor::at(22, 24));
    assert_eq!(target["uri"], URI);
    assert_eq!(
        target["range"]["start"],
        json!({ "line": 10, "character": 10 })
    );

    // Renaming the resource renames the dependency with it.
    let prepared = editor.request("textDocument/prepareRename", Editor::at(10, 12));
    assert_eq!(prepared["start"], json!({ "line": 10, "character": 10 }));
    let mut rename = Editor::at(10, 12);
    rename["newName"] = json!("web_pkg");
    let edit = editor.request("textDocument/rename", rename);
    let edits = edit["changes"][URI].as_array().unwrap();
    assert_eq!(edits.len(), 2);
    assert!(edits.iter().all(|e| e["newText"] == "web_pkg"));

    // The outline.
    let symbols = editor.request(
        "textDocument/documentSymbol",
        json!({ "textDocument": { "uri": URI } }),
    );
    assert_eq!(symbols[0]["name"], "web");
    assert_eq!(symbols[0]["children"][1]["name"], "nginx_conf");
    assert_eq!(symbols[0]["children"][1]["detail"], "file");

    // Formatting puts fields in the language's order; a formatted file
    // needs no edit.
    editor.change(
        &DOC.replace(
            "kind: ResourceSet\nmetadata:\n  name: web\n",
            "metadata:\n    name: web\nkind: ResourceSet\n",
        )
        .replace("mode: 0644", "mode: \"0644\""),
    );
    assert_eq!(editor.diagnostics()["diagnostics"], json!([]));
    let format = json!({ "textDocument": { "uri": URI }, "options": { "tabSize": 2, "insertSpaces": true } });
    let edits = editor.request("textDocument/formatting", format.clone());
    assert_eq!(
        edits[0]["newText"],
        DOC.replace("mode: 0644", "mode: \"0644\"")
    );
    editor.change(&DOC.replace("mode: 0644", "mode: \"0644\""));
    assert_eq!(
        editor.request("textDocument/formatting", format),
        Value::Null
    );

    // Closing clears nothing that was not shown, and the server exits
    // cleanly.
    editor.notify(
        "textDocument/didClose",
        json!({ "textDocument": { "uri": URI } }),
    );
    editor.stop();
}

#[test]
fn yaml_that_is_not_meridian_is_left_alone() {
    let mut editor = Editor::start();
    editor.notify(
        "textDocument/didOpen",
        json!({ "textDocument": { "uri": URI, "languageId": "yaml", "version": 1, "text": "services:\n  web:\n    image: nginx\n" } }),
    );
    let items = editor.request("textDocument/completion", Editor::at(1, 2));
    assert_eq!(items, json!([]));
    assert_eq!(
        editor.request("textDocument/hover", Editor::at(0, 2)),
        Value::Null
    );
    let format = json!({ "textDocument": { "uri": URI }, "options": { "tabSize": 2, "insertSpaces": true } });
    assert_eq!(
        editor.request("textDocument/formatting", format),
        Value::Null
    );
    editor.stop();
}

/// A Terraform document that uses a type the plugin knows only by shape.
const TERRAFORM_DOC: &str = "\
apiVersion: meridian/v1
kind: Infrastructure
metadata:
  name: web
spec:
  target: terraform
  resources:
    - id: web
      type: aws_instance
      params:
        ami: ami-123
        colour: blue
";

#[test]
fn cached_tool_schemas_are_read_from_the_workspace() {
    use meridian_sdk::{ParamKind, ParamSchema, ResourceSchema, SchemaExport};
    use meridian_tooling::schemas::{Cached, Store};

    let root = std::path::Path::new(env!("CARGO_TARGET_TMPDIR")).join("protocol-schemas");
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let mut export = SchemaExport::new("terraform", "1.13.1");
    export.pin("hashicorp/aws", "5.60.0");
    export.schemas.push(
        ResourceSchema::closed("aws_instance", "Provides an EC2 instance resource.")
            .param(ParamSchema::new("ami", ParamKind::String, "AMI to use.").required())
            .param(
                ParamSchema::new("instance_type", ParamKind::String, "Instance type.").required(),
            )
            .param(ParamSchema::new("arn", ParamKind::String, "The ARN.").computed()),
    );
    Store::new(&root.join(".meridian"))
        .save(&Cached::from_export("terraform", export))
        .unwrap();

    let mut editor = Editor::start_in(Some(&root));

    // A param the provider's schema does not list, and a required one it
    // does that is missing, are reported without any tool being run.
    let published = editor.open(TERRAFORM_DOC);
    let found = published["diagnostics"].as_array().unwrap().clone();
    let codes: Vec<&str> = found.iter().map(|d| d["code"].as_str().unwrap()).collect();
    assert!(codes.contains(&"unsupported-param"), "{found:#?}");
    assert!(codes.contains(&"missing-param"), "{found:#?}");

    // Completion under params offers what the schema lists and is not set,
    // leaving out what the tool sets.
    editor.change(&TERRAFORM_DOC.replace("        colour: blue\n", "        \n"));
    assert_eq!(
        editor.diagnostics()["diagnostics"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    let items = editor.request("textDocument/completion", Editor::at(11, 8));
    let labels: Vec<&str> = items
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["label"].as_str().unwrap())
        .collect();
    assert_eq!(labels, ["instance_type"]);

    // Hover on the type says where its description came from.
    let hover = editor.request("textDocument/hover", Editor::at(8, 14));
    let text = hover["contents"]["value"].as_str().unwrap();
    assert!(
        text.contains("Provides an EC2 instance resource."),
        "{text}"
    );
    assert!(text.contains("hashicorp/aws 5.60.0"), "{text}");

    editor.stop();
}
