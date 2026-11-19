//! The contract test: the host driving every shipped plugin's real binary.
//!
//! A document is read from YAML, validated, built into a tree and compiled by
//! each target in its own process, and the bytes that come back are compared
//! with the golden files in `plugins/<target>/testdata`. Each plugin's own
//! tests make the same comparison in process; this one proves nothing is lost
//! or reworded on the way across the boundary.

// A refusal is passed around by value here exactly as the host returns it.
#![allow(clippy::result_large_err)]

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use meridian_ast::ResourceGraph;
use meridian_ir::{Bundle, Document};
use meridian_plugin_host::{
    binary_name, to_sdk, Client, EmitError, Registry, StartOptions, BINARY_PREFIX,
};
use meridian_sdk::{
    Capabilities, Category, CompileError, Data, Operations, Position, PROTOCOL_VERSION, VERSION,
};

struct Target {
    name: &'static str,
    /// The fixtures the target refuses, each with a `.refused` golden file.
    refused: &'static [&'static str],
    resource_types: &'static [&'static str],
    capabilities: Capabilities,
}

const TARGETS: &[Target] = &[
    Target {
        name: "ansible",
        refused: &[],
        resource_types: &[
            "<namespace>.<collection>.<module>",
            "exec",
            "file",
            "group",
            "package",
            "packageRepository",
            "service",
            "user",
        ],
        capabilities: Capabilities {
            native_notify: true,
            synthetic_notify: false,
            runtime_condition: true,
            secrets: true,
            credentials: true,
            cloud_credentials: false,
            adopt: meridian_sdk::Adoption::Converge,
        },
    },
    Target {
        name: "puppet",
        refused: &["web_stack", "runtime_condition"],
        resource_types: &[
            "<module>::<type>",
            "<type>",
            "exec",
            "file",
            "group",
            "package",
            "packageRepository",
            "service",
            "user",
        ],
        capabilities: Capabilities {
            native_notify: true,
            synthetic_notify: false,
            runtime_condition: false,
            secrets: true,
            credentials: true,
            cloud_credentials: false,
            adopt: meridian_sdk::Adoption::Converge,
        },
    },
];

const FIXTURES: &[&str] = &[
    "accounts",
    "restart_chain",
    "runtime_condition",
    "web_stack",
];

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("the repository root exists")
}

/// The directory holding the built plugin binaries. They are built once per
/// test run, whichever test asks first.
fn plugin_dir() -> &'static Path {
    static DIR: OnceLock<PathBuf> = OnceLock::new();
    DIR.get_or_init(|| {
        let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join("shipped-plugins");
        fs::create_dir_all(&dir).expect("creating the plugin directory");
        for target in TARGETS {
            let binary = binary_name(target.name);
            let built =
                meridian_plugin_testkit::plugin_binary(&format!("{BINARY_PREFIX}{}", target.name));
            // Copied under another name and renamed, so a test running in
            // parallel never executes a half-written file.
            let staged = dir.join(format!("{binary}.staged"));
            fs::copy(&built, &staged).expect("copying a plugin binary");
            fs::rename(&staged, dir.join(&binary)).expect("placing a plugin binary");
        }
        dir
    })
}

fn start(target: &Target) -> Client {
    let path = plugin_dir().join(binary_name(target.name));
    Client::start(path.to_str().unwrap(), &StartOptions::default())
        .unwrap_or_else(|e| panic!("starting {}: {e}", target.name))
}

/// Reads a fixture the way the compiler will: parse, validate, resolve the
/// host, build the tree, hand it to the target.
fn load(fixture: &str, target: &str) -> meridian_sdk::ResourceGraph {
    let path = repo_root().join("fixtures").join(format!("{fixture}.yaml"));
    let shown = format!("fixtures/{fixture}.yaml");
    let data = fs::read(&path).unwrap_or_else(|e| panic!("reading {}: {e}", path.display()));
    let mut documents =
        meridian_ir::parse(&shown, &data).unwrap_or_else(|e| panic!("parsing {shown}: {e}"));
    for document in &mut documents {
        if let Document::ResourceSet(rs) = document {
            rs.spec.target = target.to_owned();
        }
    }
    let bundle = Bundle::new(documents);
    bundle
        .validate()
        .unwrap_or_else(|e| panic!("validating {shown}: {e}"));
    let rs = bundle
        .resource_set("web")
        .expect("the fixture is named web");
    let host = &rs.spec.target_host.as_ref().expect("an inline host").host;
    let graph = ResourceGraph::build(rs, host).unwrap_or_else(|e| panic!("building {shown}: {e}"));
    to_sdk(&graph)
}

/// The golden files were recorded from trees built in memory, which carry no
/// source positions, so a refusal in them has no `file:line` prefix.
fn without_positions(mut graph: meridian_sdk::ResourceGraph) -> meridian_sdk::ResourceGraph {
    for resource in &mut graph.resources {
        resource.pos = Position::default();
    }
    for edge in &mut graph.edges {
        edge.pos = Position::default();
    }
    graph
}

fn golden(target: &str, file: &str) -> String {
    let path = repo_root()
        .join("plugins")
        .join(target)
        .join("testdata")
        .join(file);
    fs::read_to_string(&path).unwrap_or_else(|e| panic!("reading {}: {e}", path.display()))
}

/// The golden files for one fixture and target, keyed by file name.
fn goldens_for(target: &str, fixture: &str) -> BTreeMap<String, String> {
    let dir = repo_root().join("plugins").join(target).join("testdata");
    let prefix = format!("{fixture}.");
    fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("listing {}: {e}", dir.display()))
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .filter(|name| name.starts_with(&prefix))
        .map(|name| {
            let text = golden(target, &name);
            (name, text)
        })
        .collect()
}

fn golden_name(fixture: &str, artifact_path: &str) -> String {
    format!("{fixture}.{}", artifact_path.replace(['/', '.'], "_"))
}

fn refusal(result: Result<meridian_sdk::Emitted, EmitError>) -> CompileError {
    match result {
        Err(EmitError::Refused(e)) => e,
        Err(EmitError::Plugin(e)) => panic!("the plugin failed instead of refusing: {e}"),
        Ok(_) => panic!("the target compiled a document it should refuse"),
    }
}

/// A document the target compiles, for the tests that only need one.
fn compilable(target: &Target) -> meridian_sdk::ResourceGraph {
    without_positions(load("restart_chain", target.name))
}

#[test]
fn every_fixture_compiles_to_its_golden_files_across_the_boundary() {
    for target in TARGETS {
        let client = start(target);
        for fixture in FIXTURES {
            let graph = without_positions(load(fixture, target.name));
            let result = client.emit(&graph, &Data::new());
            let want = goldens_for(target.name, fixture);

            if target.refused.contains(fixture) {
                let got = format!("{}\n", refusal(result));
                let name = format!("{fixture}.refused");
                assert_eq!(
                    want.keys().collect::<Vec<_>>(),
                    [&name],
                    "{}/{fixture}",
                    target.name
                );
                assert_eq!(got, want[&name], "{}: {name}", target.name);
                continue;
            }

            let emitted =
                result.unwrap_or_else(|e| panic!("{} compiling {fixture}: {e}", target.name));
            let mut got: BTreeMap<String, String> = emitted
                .artifact
                .files
                .iter()
                .map(|(path, text)| (golden_name(fixture, path), text.clone()))
                .collect();
            if !emitted.warnings.is_empty() {
                got.insert(
                    format!("{fixture}.warnings"),
                    emitted.warnings.iter().map(|w| format!("{w}\n")).collect(),
                );
            }
            // Compare the names first, so a missing or extra artifact reads as
            // that and not as a wall of differing text.
            assert_eq!(
                got.keys().collect::<Vec<_>>(),
                want.keys().collect::<Vec<_>>(),
                "{}/{fixture}: the set of artifacts differs",
                target.name
            );
            for (name, text) in &got {
                assert_eq!(text, &want[name], "{}: {name}", target.name);
            }
            // A warning lost or reworded on the way across the boundary
            // shows up here.
            let warnings: String = emitted.warnings.iter().map(|w| format!("{w}\n")).collect();
            assert_eq!(
                warnings,
                want.get(&format!("{fixture}.warnings"))
                    .cloned()
                    .unwrap_or_default(),
                "{}/{fixture}: warnings",
                target.name
            );
        }
        client.close().expect("the plugin exits when asked");
    }
}

#[test]
fn a_plugin_describes_itself() {
    for target in TARGETS {
        let client = start(target);
        let info = client.info();
        assert_eq!(client.name(), target.name);
        assert_eq!(info.name, target.name);
        assert_eq!(
            info.resource_types, target.resource_types,
            "{}",
            target.name
        );
        assert_eq!(info.capabilities, target.capabilities, "{}", target.name);
        assert_eq!(info.category, Category::ConfigurationManagement);
        assert_eq!(
            info.operations,
            Operations {
                plan: true,
                apply: true,
                outputs: false
            }
        );
        assert_eq!(info.protocol_version, PROTOCOL_VERSION);
        assert_eq!(info.sdk_version, VERSION);
        assert!(info.path.ends_with(&binary_name(target.name)));
    }
}

/// A refusal has to be reportable against the author's own line. The position
/// leaves the host inside the tree and comes back inside the refusal, across
/// a process boundary.
#[test]
fn a_refusal_points_at_the_line_the_author_wrote() {
    let puppet = &TARGETS[1];
    let client = start(puppet);
    let graph = load("web_stack", puppet.name);
    let err = refusal(client.emit(&graph, &Data::new()));

    assert_eq!(err.target, "puppet");
    assert_eq!(err.resource, "nginx_site");
    assert_eq!(err.rule, "unsupported-action");
    assert_eq!(err.pos.file, "fixtures/web_stack.yaml");
    let site = graph.resource("nginx_site").unwrap();
    assert!(site.pos.known());
    assert!(
        err.pos.line >= site.pos.line,
        "the refusal is at line {}, before the resource at line {}",
        err.pos.line,
        site.pos.line
    );
    let unpositioned = golden("puppet", "web_stack.refused");
    assert_eq!(
        format!("{err}\n"),
        format!("{}: {unpositioned}", err.pos),
        "a positioned refusal is the recorded one with its position in front"
    );
}

#[test]
fn an_unsupported_resource_type_is_refused_by_every_target() {
    for target in TARGETS {
        let client = start(target);
        let mut graph = compilable(target);
        let first = &mut graph.resources[0];
        first.r#type = "Firewall.Rule".to_owned();
        first.pos = Position {
            file: "site.yaml".to_owned(),
            doc_idx: 0,
            line: 12,
            column: 5,
        };
        let id = first.id.clone();

        let err = refusal(client.emit(&graph, &Data::new()));
        assert_eq!(err.target, target.name);
        assert_eq!(err.resource, id);
        assert_eq!(err.rule, "unsupported-resource-type");
        assert_eq!(
            (err.pos.file.as_str(), err.pos.line, err.pos.column),
            ("site.yaml", 12, 5)
        );
        assert!(err.to_string().starts_with("site.yaml:12:5: "), "{err}");

        // A refusal is an answer, not a crash: the same plugin keeps working.
        client
            .emit(&compilable(target), &Data::new())
            .unwrap_or_else(|e| panic!("{} after a refusal: {e}", target.name));
    }
}

#[test]
fn a_plugin_gives_the_same_answer_every_time_and_from_every_thread() {
    for target in TARGETS {
        let client = start(target);
        let graph = compilable(target);
        let want = client.emit(&graph, &Data::new()).unwrap();

        for call in 0..20 {
            let got = client.emit(&graph, &Data::new()).unwrap();
            assert_eq!(got, want, "{}: call {call} differed", target.name);
        }

        std::thread::scope(|scope| {
            let calls: Vec<_> = (0..8)
                .map(|_| scope.spawn(|| client.emit(&graph, &Data::new())))
                .collect();
            for (i, call) in calls.into_iter().enumerate() {
                let got = call.join().unwrap().unwrap();
                assert_eq!(got, want, "{}: concurrent call {i} differed", target.name);
            }
        });
    }
}

#[test]
fn discovery_loads_every_shipped_target() {
    // Discovery goes by file name, so anything else in the directory is left
    // alone, even if it would run.
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join("discovery");
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    for target in TARGETS {
        let name = binary_name(target.name);
        fs::copy(plugin_dir().join(&name), dir.join(&name)).unwrap();
    }
    fs::write(dir.join("README.txt"), "not a plugin\n").unwrap();
    fs::create_dir(dir.join(format!("{BINARY_PREFIX}directory"))).unwrap();

    let mut registry = Registry::new();
    let infos = registry
        .discover(dir.to_str().unwrap(), &StartOptions::default())
        .expect("discovery succeeds");
    let found: Vec<&str> = infos.iter().map(|i| i.name.as_str()).collect();
    assert_eq!(found, ["ansible", "puppet"]);
    assert_eq!(registry.names(), ["ansible", "puppet"]);

    for target in TARGETS {
        let loaded = registry
            .target(target.name)
            .expect("the target is registered");
        assert_eq!(loaded.info().capabilities, target.capabilities);
        let graph = without_positions(load("accounts", target.name));
        let emitted = loaded.emit(&graph, &Data::new()).unwrap();
        for (path, text) in &emitted.artifact.files {
            assert_eq!(
                text,
                &golden(target.name, &golden_name("accounts", path)),
                "{}: {path}",
                target.name
            );
        }
    }
    assert!(registry.target("terraform").is_none());

    // Loading a second plugin under a name already taken is refused, and the
    // first one keeps the name.
    let again = plugin_dir().join(binary_name("ansible"));
    let err = registry
        .load(again.to_str().unwrap(), &StartOptions::default())
        .expect_err("a duplicate target is refused");
    assert!(err.to_string().contains("ansible"), "{err}");
    assert_eq!(registry.names(), ["ansible", "puppet"]);

    registry.close().expect("every plugin exits when asked");
}
