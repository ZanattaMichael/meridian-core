//! Adding a plugin touches a few places outside its own directory. This test
//! names each one and fails when one was missed, so the omission is caught
//! here and not by a reviewer.

use std::fs;

use meridian_plugin_testkit::repo_root;

/// Every directory under `plugins/`, sorted.
fn plugin_directories() -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(repo_root().join("plugins"))
        .unwrap()
        .filter_map(Result::ok)
        .filter(|e| e.path().is_dir())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

#[test]
fn every_plugin_directory_is_in_the_list() {
    assert_eq!(
        plugin_directories(),
        meridian_all_targets::names(),
        "the directories under plugins/ and meridian_all_targets::ALL differ; \
         add the new plugin to crates/meridian-all-targets"
    );
}

#[test]
fn every_plugin_is_a_workspace_dependency_of_this_crate() {
    let manifest =
        fs::read_to_string(repo_root().join("crates/meridian-all-targets/Cargo.toml")).unwrap();
    for name in meridian_all_targets::names() {
        assert!(
            manifest.contains(&format!("meridian-target-{name} = ")),
            "crates/meridian-all-targets/Cargo.toml does not depend on meridian-target-{name}"
        );
    }
}

/// The pages and tables that list every target by hand. A target missing
/// from one of them is a reader who cannot find it.
#[test]
fn every_target_is_where_readers_look_for_it() {
    let root = repo_root();
    let mut missing = Vec::new();
    for name in meridian_all_targets::names() {
        let page = format!("docs/targets/{name}.md");
        if !root.join(&page).is_file() {
            missing.push(format!("{page} (the target's own page)"));
        }
        if !root.join("examples").join(name).is_dir() {
            missing.push(format!("examples/{name}/ (at least one example)"));
        }
        for (file, what) in [
            ("README.md", format!("docs/targets/{name}.md")),
            ("docs/index.md", format!("targets/{name}.md")),
            (
                "docs/quickstart/install.md",
                format!("meridian-target-{name}"),
            ),
            ("docs/reference/capability-matrix.md", format!("[{name}]")),
        ] {
            let text = fs::read_to_string(root.join(file)).unwrap_or_default();
            if !text.contains(&what) {
                missing.push(format!("{file} does not mention {what}"));
            }
        }
    }
    assert!(
        missing.is_empty(),
        "a target is missing from where it is listed:\n  {}",
        missing.join("\n  ")
    );
}

#[test]
fn every_target_with_a_real_tool_test_has_a_workflow_that_runs_it() {
    let root = repo_root();
    let workflows = root.join(".github/workflows");
    let mut missing = Vec::new();
    for name in meridian_all_targets::names() {
        // A real-tool test is marked `#[ignore]`, so a plugin with none has
        // nothing for a workflow to run; the guide does not require one of a
        // first pull request.
        let tests = root.join("plugins").join(name).join("tests");
        let has_real_test = std::fs::read_dir(&tests)
            .map(|dir| {
                dir.flatten().any(|entry| {
                    std::fs::read_to_string(entry.path())
                        .map(|text| text.contains("#[ignore"))
                        .unwrap_or(false)
                })
            })
            .unwrap_or(false);
        if !has_real_test {
            continue;
        }
        let own = workflows.join(format!("{name}.yml")).is_file();
        // Bicep and ARM share azure.yml, since both drive `az`.
        let shared = matches!(name, "arm" | "bicep") && workflows.join("azure.yml").is_file();
        if !own && !shared {
            missing.push(name);
        }
    }
    assert!(
        missing.is_empty(),
        "no real-tool workflow under .github/workflows for: {}",
        missing.join(", ")
    );
}
