//! Modules from git, through the `git` program on the machine.
//!
//! Using the installed `git` means every transport and credential that works
//! for `git clone` works here, and Meridian stores no git credentials.

use std::collections::BTreeMap;
use std::path::Path;
use std::process::Command;

use semver::Version;

use crate::integrity::copy_tree;
use crate::{Error, Kind};

/// The `git` binary to run. `MERIDIAN_GIT` names another one.
fn git() -> Command {
    let program = std::env::var_os("MERIDIAN_GIT").unwrap_or_else(|| "git".into());
    let mut command = Command::new(program);
    // Never stop to ask for a password: a run that cannot authenticate
    // should fail, not hang.
    command.env("GIT_TERMINAL_PROMPT", "0");
    command
}

fn run(mut command: Command, doing: &str) -> Result<String, Error> {
    let output = command.output().map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            Error::new(
                Kind::Git,
                format!(
                    "{doing}: the `git` program was not found; a git module needs git installed"
                ),
            )
        } else {
            Error::new(Kind::Git, format!("{doing}: could not run git: {e}"))
        }
    })?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let said = stderr.trim();
        return Err(Error::new(
            Kind::Git,
            if said.is_empty() {
                format!("{doing}: git failed ({})", output.status)
            } else {
                format!("{doing}: git failed:\n{said}")
            },
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// Reads a tag name as a version: `v1.2.3` or `1.2.3`.
fn tag_version(tag: &str) -> Option<Version> {
    Version::parse(tag.strip_prefix('v').unwrap_or(tag)).ok()
}

/// The versions a repository offers, each with the tag that carries it.
pub(crate) fn versions(url: &str) -> Result<BTreeMap<Version, String>, Error> {
    let mut command = git();
    command.args(["ls-remote", "--tags", "--refs", "--", url]);
    let listing = run(command, &format!("listing the tags of {url}"))?;
    let mut out = BTreeMap::new();
    for line in listing.lines() {
        let Some(tag) = line
            .split_whitespace()
            .nth(1)
            .and_then(|r| r.strip_prefix("refs/tags/"))
        else {
            continue;
        };
        if let Some(version) = tag_version(tag) {
            // `v1.2.3` wins over a bare `1.2.3` for the same version.
            if tag.starts_with('v') || !out.contains_key(&version) {
                out.insert(version, tag.to_owned());
            }
        }
    }
    Ok(out)
}

fn is_commit(reference: &str) -> bool {
    reference.len() == 40 && reference.bytes().all(|b| b.is_ascii_hexdigit())
}

/// Fetches `reference` (a tag, branch or commit) of `url` and copies the
/// module in `subdir` into the new directory `dest`. Returns the commit.
pub(crate) fn fetch(
    url: &str,
    reference: &str,
    subdir: Option<&str>,
    scratch: &Path,
    dest: &Path,
) -> Result<String, Error> {
    let doing = format!("fetching {reference} of {url}");
    let repo = scratch.join("repo");
    std::fs::create_dir_all(&repo).map_err(|e| {
        Error::new(
            Kind::Git,
            format!("{doing}: creating {}: {e}", repo.display()),
        )
    })?;
    let at = |args: &[&str]| {
        let mut command = git();
        command.arg("-C").arg(&repo).args(args);
        command
    };
    run(at(&["init", "--quiet"]), &doing)?;

    // The one thing asked for, and no history. A server may refuse to serve
    // a bare commit that no branch or tag points at; everything is fetched
    // then, and the commit looked up in it.
    let shallow = run(
        at(&["fetch", "--quiet", "--depth", "1", "--", url, reference]),
        &doing,
    );
    let commit = match shallow {
        Ok(_) => run(
            at(&["rev-parse", "--verify", "FETCH_HEAD^{commit}"]),
            &doing,
        )?,
        Err(first) => {
            let all = [
                "fetch",
                "--quiet",
                "--tags",
                "--",
                url,
                "+refs/heads/*:refs/remotes/origin/*",
            ];
            if run(at(&all), &doing).is_err() {
                return Err(first);
            }
            let candidates = [
                format!("{reference}^{{commit}}"),
                format!("refs/remotes/origin/{reference}^{{commit}}"),
            ];
            let found = candidates
                .iter()
                .find_map(|c| run(at(&["rev-parse", "--verify", "--quiet", c]), &doing).ok());
            match found {
                Some(commit) => commit,
                None => return Err(first),
            }
        }
    };
    let commit = commit.trim().to_owned();
    if !is_commit(&commit) {
        return Err(Error::new(
            Kind::Git,
            format!("{doing}: git did not name a commit ({commit:?})"),
        ));
    }

    // Symbolic links are checked out as links, never followed here, and a
    // module holding one is refused when it is copied.
    run(
        at(&[
            "-c",
            "core.symlinks=true",
            "-c",
            "advice.detachedHead=false",
            "checkout",
            "--quiet",
            "--detach",
            &commit,
        ]),
        &doing,
    )?;

    let root = match subdir {
        Some(subdir) => repo.join(subdir),
        None => repo.clone(),
    };
    if !root.is_dir() {
        return Err(Error::new(
            Kind::Git,
            format!(
                "{doing}: the repository has no directory {:?}",
                subdir.unwrap_or_default()
            ),
        ));
    }
    copy_tree(&root, dest).map_err(|e| Error::new(Kind::Git, format!("{doing}: {e}")))?;
    Ok(commit)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tags_are_versions_with_or_without_a_v() {
        assert_eq!(tag_version("v1.2.3"), Some(Version::new(1, 2, 3)));
        assert_eq!(tag_version("1.2.3"), Some(Version::new(1, 2, 3)));
        assert_eq!(
            tag_version("v2.0.0-rc.1"),
            Some(Version::parse("2.0.0-rc.1").unwrap())
        );
        for not in ["latest", "v1", "v1.2", "release-1.2.3", "vv1.2.3"] {
            assert_eq!(tag_version(not), None, "{not}");
        }
    }

    #[test]
    fn only_a_full_hash_is_taken_for_a_commit() {
        assert!(is_commit("4f2a9c1e5b3d8f6a7c2e4b1d0f9a8c79c1d7be0a"));
        assert!(!is_commit("4f2a9c1"));
        assert!(!is_commit("main"));
    }
}
