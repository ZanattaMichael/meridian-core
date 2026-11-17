//! Every document under `examples/` compiles through the real command and
//! the real plugins, so an example the documentation shows cannot rot.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use meridian_plugin_testkit::{plugin_binary, repo_root, scratch};

/// Whether a file holds values for `--data`, not documents.
fn is_data(file: &Path) -> bool {
    file.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.ends_with(".data.yaml"))
}

fn examples() -> Vec<PathBuf> {
    let mut found = Vec::new();
    for directory in fs::read_dir(repo_root().join("examples")).unwrap() {
        let directory = directory.unwrap().path();
        if !directory.is_dir() {
            continue;
        }
        for file in fs::read_dir(&directory).unwrap() {
            let file = file.unwrap().path();
            if file.extension().is_some_and(|e| e == "yaml") && !is_data(&file) {
                found.push(file);
            }
        }
    }
    found.sort();
    found
}

/// The data an example is compiled with, one list of `--data` files per run.
///
/// `site.yaml` is given `site.data.yaml` when that file exists. Each
/// `site.<name>.data.yaml` beside it is a variant, layered over the first as
/// a second `--data`, and compiled as a run of its own. An example with no
/// data file is compiled once, with no data.
fn data_sets(example: &Path) -> Vec<Vec<PathBuf>> {
    let stem = example.file_stem().unwrap().to_str().unwrap();
    let directory = example.parent().unwrap();
    let base = directory.join(format!("{stem}.data.yaml"));
    if !base.exists() {
        return vec![Vec::new()];
    }
    let mut sets = vec![vec![base.clone()]];
    let mut variants: Vec<PathBuf> = fs::read_dir(directory)
        .unwrap()
        .map(|file| file.unwrap().path())
        .filter(|file| {
            let name = file.file_name().unwrap().to_str().unwrap();
            is_data(file) && *file != base && name.starts_with(&format!("{stem}."))
        })
        .collect();
    variants.sort();
    for variant in variants {
        sets.push(vec![base.clone(), variant]);
    }
    sets
}

/// The targets a file's documents select.
fn targets(file: &Path) -> BTreeSet<String> {
    fs::read_to_string(file)
        .unwrap()
        .lines()
        .filter_map(|line| line.strip_prefix("  target: "))
        .map(|target| target.trim().to_owned())
        .collect()
}

/// The names of a file's documents.
fn names(file: &Path) -> Vec<String> {
    let text = fs::read_to_string(file).unwrap();
    let mut names = Vec::new();
    let mut in_metadata = false;
    for line in text.lines() {
        if line == "metadata:" {
            in_metadata = true;
        } else if !line.starts_with(' ') && !line.is_empty() {
            in_metadata = false;
        } else if in_metadata {
            if let Some(name) = line.strip_prefix("  name: ") {
                names.push(name.trim().to_owned());
            }
        }
    }
    names
}

/// A workflow with every `targetHost` that waits for an output given an
/// address instead, as it is once the stage it waits for has been applied.
fn given_hosts(text: &str) -> String {
    let mut out = String::new();
    let mut lines = text.lines().peekable();
    while let Some(line) = lines.next() {
        if line == "    from: infrastructure" {
            out.push_str("    from: inline\n    host: 203.0.113.10\n");
            // `ref` and `output` belong to the form being replaced.
            while lines.peek().is_some_and(|next| {
                next.starts_with("    ref: ") || next.starts_with("    output: ")
            }) {
                lines.next();
            }
        } else {
            out.push_str(line);
            out.push('\n');
        }
    }
    out
}

/// `meridian <command> <file>` with every plugin the file needs and the data
/// given.
fn meridian(command: &str, file: &Path, data: &[PathBuf]) -> Command {
    let mut meridian = Command::new(env!("CARGO_BIN_EXE_meridian"));
    meridian.arg(command).arg(file);
    if command != "validate" {
        for target in targets(file) {
            let binary = plugin_binary(&format!("meridian-target-{target}"));
            meridian.arg("--plugin-dir").arg(binary.parent().unwrap());
        }
        for data in data {
            meridian.arg("--data").arg(data);
        }
    }
    meridian
}

/// Whether the command succeeded, and everything it printed.
fn outcome(mut command: Command) -> (bool, String) {
    let output = command.output().unwrap();
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    (output.status.success(), text)
}

fn run(command: Command, what: &str) -> String {
    let (succeeded, text) = outcome(command);
    assert!(succeeded, "{what} failed:\n{text}");
    text
}

/// The examples whose conditions read a secret. `compile` never fetches one,
/// so these are the examples it cannot decide.
const DECIDED_BY_A_SECRET: &[&str] = &[
    "examples/language/features-by-secret.yaml",
    "examples/language/secret-from-a-store.yaml",
];

/// What `compile` says of a condition that reads a secret.
const NOT_FETCHED: &str =
    "is not fetched here: a secret is fetched by plan and apply, never by validate or compile";

fn decided_by_a_secret(file: &Path) -> bool {
    DECIDED_BY_A_SECRET
        .iter()
        .any(|example| repo_root().join(example) == file)
}

/// Every file `meridian compile --out` wrote for a workflow, by its path
/// under the output directory.
fn compiled(file: &Path, data: &[PathBuf], scratch_name: &str) -> BTreeMap<String, String> {
    let out = scratch(env!("CARGO_TARGET_TMPDIR"), scratch_name);
    let mut command = meridian("compile", file, data);
    command.arg("--out").arg(&out);
    run(command, &format!("compiling {}", file.display()));
    let mut files = BTreeMap::new();
    let mut pending = vec![out.clone()];
    while let Some(directory) = pending.pop() {
        for entry in fs::read_dir(&directory).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                pending.push(path);
            } else {
                let relative = path.strip_prefix(&out).unwrap().to_str().unwrap();
                // Named with forward slashes on every system, so the tests
                // can ask for "document/file" wherever they run.
                files.insert(
                    relative.replace('\\', "/"),
                    fs::read_to_string(&path).unwrap(),
                );
            }
        }
    }
    files
}

/// The documents a `spec.when` leaves out of a run with this data, by what
/// the command prints: the document's line, then `skipped: its when ...`.
fn skipped(file: &Path, data: &[PathBuf], scratch_name: &str) -> BTreeSet<String> {
    let out = scratch(env!("CARGO_TARGET_TMPDIR"), scratch_name);
    let mut command = meridian("compile", file, data);
    command.arg("--out").arg(&out);
    let said = run(command, &format!("compiling {}", file.display()));
    let mut skipped = BTreeSet::new();
    let mut document = "";
    for line in said.lines() {
        if let Some(what) = line.strip_prefix("  ") {
            if what.starts_with("skipped: its when") {
                skipped.insert(document.to_owned());
            }
        } else if let Some((name, _)) = line.split_once(" (") {
            document = name;
        }
    }
    skipped
}

#[test]
fn every_example_validates_and_compiles_with_the_real_plugins() {
    let examples = examples();
    assert!(!examples.is_empty(), "no examples were found");
    for file in examples {
        assert!(!targets(&file).is_empty(), "{}: no target", file.display());
        run(
            meridian("validate", &file, &[]),
            &format!("validating {}", file.display()),
        );
        for data in data_sets(&file) {
            let what = format!("compiling {} with {data:?}", file.display());
            if decided_by_a_secret(&file) {
                // Its conditions read secrets, and compile fetches none, so
                // it stops and says so. The example is planned with its
                // secrets in `secrets_decide_an_example_and_are_written_nowhere`.
                let (succeeded, said) = outcome(meridian("compile", &file, &data));
                assert!(!succeeded, "{what} read a secret:\n{said}");
                assert!(said.contains(NOT_FETCHED), "{what}:\n{said}");
            } else {
                run(meridian("compile", &file, &data), &what);
            }
        }
    }
    for example in DECIDED_BY_A_SECRET {
        assert!(repo_root().join(example).exists(), "{example} is gone");
    }
}

/// A workflow is several documents, and each one a command can compile ends
/// up in a directory of its own, named after the document. A ResourceSet that
/// waits for an output of an earlier stage has no host yet, so it is reported
/// as deferred and nothing is written for it.
#[test]
fn every_workflow_compiles_each_document_or_defers_it() {
    let workflows: Vec<PathBuf> = examples()
        .into_iter()
        .filter(|file| file.parent().unwrap().ends_with("workflow"))
        .collect();
    assert!(
        workflows.len() >= 7,
        "examples/workflow holds {} workflows",
        workflows.len()
    );
    for file in workflows {
        let text = fs::read_to_string(&file).unwrap();
        let names = names(&file);
        assert!(
            names.len() >= 2,
            "{} is not a workflow of several documents",
            file.display()
        );
        // The documents that wait for an output, in the order the file
        // declares its documents.
        let waiting: Vec<bool> = text
            .split("\n---\n")
            .map(|document| document.contains("    from: infrastructure"))
            .collect();
        assert_eq!(waiting.len(), names.len(), "{}", file.display());

        let stem = file.file_stem().unwrap().to_str().unwrap();
        // A document with a `spec.when` may be left out of a run. It must
        // still be part of at least one: a document no data set ever keeps
        // is a document nothing here compiles.
        let mut ever_kept: BTreeSet<&String> = BTreeSet::new();
        for (i, data) in data_sets(&file).iter().enumerate() {
            let left_out = skipped(&file, data, &format!("workflow-skipped-{stem}-{i}"));
            ever_kept.extend(names.iter().filter(|name| !left_out.contains(*name)));
            // A deferred document is not compiled, so on its own that run
            // proves nothing about it. Each one is compiled again here as it
            // will be once its stage has run: with the host filled in.
            let with_hosts = given_hosts(&text);
            assert!(!with_hosts.contains("from: infrastructure"));
            let scratch_dir = scratch(env!("CARGO_TARGET_TMPDIR"), &format!("hosts-{stem}-{i}"));
            let copy = scratch_dir.join(file.file_name().unwrap());
            fs::write(&copy, with_hosts).unwrap();
            let files = compiled(&copy, data, &format!("workflow-hosts-{stem}-{i}"));
            for name in &names {
                assert_eq!(
                    files
                        .keys()
                        .any(|path| path.starts_with(&format!("{name}/"))),
                    !left_out.contains(name),
                    "{}: document {name:?} once it had a host (left out by its when: {}); files: {:?}",
                    file.display(),
                    left_out.contains(name),
                    files.keys().collect::<Vec<_>>()
                );
            }

            let files = compiled(&file, data, &format!("workflow-{stem}-{i}"));
            for (name, waits) in names.iter().zip(&waiting) {
                let written = files
                    .keys()
                    .any(|path| path.starts_with(&format!("{name}/")));
                assert_eq!(
                    written,
                    !waits && !left_out.contains(name),
                    "{}: document {name:?} (waits for an output: {waits}); files: {:?}",
                    file.display(),
                    files.keys().collect::<Vec<_>>()
                );
            }
        }
        assert_eq!(
            ever_kept.len(),
            names.len(),
            "{}: a document is left out by its when with every data set beside the example",
            file.display()
        );
    }
}

/// The same conditions decide the same way in every document of a workflow,
/// whatever tool each document compiles for.
#[test]
fn one_set_of_conditions_decides_a_whole_workflow() {
    let file = repo_root().join("examples/workflow/environments.yaml");
    let sets = data_sets(&file);
    assert_eq!(sets.len(), 2, "a development and a production data set");
    let dev = compiled(&file, &sets[0], "environments-dev");
    let prod = compiled(&file, &sets[1], "environments-prod");
    let all = |files: &BTreeMap<String, String>, document: &str| -> String {
        files
            .iter()
            .filter(|(path, _)| path.starts_with(&format!("{document}/")))
            .map(|(_, content)| content.as_str())
            .collect()
    };

    // Terraform: `data.env != 'prod'` keeps the bastion in development only,
    // and `data.env == 'prod'` keeps the address in production only.
    let (dev_machines, prod_machines) = (all(&dev, "machines"), all(&prod, "machines"));
    assert!(dev_machines.contains("bastion"), "{dev_machines}");
    assert!(!prod_machines.contains("bastion"), "{prod_machines}");
    assert!(!dev_machines.contains("aws_eip"), "{dev_machines}");
    assert!(prod_machines.contains("aws_eip"), "{prod_machines}");
    // `defined(data.audit) && data.audit.enabled`: the key is only in production.
    assert!(!dev_machines.contains("shop-audit-logs"));
    assert!(prod_machines.contains("shop-audit-logs"));
    // The resource that waited for the bastion carries on without it.
    assert!(prod_machines.contains("terraform_data"));

    // Kubernetes: the same `data.env != 'prod'`, the same answer.
    let (dev_jobs, prod_jobs) = (all(&dev, "jobs"), all(&prod, "jobs"));
    assert!(
        dev_jobs.contains("ghcr.io/example/load:0.4.0"),
        "{dev_jobs}"
    );
    assert!(
        !prod_jobs.contains("ghcr.io/example/load:0.4.0"),
        "{prod_jobs}"
    );
    assert!(!dev_jobs.contains("PodDisruptionBudget"));
    assert!(prod_jobs.contains("PodDisruptionBudget"));
    // What no condition touches is in both.
    for jobs in [&dev_jobs, &prod_jobs] {
        assert!(jobs.contains("CronJob"), "{jobs}");
    }
    // No condition reaches a tool: nothing compiled mentions `data.`.
    for (path, content) in dev.iter().chain(&prod) {
        assert!(!content.contains("data.env"), "{path} carries a condition");
    }

    // With no data the workflow has no answer, and says which key it needs.
    let output = meridian("compile", &file, &[]).output().unwrap();
    assert!(!output.status.success());
    let said = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(said.contains("data.env is not set"), "{said}");
}

/// `tenants.yaml` is one workflow shaped entirely by data: which documents
/// run, which resources they hold and every name and size in them. The
/// starter and the enterprise plan are compiled, with each deferred document
/// given its host, and compared with what the plan is documented to mean.
#[test]
fn a_plan_decides_documents_resources_and_values_in_the_tenants_workflow() {
    let file = repo_root().join("examples/workflow/tenants.yaml");
    let sets = data_sets(&file);
    assert_eq!(sets.len(), 2, "a starter and an enterprise data set");

    // The Salt document waits for Terraform's output, so it is compiled as
    // it will be once that has been applied.
    let with_hosts = scratch(env!("CARGO_TARGET_TMPDIR"), "tenants-hosts").join("tenants.yaml");
    fs::write(
        &with_hosts,
        given_hosts(&fs::read_to_string(&file).unwrap()),
    )
    .unwrap();
    let starter = compiled(&with_hosts, &sets[0], "tenants-starter");
    let enterprise = compiled(&with_hosts, &sets[1], "tenants-enterprise");
    let all = |files: &BTreeMap<String, String>, document: &str| -> String {
        files
            .iter()
            .filter(|(path, _)| path.starts_with(&format!("{document}/")))
            .map(|(_, content)| content.as_str())
            .collect()
    };

    // Which documents are part of the run at all.
    assert_eq!(
        skipped(&file, &sets[0], "tenants-skipped-starter"),
        BTreeSet::from(["backup".to_owned(), "dashboards".to_owned()])
    );
    assert!(skipped(&file, &sets[1], "tenants-skipped-enterprise").is_empty());
    for document in ["dashboards", "backup"] {
        assert_eq!(
            all(&starter, document),
            "",
            "{document} on the starter plan"
        );
        assert_ne!(all(&enterprise, document), "", "{document} on enterprise");
    }

    // Terraform: named conditions, variables, functions and a typed value.
    let (small, large) = (all(&starter, "network"), all(&enterprise, "network"));
    assert!(
        small.contains("t3.small") && !small.contains("m6i.large"),
        "{small}"
    );
    assert!(
        large.contains("m6i.large") && !large.contains("t3.small"),
        "{large}"
    );
    assert!(!small.contains("aws_db_instance"), "{small}");
    assert!(large.contains("aws_db_instance"), "{large}");
    // `cidrSubnet(data.cidr, 8, 1)`, and `upper` and `title` in the tags.
    for network in [&small, &large] {
        assert!(network.contains("10.20.1.0/24"), "{network}");
        assert!(network.contains("PLATFORM"), "{network}");
    }
    assert!(small.contains("\"Plan\": \"Starter\""), "{small}");
    assert!(large.contains("\"Name\": \"acme-enterprise\""), "{large}");
    // A number and a boolean keep their types on the way through.
    assert!(large.contains("\"allocated_storage\": 200"), "{large}");
    assert!(large.contains("\"multi_az\": true"), "{large}");
    // The output with a condition is declared only with the database.
    assert!(!small.contains("\"database\""), "{small}");
    assert!(large.contains("\"database\""), "{large}");
    // The references the author wrote are still references for Terraform.
    assert!(large.contains("${aws_vpc.main.id}"), "{large}");

    // Salt: a list joined into text, and an edge with a condition.
    let (plain, tls) = (all(&starter, "app"), all(&enterprise, "app"));
    assert!(plain.contains("listen 8080;"), "{plain}");
    assert!(tls.contains("listen 80 443;"), "{tls}");
    assert!(!plain.contains("/etc/ssl/acme.pem"), "{plain}");
    assert!(tls.contains("/etc/ssl/acme.pem"), "{tls}");
    // Text from data that looks like a reference is written as the text it
    // is, on both plans.
    for app in [&plain, &tls] {
        assert!(
            app.contains("Set ${EDITOR} before you edit anything on this machine."),
            "{app}"
        );
    }

    // Kubernetes and Chef: only on enterprise, with the values placed.
    let dashboards = all(&enterprise, "dashboards");
    assert!(dashboards.contains("tenant-acme"), "{dashboards}");
    assert!(
        dashboards.contains("RETENTION_DAYS: \"90\""),
        "{dashboards}"
    );
    let backup = all(&enterprise, "backup");
    assert!(backup.contains("backups-acme"), "{backup}");
    assert!(backup.contains("RETAIN_DAYS=90"), "{backup}");

    // Nothing compiled carries an expression: every one was decided.
    for (path, content) in starter.iter().chain(&enterprise) {
        for left in ["data.", "var.", "cond."] {
            assert!(
                !content.contains(&format!("{{ {left}")),
                "{path} still holds an interpolation"
            );
        }
    }

    // One value changed on the command line changes every document.
    let mut command = meridian("compile", &with_hosts, &sets[1]);
    let out = scratch(env!("CARGO_TARGET_TMPDIR"), "tenants-globex");
    command
        .arg("--out")
        .arg(&out)
        .args(["--set", "tenant=globex"]);
    let said = run(command, "compiling tenants.yaml for another tenant");
    assert!(said.contains("backup.globex.example.com"), "{said}");
    let conf = fs::read_to_string(out.join("network/main.tf.json")).unwrap();
    assert!(
        conf.contains("globex-enterprise") && !conf.contains("acme"),
        "{conf}"
    );
}

/// One machine for `features-by-secret.yaml`: which secrets it has, and the
/// files a plan must then create.
struct Machine {
    what: &'static str,
    password: &'static str,
    license: Option<&'static str>,
    token: Option<&'static str>,
    env: &'static str,
    creates: &'static [&'static str],
}

const LICENSE: &str = "LIC-7Q2M-not-a-real-licence-ZX90";
const DEPLOY_TOKEN: &str = "dpl_4411-not-a-real-token-kk";
const STRONG: &str = "c0rrect-h0rse-not-a-real-password";

const MACHINES: &[Machine] = &[
    Machine {
        what: "every secret, in production",
        password: STRONG,
        license: Some(LICENSE),
        token: Some(DEPLOY_TOKEN),
        env: "prod",
        creates: &[
            "conf_dir",
            "deploy_conf",
            "hardening",
            "premium_conf",
            "site_conf",
        ],
    },
    Machine {
        what: "no licence",
        password: STRONG,
        license: None,
        token: Some(DEPLOY_TOKEN),
        env: "prod",
        creates: &["conf_dir", "deploy_conf", "hardening", "site_conf"],
    },
    Machine {
        what: "no deploy token",
        password: STRONG,
        license: Some(LICENSE),
        token: None,
        env: "prod",
        creates: &["conf_dir", "hardening", "premium_conf", "site_conf"],
    },
    Machine {
        what: "the default password",
        password: "changeme",
        license: Some(LICENSE),
        token: Some(DEPLOY_TOKEN),
        env: "prod",
        // No deploys and no hardening on a default password, and a warning.
        creates: &["conf_dir", "password_warning", "premium_conf", "site_conf"],
    },
    Machine {
        what: "an empty password",
        password: "",
        license: None,
        token: None,
        env: "prod",
        creates: &["conf_dir", "password_warning", "site_conf"],
    },
    Machine {
        what: "a strong password outside production",
        password: STRONG,
        license: None,
        token: None,
        env: "dev",
        creates: &["conf_dir", "site_conf"],
    },
];

/// Every file under `dir`, as text.
fn everything_under(dir: &Path) -> Vec<(PathBuf, String)> {
    let mut found = Vec::new();
    let mut pending = vec![dir.to_path_buf()];
    while let Some(directory) = pending.pop() {
        let Ok(entries) = fs::read_dir(&directory) else {
            continue;
        };
        for entry in entries {
            let path = entry.unwrap().path();
            if path.is_dir() {
                pending.push(path);
            } else {
                let text = String::from_utf8_lossy(&fs::read(&path).unwrap()).into_owned();
                found.push((path, text));
            }
        }
    }
    found
}

/// `features-by-secret.yaml` is planned and applied for six machines that
/// differ only in the secrets they have. The example is the file from the
/// repository with three things stood in, none of them part of the language
/// being shown: the target is the test's own (a plan needs a tool, and the
/// conditions are decided before any tool is involved), the licence is read
/// from the test's directory, and the token is printed by `printf` instead of
/// a password manager.
#[test]
fn secrets_decide_an_example_and_are_written_nowhere() {
    let example = repo_root().join(DECIDED_BY_A_SECRET[0]);
    let source = fs::read_to_string(&example).unwrap();
    let data = &data_sets(&example)[0];
    for machine in MACHINES {
        let name = format!("by-secret-{}", machine.what.replace([' ', ','], "-"));
        let dir = scratch(env!("CARGO_TARGET_TMPDIR"), &name);
        let token = match machine.token {
            Some(token) => format!("command: [printf, \"%s\", \"{token}\"]"),
            // A command that prints nothing: there is no token.
            None => "command: [printf, \"\"]".to_owned(),
        };
        let stood_in = [
            ("target: ansible", "target: fake".to_owned()),
            (
                "path: /etc/site/license.key",
                "path: license.key".to_owned(),
            ),
            ("command: [pass, show, site/deploy-token]", token),
        ]
        .iter()
        .fold(source.clone(), |text, (from, to)| {
            assert!(text.contains(from), "the example no longer has {from:?}");
            text.replace(from, to)
        });
        fs::write(dir.join("site.yaml"), stood_in).unwrap();
        if let Some(license) = machine.license {
            let path = dir.join("license.key");
            fs::write(&path, format!("{license}\n")).unwrap();
            // Only Unix has a mode to keep the file private with.
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
            }
        }

        let meridian = |command: &str| {
            let mut meridian = Command::new(env!("CARGO_BIN_EXE_meridian"));
            meridian
                .arg(command)
                .arg("site.yaml")
                .arg("--data")
                .arg(&data[0])
                .arg("--set")
                .arg(format!("env={}", machine.env))
                .current_dir(&dir)
                .env("MERIDIAN_PLUGIN_DIR", meridian_target_fake::plugin_dir())
                .env_remove("MERIDIAN_STATE_DIR")
                .env("SITE_DB_PASSWORD", machine.password);
            meridian
        };

        let planned = run(meridian("plan"), &format!("planning for {}", machine.what));
        let creates: BTreeSet<&str> = planned
            .lines()
            .filter_map(|line| line.trim().strip_prefix("+ create files/"))
            .filter_map(|rest| rest.split(':').next())
            .collect();
        assert_eq!(
            creates,
            machine.creates.iter().copied().collect(),
            "{}:\n{planned}",
            machine.what
        );
        let applied = run(meridian("apply"), &format!("applying for {}", machine.what));

        // The answer to a condition is not a secret, and is written.
        let conf = everything_under(&dir.join(".meridian"))
            .into_iter()
            .find(|(path, _)| path.ends_with("files/site_conf"))
            .map(|(_, text)| text)
            .unwrap_or_else(|| panic!("{}: no site_conf was applied", machine.what));
        assert!(
            conf.contains(&format!("licensed = {}", machine.license.is_some())),
            "{}: {conf}",
            machine.what
        );

        // No secret is in anything the run printed or left behind. A
        // password of "changeme" or "" is the document's own text, so those
        // are not searched for.
        let secrets = [
            Some(machine.password).filter(|password| *password == STRONG),
            machine.license,
            machine.token,
        ];
        for secret in secrets.into_iter().flatten() {
            for said in [&planned, &applied] {
                assert!(!said.contains(secret), "{}:\n{said}", machine.what);
            }
            for (path, text) in everything_under(&dir) {
                let own = path.ends_with("license.key") || path.ends_with("site.yaml");
                assert!(
                    own || !text.contains(secret),
                    "{}: {} holds a secret",
                    machine.what,
                    path.display()
                );
            }
        }
    }
}

/// `secret-in-a-setting.yaml` places a secret in a file. `compile` writes
/// the secret's name where it belongs and never reads the secret, and `plan`
/// stops before anything could be written.
/// `secret-from-a-store.yaml` is planned on three machines that keep the
/// deploy token in different places, and on one that has none. The target is
/// stood in as above, and the keyring's key is one no machine has, so that a
/// developer's own keyring does not answer.
#[test]
fn one_document_finds_its_secret_wherever_each_machine_keeps_it() {
    let example = repo_root().join(DECIDED_BY_A_SECRET[1]);
    let source = fs::read_to_string(&example).unwrap();
    let root = scratch(env!("CARGO_TARGET_TMPDIR"), "secret-from-a-store");
    let key = format!("meridian-test:example-not-stored:{}", std::process::id());
    let stood_in = [
        ("target: ansible", "target: fake".to_owned()),
        ("key: meridian:deploy-token", format!("key: \"{key}\"")),
    ]
    .iter()
    .fold(source, |text, (from, to)| {
        assert!(text.contains(from), "the example no longer has {from:?}");
        text.replace(from, to)
    });
    let credentials = root.join("credentials");
    fs::create_dir_all(&credentials).unwrap();
    fs::write(
        credentials.join("deploy-token"),
        format!("{DEPLOY_TOKEN}\n"),
    )
    .unwrap();

    let meridian = |dir: &Path, command: &[&str], env: &[(&str, &str)]| {
        let mut meridian = Command::new(env!("CARGO_BIN_EXE_meridian"));
        meridian
            .args(command)
            .arg("site.yaml")
            .current_dir(dir)
            .env("MERIDIAN_PLUGIN_DIR", meridian_target_fake::plugin_dir())
            .env_remove("MERIDIAN_STATE_DIR")
            .env_remove("DBUS_SESSION_BUS_ADDRESS")
            .env_remove("CREDENTIALS_DIRECTORY")
            .env_remove("DEPLOY_TOKEN");
        for (name, value) in env {
            meridian.env(name, value);
        }
        meridian
    };
    let given = credentials.display().to_string();
    for (what, env, place, deploys) in [
        (
            "a pipeline",
            vec![("DEPLOY_TOKEN", DEPLOY_TOKEN)],
            "env            ok: the environment variable DEPLOY_TOKEN is set",
            true,
        ),
        (
            "a service",
            vec![("CREDENTIALS_DIRECTORY", given.as_str())],
            "systemd-creds  ok: this unit was given the credential deploy-token",
            true,
        ),
        (
            // The unit's credential is asked before the variable.
            "a service that also has the variable",
            vec![
                ("CREDENTIALS_DIRECTORY", given.as_str()),
                ("DEPLOY_TOKEN", "not-the-token"),
            ],
            "systemd-creds  ok: this unit was given the credential deploy-token",
            true,
        ),
        (
            "a machine with no token",
            vec![],
            "absent, optional: ",
            false,
        ),
    ] {
        // Each machine is a directory of its own, with its own record.
        let dir = root.join(what.replace(' ', "-"));
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("site.yaml"), &stood_in).unwrap();
        let checked = run(
            meridian(&dir, &["secret", "check"], &env),
            &format!("checking on {what}"),
        );
        // The column is as wide as the widest provider shown.
        let squeezed = checked.split_whitespace().collect::<Vec<_>>().join(" ");
        let wanted = place.split_whitespace().collect::<Vec<_>>().join(" ");
        assert!(squeezed.contains(&wanted), "{what}:\n{checked}");

        let planned = run(
            meridian(&dir, &["plan"], &env),
            &format!("planning on {what}"),
        );
        assert_eq!(
            planned.contains("+ create files/deploy_timer"),
            deploys,
            "{what}:\n{planned}"
        );
        assert!(planned.contains("+ create files/deploys_conf"), "{planned}");
        let applied = run(
            meridian(&dir, &["apply"], &env),
            &format!("applying on {what}"),
        );
        for said in [&checked, &planned, &applied] {
            assert!(!said.contains(DEPLOY_TOKEN), "{what}:\n{said}");
        }
        for (path, text) in everything_under(&dir) {
            assert!(
                !text.contains(DEPLOY_TOKEN),
                "{what}: {} holds the token",
                path.display()
            );
        }
    }
}

#[test]
fn a_secret_in_a_setting_is_compiled_as_its_name_and_never_planned() {
    const CANARY: &str = "zzq-not-a-real-password-canary";
    let example = repo_root().join("examples/language/secret-in-a-setting.yaml");
    let data = &data_sets(&example)[0];
    let out = scratch(env!("CARGO_TARGET_TMPDIR"), "secret-in-a-setting");

    let mut compile = meridian("compile", &example, data);
    compile
        .arg("--out")
        .arg(&out)
        .env("APP_DB_PASSWORD", CANARY);
    let said = run(compile, "compiling secret-in-a-setting.yaml");
    let written: String = everything_under(&out)
        .into_iter()
        .map(|(_, text)| text)
        .collect();
    for line in [
        "DATABASE_URL=postgres://app:urlEncode((secret db_password))@db.internal:5432/app",
        "SIGNING_KEY=base64Encode((secret signing_key))",
        "LOG_LEVEL=info",
        "listen = 0.0.0.0:8080",
    ] {
        assert!(written.contains(line), "no {line:?} in:\n{written}");
    }
    assert!(!said.contains(CANARY) && !written.contains(CANARY));

    let mut plan = meridian("plan", &example, data);
    plan.current_dir(&out).env("APP_DB_PASSWORD", CANARY);
    let (succeeded, said) = outcome(plan);
    assert!(!succeeded, "{said}");
    assert!(
        said.contains(
            "resource \"app_env\": params.content holds a secret, and target \"chef\" cannot \
             be handed a secret yet"
        ),
        "{said}"
    );
    assert!(!said.contains(CANARY), "{said}");
    for (path, text) in everything_under(&out) {
        assert!(
            !text.contains(CANARY),
            "{} holds the secret",
            path.display()
        );
    }
}
