//! Embeds the repository's examples and the reference pages the server
//! serves as resources, so that `meridian mcp` answers `get_example` and
//! `resources/read` from the binary alone, with no checkout beside it.
//!
//! Every example under `examples/` is compiled by `crates/meridian-cli/tests/examples.rs`
//! and formatted by `meridian fmt --check`, which is why they are worth
//! handing to a model as they stand. The pages are the DSL reference and the
//! two tables a model most needs beside it.

use std::fs;
use std::path::{Path, PathBuf};

/// The pages served as resources, relative to `docs/`.
const PAGES: &[&str] = &[
    "reference/dsl/index.md",
    "reference/dsl/how-it-fits-together.md",
    "reference/dsl/resources.md",
    "reference/dsl/dependson.md",
    "reference/dsl/notifies.md",
    "reference/dsl/references.md",
    "reference/dsl/interpolation.md",
    "reference/dsl/expressions.md",
    "reference/dsl/conditions.md",
    "reference/dsl/secrets.md",
    "reference/dsl/credentials.md",
    "reference/dsl/outputs-and-hand-offs.md",
    "reference/dsl/infrastructure-as-code.md",
    "reference/dsl/configuration-management.md",
    "reference/dsl/containerization.md",
    "reference/dsl/container-orchestration.md",
    "reference/dsl/yaml-pitfalls.md",
    "reference/capability-matrix.md",
    "reference/lint.md",
];

fn main() {
    let root = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap()).join("../..");
    let out = PathBuf::from(std::env::var("OUT_DIR").unwrap()).join("embedded.rs");
    let mut code = String::new();

    let examples_dir = root.join("examples");
    println!("cargo:rerun-if-changed={}", examples_dir.display());
    let mut examples: Vec<PathBuf> = Vec::new();
    visit(&examples_dir, &mut examples);
    examples.sort();
    code.push_str("/// Every example, as `<directory>/<file>` and its text.\n");
    code.push_str("pub const EXAMPLES: &[(&str, &str)] = &[\n");
    for path in &examples {
        let shown = path
            .strip_prefix(&examples_dir)
            .unwrap()
            .to_string_lossy()
            .replace('\\', "/");
        code.push_str(&format!(
            "    ({shown:?}, include_str!({:?})),\n",
            path.display().to_string()
        ));
    }
    code.push_str("];\n\n");

    // The target pages, one per plugin, so `explain` can point at them.
    let targets_dir = root.join("docs/targets");
    println!("cargo:rerun-if-changed={}", targets_dir.display());
    let mut pages: Vec<String> = PAGES.iter().map(|p| (*p).to_owned()).collect();
    let mut target_pages: Vec<String> = fs::read_dir(&targets_dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "md"))
        .map(|p| format!("targets/{}", p.file_name().unwrap().to_string_lossy()))
        .collect();
    target_pages.sort();
    pages.extend(target_pages);
    code.push_str("/// The reference pages, as their path under `docs/` and their text.\n");
    code.push_str("pub const PAGES: &[(&str, &str)] = &[\n");
    for page in &pages {
        let path = root.join("docs").join(page);
        println!("cargo:rerun-if-changed={}", path.display());
        if path.is_file() {
            code.push_str(&format!(
                "    ({page:?}, include_str!({:?})),\n",
                path.display().to_string()
            ));
        }
    }
    code.push_str("];\n");
    fs::write(out, code).unwrap();
}

/// Every YAML file under `dir`, at any depth, in no particular order.
fn visit(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        println!("cargo:rerun-if-changed={}", path.display());
        if path.is_dir() {
            visit(&path, out);
        } else if path.extension().is_some_and(|e| e == "yaml") {
            out.push(path);
        }
    }
}
