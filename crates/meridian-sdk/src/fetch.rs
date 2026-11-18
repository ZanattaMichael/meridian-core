//! Fetching a vendor's published schema file for a schema source.
//!
//! Several tools publish the schemas of their resource types as a file
//! rather than through a command: CloudFormation's per-region zip of
//! resource provider schemas, the Azure type definitions Bicep compiles
//! against, the cloud-config and Compose JSON Schemas. A schema source for
//! such a target downloads the file instead of running a tool, and this
//! module is the one way it does so, so every file source keeps the same
//! rules:
//!
//! - **It is fetched over HTTPS only**, and a redirect may not leave HTTPS.
//! - **It is kept in the work directory** the host gives the source, with
//!   the `ETag` the server sent, so the next sync asks the server whether
//!   the file changed (`If-None-Match`) and downloads nothing when it has
//!   not.
//! - **A copy already in the work directory stands in when the network
//!   does not answer**, and the export says so in a note, so a sync on a
//!   machine that is offline for the moment does not lose what the last
//!   one fetched. There is no copy the first time, and then the sync fails
//!   for that target, as a tool that cannot be run would.
//! - **What was read is pinned by its SHA-256**, so `meridian.lock` tells a
//!   cache made from one revision of the file apart from another, as it
//!   tells provider versions apart.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use sha2::{Digest, Sha256};

use crate::{Log, RunError};

/// A fetched file may be large (CloudFormation's zip is a few megabytes;
/// the Azure index is ten); this is generous.
pub const MAX_FETCHED_BYTES: u64 = 256 << 20;

/// A file fetched into a work directory.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Fetched {
    /// Where the file is on disk.
    pub path: PathBuf,
    /// Its contents.
    pub bytes: Vec<u8>,
    /// The SHA-256 of the contents, lower-case hex, for a pin.
    pub sha256: String,
    /// `true` when the copy already on disk was used: the server said it
    /// had not changed, or could not be reached.
    pub reused: bool,
    /// What the operator should know, when the copy on disk stood in for
    /// a server that could not be reached.
    pub note: Option<String>,
}

impl Fetched {
    /// The pin for this file: `sha256:` and the first twelve hex digits,
    /// enough to tell revisions apart and short enough for a lock file.
    pub fn pin(&self) -> String {
        format!("sha256:{}", &self.sha256[..12.min(self.sha256.len())])
    }
}

/// Fetches `url` into `work_dir/name`, or reuses the copy there when the
/// server says it is unchanged or cannot be reached. `what` names the file
/// in messages, such as "the CloudFormation resource schemas".
pub fn fetch(
    url: &str,
    work_dir: &Path,
    name: &str,
    what: &str,
    log: &dyn Log,
) -> Result<Fetched, RunError> {
    if !url.starts_with("https://") {
        return Err(RunError::new(format!(
            "{what} would be fetched from {url}, which is not an https:// address"
        )));
    }
    fs::create_dir_all(work_dir)
        .map_err(|e| RunError::new(format!("could not create {}: {e}", work_dir.display())))?;
    let path = work_dir.join(name);
    let etag_path = work_dir.join(format!("{name}.etag"));
    let kept = fs::read(&path).ok();
    let kept_etag = fs::read_to_string(&etag_path)
        .ok()
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty() && kept.is_some());

    log.out(&format!("fetching {what} from {url}"));
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .http_status_as_error(false)
        .https_only(true)
        .timeout_global(Some(Duration::from_secs(600)))
        .timeout_connect(Some(Duration::from_secs(30)))
        .user_agent(concat!("meridian/", env!("CARGO_PKG_VERSION")))
        .build()
        .into();
    let mut request = agent.get(url);
    if let Some(etag) = &kept_etag {
        request = request.header("If-None-Match", etag.as_str());
    }
    let response = match request.call() {
        Ok(response) => response,
        Err(e) => {
            return match kept {
                Some(bytes) => {
                    let note = format!(
                        "{what} could not be fetched from {url} ({e}); the copy from the last \
                         sync was used"
                    );
                    log.err(&note);
                    Ok(done(path, bytes, true, Some(note)))
                }
                None => Err(RunError::new(format!(
                    "{what} could not be fetched from {url}: {e}"
                ))),
            };
        }
    };
    let status = response.status().as_u16();
    if status == 304 {
        if let Some(bytes) = kept {
            log.out(&format!("{what}: unchanged since the last sync"));
            return Ok(done(path, bytes, true, None));
        }
    }
    if !(200..300).contains(&status) {
        return Err(RunError::new(format!(
            "{what} could not be fetched: {url} answered {status}"
        )));
    }
    let etag = response
        .headers()
        .get("etag")
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned);
    let bytes = response
        .into_body()
        .with_config()
        .limit(MAX_FETCHED_BYTES)
        .read_to_vec()
        .map_err(|e| RunError::new(format!("reading {what} from {url}: {e}")))?;
    fs::write(&path, &bytes)
        .map_err(|e| RunError::new(format!("could not write {}: {e}", path.display())))?;
    match etag {
        Some(etag) => {
            let _ = fs::write(&etag_path, etag);
        }
        None => {
            let _ = fs::remove_file(&etag_path);
        }
    }
    log.out(&format!("{what}: {} bytes", bytes.len()));
    Ok(done(path, bytes, false, None))
}

fn done(path: PathBuf, bytes: Vec<u8>, reused: bool, note: Option<String>) -> Fetched {
    let sha256 = sha256_hex(&bytes);
    Fetched {
        path,
        bytes,
        sha256,
        reused,
        note,
    }
}

/// The SHA-256 of `bytes` as lower-case hex.
pub fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut out = String::with_capacity(64);
    for b in digest {
        use std::fmt::Write as _;
        let _ = write!(out, "{b:02x}");
    }
    out
}

/// The first paragraph of a description, which is what an editor shows
/// beside a name; the rest is documentation.
pub fn first_paragraph(text: &str) -> String {
    let text = text.trim();
    let end = text.find("\n\n").unwrap_or(text.len());
    text[..end].split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::NoLog;

    #[test]
    fn a_plain_http_address_is_refused_before_anything_is_fetched() {
        let dir = std::env::temp_dir().join("meridian-sdk-fetch-http");
        let e = fetch(
            "http://example.invalid/x.json",
            &dir,
            "x.json",
            "the file",
            &NoLog,
        )
        .unwrap_err();
        assert!(e.msg.contains("not an https:// address"), "{}", e.msg);
    }

    #[test]
    fn a_server_that_cannot_be_reached_falls_back_to_the_kept_copy() {
        let dir =
            std::env::temp_dir().join(format!("meridian-sdk-fetch-kept-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        // Nothing kept: the failure is an error.
        let url = "https://localhost:9/nothing.json";
        let e = fetch(url, &dir, "nothing.json", "the file", &NoLog).unwrap_err();
        assert!(e.msg.contains("could not be fetched"), "{}", e.msg);
        // A kept copy stands in, with a note.
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("nothing.json"), b"{}").unwrap();
        let fetched = fetch(url, &dir, "nothing.json", "the file", &NoLog).unwrap();
        assert!(fetched.reused);
        assert_eq!(fetched.bytes, b"{}");
        assert!(fetched.note.as_deref().unwrap().contains("last sync"));
        assert_eq!(fetched.pin(), "sha256:44136fa355b3");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_sha_and_pin_are_of_the_bytes() {
        assert_eq!(
            sha256_hex(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            first_paragraph("  A thing.\n  More.\n\nSecond."),
            "A thing. More."
        );
    }
}
