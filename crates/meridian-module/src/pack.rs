//! The archive a registry stores: a gzipped tar of a module's files.
//!
//! Packing is deterministic. The same files always give the same bytes,
//! whoever packs them and when, so a registry digest means something.

use std::fs;
use std::io::{Read, Write};
use std::path::Path;

use flate2::read::GzDecoder;
use flate2::{Compression, GzBuilder};

use crate::integrity::files;
use crate::manifest::safe_relative;
use crate::{Error, Kind};

/// The most a module may unpack to. A module is configuration; one this big
/// is a mistake or an attack.
pub const MAX_UNPACKED_BYTES: u64 = 256 << 20;

/// Packs the module at `root`.
pub fn pack(root: &Path) -> Result<Vec<u8>, Error> {
    let bad =
        |e: std::io::Error| Error::new(Kind::Pack, format!("packing {}: {e}", root.display()));
    let gz = GzBuilder::new()
        .mtime(0)
        .write(Vec::new(), Compression::default());
    let mut tar = tar::Builder::new(gz);
    for (relative, path) in files(root)? {
        let content = fs::read(&path)
            .map_err(|e| Error::new(Kind::Pack, format!("reading {}: {e}", path.display())))?;
        // No owner, no time, one mode: nothing about the machine that packed
        // it reaches the archive.
        let mut header = tar::Header::new_gnu();
        header.set_size(content.len() as u64);
        header.set_mode(0o644);
        header.set_mtime(0);
        header.set_uid(0);
        header.set_gid(0);
        header.set_entry_type(tar::EntryType::Regular);
        tar.append_data(&mut header, &relative, content.as_slice())
            .map_err(bad)?;
    }
    let mut gz = tar.into_inner().map_err(bad)?;
    gz.flush().map_err(bad)?;
    gz.finish().map_err(bad)
}

/// Unpacks an archive into the new directory `dest`.
///
/// An archive is input from somewhere else. Anything that is not a plain
/// file at a path inside the module is refused, and nothing is linked,
/// created outside `dest`, or given the archive's permissions.
pub fn unpack(archive: &[u8], dest: &Path) -> Result<(), Error> {
    let bad = |what: String| Error::new(Kind::Pack, format!("the module archive {what}"));
    let io = |e: std::io::Error| {
        Error::new(
            Kind::Pack,
            format!("the module archive cannot be read: {e}"),
        )
    };
    fs::create_dir_all(dest)
        .map_err(|e| Error::new(Kind::Pack, format!("writing {}: {e}", dest.display())))?;
    let mut tar = tar::Archive::new(GzDecoder::new(archive));
    let mut total: u64 = 0;
    for entry in tar.entries().map_err(io)? {
        let mut entry = entry.map_err(io)?;
        let path = entry.path().map_err(io)?.into_owned();
        let Some(relative) = path.to_str().map(|p| p.trim_end_matches('/').to_owned()) else {
            return Err(bad("holds a path that is not UTF-8".into()));
        };
        let kind = entry.header().entry_type();
        if kind.is_dir() {
            continue;
        }
        if !kind.is_file() {
            return Err(bad(format!(
                "holds {relative:?}, which is not a regular file"
            )));
        }
        if relative.contains(char::is_control) {
            return Err(bad(format!(
                "holds the path {relative:?}, which has a control character in it"
            )));
        }
        if !safe_relative(&relative) {
            return Err(bad(format!(
                "holds the path {relative:?}, which would leave the module"
            )));
        }
        total += entry.header().size().map_err(io)?;
        if total > MAX_UNPACKED_BYTES {
            return Err(bad(format!(
                "unpacks to more than {} MiB",
                MAX_UNPACKED_BYTES >> 20
            )));
        }
        let mut content = Vec::new();
        // Bounded by what is left, so a header that lies about its size
        // cannot make this read without end.
        (&mut entry)
            .take(MAX_UNPACKED_BYTES + 1)
            .read_to_end(&mut content)
            .map_err(io)?;
        let target = dest.join(&relative);
        let write = |e: std::io::Error| {
            Error::new(Kind::Pack, format!("writing {}: {e}", target.display()))
        };
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent).map_err(write)?;
        }
        if target.exists() {
            return Err(bad(format!("holds {relative:?} twice")));
        }
        fs::write(&target, content).map_err(write)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::integrity::hash_dir;
    use std::path::PathBuf;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("meridian-pack-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn archive(entries: &[(&str, tar::EntryType, &[u8])]) -> Vec<u8> {
        let mut tar = tar::Builder::new(GzBuilder::new().write(Vec::new(), Compression::fast()));
        for (path, kind, content) in entries {
            let mut header = tar::Header::new_gnu();
            header.set_size(content.len() as u64);
            header.set_mode(0o777);
            header.set_entry_type(*kind);
            // Written raw, so a hostile path is not cleaned up on the way in.
            let name = header.as_old_mut().name.as_mut();
            name[..path.len()].copy_from_slice(path.as_bytes());
            if kind.is_symlink() {
                header.set_link_name("/etc/passwd").unwrap();
            }
            header.set_cksum();
            tar.append(&header, *content).unwrap();
        }
        tar.into_inner().unwrap().finish().unwrap()
    }

    #[test]
    fn a_module_survives_packing_and_packs_the_same_every_time() {
        let src = scratch("src");
        fs::create_dir_all(src.join("documents")).unwrap();
        fs::create_dir_all(src.join(".git")).unwrap();
        fs::write(src.join("meridian.yaml"), "name").unwrap();
        fs::write(src.join("documents/web.yaml"), "doc").unwrap();
        fs::write(src.join(".git/HEAD"), "ref").unwrap();

        let first = pack(&src).unwrap();
        // A file's time is not part of the archive.
        fs::write(src.join("meridian.yaml"), "name").unwrap();
        assert_eq!(pack(&src).unwrap(), first);

        let out = scratch("out");
        unpack(&first, &out).unwrap();
        assert_eq!(hash_dir(&out).unwrap(), hash_dir(&src).unwrap());
        assert!(!out.join(".git").exists());
        assert_eq!(
            fs::read_to_string(out.join("documents/web.yaml")).unwrap(),
            "doc"
        );
        for dir in [src, out] {
            let _ = fs::remove_dir_all(dir);
        }
    }

    #[test]
    fn an_archive_cannot_write_outside_the_module_or_link_out_of_it() {
        use tar::EntryType::{Regular, Symlink};
        for (name, entries, want) in [
            (
                "dotdot",
                vec![("../escape.yaml", Regular, &b"x"[..])],
                "the module archive holds the path \"../escape.yaml\", which would leave the module",
            ),
            (
                "absolute",
                vec![("/tmp/meridian-escape", Regular, &b"x"[..])],
                "which would leave the module",
            ),
            (
                "symlink",
                vec![("data", Symlink, &b""[..])],
                "the module archive holds \"data\", which is not a regular file",
            ),
            (
                "newline",
                vec![("policy.yaml\nabc  zz.yaml", Regular, &b"x"[..])],
                "which has a control character in it",
            ),
            (
                "twice",
                vec![("a.yaml", Regular, &b"x"[..]), ("a.yaml", Regular, &b"y"[..])],
                "the module archive holds \"a.yaml\" twice",
            ),
        ] {
            let dest = scratch(name);
            let err = unpack(&archive(&entries), &dest).unwrap_err().to_string();
            assert!(err.contains(want), "{name}: {err}");
            assert!(!dest.parent().unwrap().join("escape.yaml").exists());
            let _ = fs::remove_dir_all(dest);
        }
        let dest = scratch("garbage");
        let err = unpack(b"not an archive", &dest).unwrap_err().to_string();
        assert!(
            err.starts_with("the module archive cannot be read: "),
            "{err}"
        );
        let _ = fs::remove_dir_all(dest);
    }
}
