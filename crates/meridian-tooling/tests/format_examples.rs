//! The formatter over every document the repository ships. Each one must
//! format without being refused, and formatting the result must change
//! nothing.

use std::path::{Path, PathBuf};

use meridian_tooling::fmt::{format, Options};

fn yaml_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            yaml_files(&path, out);
        } else if matches!(
            path.extension().and_then(|e| e.to_str()),
            Some("yaml" | "yml")
        ) {
            out.push(path);
        }
    }
}

fn shipped() -> Vec<PathBuf> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let mut files = Vec::new();
    for dir in ["examples", "fixtures", "plugins", "crates"] {
        yaml_files(&root.join(dir), &mut files);
    }
    files.sort();
    files
}

#[test]
fn every_shipped_document_formats_and_stays_formatted() {
    let files = shipped();
    assert!(files.len() > 20, "found only {} documents", files.len());
    let mut formatted = 0;
    for path in files {
        let name = path.to_string_lossy();
        let text = std::fs::read_to_string(&path).expect("readable");
        // Some fixtures are deliberately broken YAML; those are refused as a
        // syntax error, which is the right answer for them.
        if meridian_ir::parse_stream(&name, text.as_bytes()).is_err() {
            assert!(
                matches!(
                    format(&name, &text, Options::default()),
                    Err(meridian_tooling::fmt::Error::Syntax(_))
                ),
                "{name}: a file that does not parse must be refused as a syntax error"
            );
            continue;
        }
        let once =
            format(&name, &text, Options::default()).unwrap_or_else(|e| panic!("{name}: {e}"));
        let twice = format(&name, &once, Options::default())
            .unwrap_or_else(|e| panic!("{name}: second pass: {e}"));
        assert_eq!(once, twice, "{name}: formatting is not stable");
        formatted += 1;
    }
    assert!(formatted > 20, "formatted only {formatted} documents");
}
