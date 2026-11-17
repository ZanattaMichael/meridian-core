//! The content hash a module is pinned by, and the tree walk it shares with
//! packing and copying.
//!
//! The hash is of the files, not of an archive, so the same module has the
//! same hash whether it came from git, from a registry or from a mirror.

use std::fs;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use crate::signature::SIGNATURE_FILE;
use crate::Error;

/// Directories that are never part of a module: git's own, and Meridian's
/// state directory.
const EXCLUDED: &[&str] = &[".git", ".meridian"];

pub(crate) fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    bytes.iter().fold(String::new(), |mut out, b| {
        let _ = write!(out, "{b:02x}");
        out
    })
}

/// The SHA-256 of `bytes`, in hex.
pub(crate) fn sha256_hex(bytes: &[u8]) -> String {
    hex(&Sha256::digest(bytes))
}

/// Every file of the module at `root`, as a path relative to it with `/`
/// separators, sorted. A symbolic link is refused: what it points at is not
/// part of the module, so it could not be pinned.
pub(crate) fn files(root: &Path) -> Result<Vec<(String, PathBuf)>, Error> {
    let mut out = Vec::new();
    let mut pending = vec![(String::new(), root.to_path_buf())];
    while let Some((prefix, dir)) = pending.pop() {
        let entries = fs::read_dir(&dir)
            .map_err(|e| Error::new(format!("reading {}: {e}", dir.display())))?;
        for entry in entries {
            let entry = entry.map_err(|e| Error::new(format!("reading {}: {e}", dir.display())))?;
            let name = entry.file_name();
            let Some(name) = name.to_str() else {
                return Err(Error::new(format!(
                    "{} holds a file whose name is not UTF-8",
                    dir.display()
                )));
            };
            if prefix.is_empty() && EXCLUDED.contains(&name) {
                continue;
            }
            let relative = if prefix.is_empty() {
                name.to_owned()
            } else {
                format!("{prefix}/{name}")
            };
            let kind = entry
                .file_type()
                .map_err(|e| Error::new(format!("reading {}: {e}", entry.path().display())))?;
            if kind.is_symlink() {
                return Err(Error::new(format!(
                    "{relative} is a symbolic link, and a module cannot hold one"
                )));
            } else if kind.is_dir() {
                pending.push((relative, entry.path()));
            } else if kind.is_file() {
                out.push((relative, entry.path()));
            } else {
                return Err(Error::new(format!(
                    "{relative} is not a regular file, and a module cannot hold it"
                )));
            }
        }
    }
    out.sort();
    Ok(out)
}

/// The integrity hash of the module at `root`: `h1:` and the SHA-256 of a
/// listing with one line per file, `<sha-256 of the file>  <path>`, sorted
/// by path. The module's signature file is left out: it signs this hash.
pub fn hash_dir(root: &Path) -> Result<String, Error> {
    let mut listing = String::new();
    for (relative, path) in files(root)? {
        // The signature is over this hash, so it cannot be part of it.
        if relative == SIGNATURE_FILE {
            continue;
        }
        let content =
            fs::read(&path).map_err(|e| Error::new(format!("reading {}: {e}", path.display())))?;
        listing.push_str(&sha256_hex(&content));
        listing.push_str("  ");
        listing.push_str(&relative);
        listing.push('\n');
    }
    Ok(format!("h1:{}", sha256_hex(listing.as_bytes())))
}

/// Copies the module at `from` into the new directory `to`, leaving out what
/// is never part of a module.
pub(crate) fn copy_tree(from: &Path, to: &Path) -> Result<(), Error> {
    let io =
        |what: &Path, e: std::io::Error| Error::new(format!("writing {}: {e}", what.display()));
    fs::create_dir_all(to).map_err(|e| io(to, e))?;
    for (relative, path) in files(from)? {
        let dest = to.join(&relative);
        if let Some(parent) = dest.parent() {
            fs::create_dir_all(parent).map_err(|e| io(parent, e))?;
        }
        fs::copy(&path, &dest).map_err(|e| io(&dest, e))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tree(name: &str, entries: &[(&str, &str)]) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("meridian-integrity-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        for (path, content) in entries {
            let path = dir.join(path);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, content).unwrap();
        }
        dir
    }

    #[test]
    fn the_hash_is_of_the_listing_and_nothing_else() {
        let a = tree("a", &[("meridian.yaml", "x"), ("documents/web.yaml", "y")]);
        let listing = format!(
            "{}  documents/web.yaml\n{}  meridian.yaml\n",
            sha256_hex(b"y"),
            sha256_hex(b"x")
        );
        assert_eq!(
            hash_dir(&a).unwrap(),
            format!("h1:{}", sha256_hex(listing.as_bytes()))
        );
        // SHA-256 of the empty string, so the hex is checked against a known value.
        assert_eq!(
            sha256_hex(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        let _ = fs::remove_dir_all(a);
    }

    #[test]
    fn git_and_state_directories_are_not_part_of_a_module() {
        let plain = tree("plain", &[("meridian.yaml", "x")]);
        let noisy = tree(
            "noisy",
            &[
                ("meridian.yaml", "x"),
                (".git/HEAD", "ref"),
                (".meridian/modules/a", "b"),
            ],
        );
        assert_eq!(hash_dir(&plain).unwrap(), hash_dir(&noisy).unwrap());
        // Only at the top: a nested directory of that name is content.
        let nested = tree(
            "nested",
            &[("meridian.yaml", "x"), ("data/.git/HEAD", "ref")],
        );
        assert_ne!(hash_dir(&plain).unwrap(), hash_dir(&nested).unwrap());

        let copy = plain.with_extension("copy");
        let _ = fs::remove_dir_all(&copy);
        copy_tree(&noisy, &copy).unwrap();
        assert!(!copy.join(".git").exists());
        assert_eq!(hash_dir(&copy).unwrap(), hash_dir(&plain).unwrap());
        for dir in [plain, noisy, nested, copy] {
            let _ = fs::remove_dir_all(dir);
        }
    }

    #[test]
    fn any_change_to_a_file_or_its_name_changes_the_hash() {
        let a = tree("base", &[("meridian.yaml", "x"), ("data/d.yaml", "1")]);
        let b = tree("edited", &[("meridian.yaml", "x"), ("data/d.yaml", "2")]);
        let c = tree("renamed", &[("meridian.yaml", "x"), ("data/e.yaml", "1")]);
        let d = tree(
            "added",
            &[("meridian.yaml", "x"), ("data/d.yaml", "1"), ("z", "")],
        );
        let base = hash_dir(&a).unwrap();
        for other in [&b, &c, &d] {
            assert_ne!(hash_dir(other).unwrap(), base);
        }
        for dir in [a, b, c, d] {
            let _ = fs::remove_dir_all(dir);
        }
    }

    #[cfg(unix)]
    #[test]
    fn a_symbolic_link_is_refused() {
        let a = tree("link", &[("meridian.yaml", "x")]);
        std::os::unix::fs::symlink("/etc/passwd", a.join("data")).unwrap();
        assert_eq!(
            hash_dir(&a).unwrap_err().to_string(),
            "data is a symbolic link, and a module cannot hold one"
        );
        let _ = fs::remove_dir_all(a);
    }
}
