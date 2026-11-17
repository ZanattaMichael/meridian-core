//! Every call to `expose` is a place a secret can leave its protection, so
//! there are few of them and each is known. This test reads the workspace's
//! sources and fails when one appears anywhere it is not expected: adding a
//! call site is a decision to be made on purpose, here, in review.

use std::fs;
use std::path::{Path, PathBuf};

/// The source files that may call `expose`, relative to the workspace, with
/// why each one does.
const ALLOWED: &[(&str, &str)] = &[
    (
        "crates/meridian-secret/src/lib.rs",
        "the definition, and its documentation",
    ),
    (
        "crates/meridian-secret/src/mask.rs",
        "the output filter searches for the bytes, and derives their encoded forms",
    ),
    (
        "crates/meridian-secret/src/deliver.rs",
        "a secret file is written from the bytes",
    ),
    (
        "crates/meridian-module/src/oci.rs",
        "a registry login's password is encoded into the Authorization header it is sent in",
    ),
    (
        "crates/meridian-secret/src/table.rs",
        "secrets are joined and encoded into new protected memory",
    ),
    (
        "crates/meridian-secret/src/stores.rs",
        "meridian secret set hands the bytes to the kernel keyring, or to secret-tool on its \
         standard input",
    ),
];

fn workspace() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap()
}

fn sources(dir: &Path, found: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name();
        if path.is_dir() {
            // Tests may expose a secret to check it; `target` is not source.
            if name != "target" && name != "tests" && name != ".git" {
                sources(&path, found);
            }
        } else if path.extension().is_some_and(|e| e == "rs") {
            found.push(path);
        }
    }
}

#[test]
fn expose_is_called_only_where_it_is_expected() {
    let root = workspace();
    let mut files = Vec::new();
    sources(&root.join("crates"), &mut files);
    sources(&root.join("plugins"), &mut files);
    assert!(files.len() > 50, "the workspace's sources were not found");

    let mut unexpected = Vec::new();
    for file in files {
        let relative = file
            .strip_prefix(&root)
            .unwrap()
            .to_string_lossy()
            .replace('\\', "/");
        if ALLOWED.iter().any(|(path, _)| *path == relative) {
            continue;
        }
        let text = fs::read_to_string(&file).unwrap();
        for (i, line) in text.lines().enumerate() {
            let code = line.split("//").next().unwrap_or(line);
            if code.contains(".expose(") {
                unexpected.push(format!("{relative}:{}: {}", i + 1, line.trim()));
            }
        }
    }
    assert!(
        unexpected.is_empty(),
        "a secret is exposed somewhere new. If that is intended, add the file to ALLOWED in \
         this test with the reason:\n{}",
        unexpected.join("\n")
    );
}

#[test]
fn every_allowed_file_exists_and_does_call_expose() {
    // An entry for a file that no longer exposes anything is a hole waiting
    // to be reused.
    let root = workspace();
    for (path, why) in ALLOWED {
        let text =
            fs::read_to_string(root.join(path)).unwrap_or_else(|e| panic!("{path} ({why}): {e}"));
        assert!(text.contains("expose("), "{path} no longer calls expose");
    }
}

#[test]
fn unsafe_code_is_confined_to_one_module() {
    let root = workspace();
    let mut files = Vec::new();
    sources(&root.join("crates/meridian-secret/src"), &mut files);
    for file in files {
        let text = fs::read_to_string(&file).unwrap();
        let uses = text
            .lines()
            .any(|l| l.split("//").next().unwrap_or(l).contains("unsafe "));
        let is_sys = file.file_name().is_some_and(|n| n == "sys.rs");
        assert_eq!(uses, is_sys, "{}", file.display());
    }
}
