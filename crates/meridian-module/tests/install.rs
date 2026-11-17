//! Resolving, fetching and pinning modules, against real git repositories on
//! disk and modules required by path.

#![cfg(unix)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use meridian_module::{install, verify, Change, Lock, Mode, Modules, Project, LOCK_FILE};

const CORE: &str = "0.1.0";

fn scratch(test: &str) -> PathBuf {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR"))
        .join("install")
        .join(test);
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

fn write(path: &Path, content: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, content).unwrap();
}

fn manifest(name: &str, version: &str, modules: &str) -> String {
    let version = if version.is_empty() {
        String::new()
    } else {
        format!("  version: {version}\n")
    };
    let spec = if modules.is_empty() {
        String::new()
    } else {
        format!("spec:\n  modules:\n{modules}")
    };
    format!("apiVersion: meridian/v1\nkind: Module\nmetadata:\n  name: {name}\n{version}{spec}")
}

fn git(repo: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args([
            "-c",
            "user.name=Meridian Tests",
            "-c",
            "user.email=tests@example.com",
            "-c",
            "commit.gpgsign=false",
            "-c",
            "tag.gpgsign=false",
        ])
        .args(args)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .output()
        .expect("git runs");
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_owned()
}

/// A git repository holding one module, with a commit and a tag for each
/// release. Returns its `file://` address.
struct Repo {
    dir: PathBuf,
}

impl Repo {
    fn new(dir: PathBuf) -> Repo {
        fs::create_dir_all(&dir).unwrap();
        git(&dir, &["init", "--quiet", "--initial-branch=main"]);
        Repo { dir }
    }

    fn url(&self) -> String {
        format!("file://{}", self.dir.display())
    }

    /// Commits the files, and tags the commit when `tag` is not empty.
    fn release(&self, tag: &str, files: &[(&str, &str)]) -> String {
        for (path, content) in files {
            write(&self.dir.join(path), content);
        }
        git(&self.dir, &["add", "--all"]);
        git(
            &self.dir,
            &["commit", "--quiet", "--allow-empty", "-m", "release"],
        );
        if !tag.is_empty() {
            git(&self.dir, &["tag", "--force", tag]);
        }
        git(&self.dir, &["rev-parse", "HEAD"])
    }
}

/// A project directory with a manifest requiring `modules`.
struct Site {
    dir: PathBuf,
    state: PathBuf,
}

impl Site {
    fn new(root: &Path, modules: &str) -> Site {
        let site = Site {
            dir: root.join("site"),
            state: root.join("site/.meridian"),
        };
        site.require(modules);
        site
    }

    fn require(&self, modules: &str) {
        write(
            &self.dir.join("meridian.yaml"),
            &manifest("site", "", modules),
        );
    }

    fn project(&self) -> Project<'_> {
        Project {
            dir: &self.dir,
            state_dir: &self.state,
            core_version: CORE,
            signatures: &meridian_module::NO_SIGNATURES,
        }
    }

    fn install(&self, mode: Mode) -> Result<meridian_module::Report, String> {
        install(&self.project(), mode).map_err(|e| e.to_string())
    }

    fn lock_text(&self) -> String {
        fs::read_to_string(self.dir.join(LOCK_FILE)).unwrap_or_default()
    }

    fn pinned(&self) -> Vec<(String, String)> {
        Lock::load(&self.dir)
            .unwrap()
            .unwrap_or_default()
            .modules
            .into_iter()
            .map(|m| (m.name, m.version))
            .collect()
    }
}

fn pair(name: &str, version: &str) -> (String, String) {
    (name.to_owned(), version.to_owned())
}

fn nginx_repo(root: &Path) -> Repo {
    let repo = Repo::new(root.join("nginx.git"));
    for version in ["1.0.0", "1.1.0", "2.0.0"] {
        repo.release(
            &format!("v{version}"),
            &[
                ("meridian.yaml", &manifest("acme/nginx", version, "")),
                ("documents/proxy.yaml", &format!("# nginx {version}\n")),
            ],
        );
    }
    repo.release("not-a-version", &[("README.md", "notes\n")]);
    repo
}

#[test]
fn the_newest_version_that_fits_is_pinned_and_stays_pinned() {
    let root = scratch("git");
    let repo = nginx_repo(&root);
    let require = |req: &str| {
        format!(
            "    - name: acme/nginx\n      version: \"{req}\"\n      git: {}\n",
            repo.url()
        )
    };
    let site = Site::new(&root, &require("^1"));

    let report = site.install(Mode::Install).unwrap();
    assert!(report.wrote_lock);
    assert_eq!(site.pinned(), [pair("acme/nginx", "1.1.0")]);
    assert_eq!(report.changes[0].1, Change::Added);
    let pin = &report.lock.modules[0];
    assert_eq!(pin.source, format!("git+{}", repo.url()));
    assert_eq!(
        pin.resolved,
        git(&repo.dir, &["rev-parse", "v1.1.0^{commit}"])
    );
    assert!(
        pin.integrity.starts_with("h1:") && pin.integrity.len() == 67,
        "{pin:?}"
    );

    let modules = Modules::load(&site.project()).unwrap();
    let file = modules.resolve("@acme/nginx:documents/proxy.yaml").unwrap();
    assert_eq!(fs::read_to_string(&file).unwrap(), "# nginx 1.1.0\n");
    assert_eq!(
        modules.root("acme/nginx").unwrap(),
        site.state.join("modules/acme/nginx/1.1.0")
    );
    // Git's own directory is not part of the module.
    assert!(!modules.root("acme/nginx").unwrap().join(".git").exists());

    // Installing again changes nothing, and neither does a newer release.
    let before = site.lock_text();
    repo.release(
        "v1.2.0",
        &[("meridian.yaml", &manifest("acme/nginx", "1.2.0", ""))],
    );
    let again = site.install(Mode::Install).unwrap();
    assert!(!again.wrote_lock);
    assert_eq!(again.changes[0].1, Change::Unchanged);
    assert_eq!(site.lock_text(), before);
    site.install(Mode::Locked).unwrap();

    // Only an update moves a pin, and it says where from.
    let updated = site.install(Mode::Update(vec![])).unwrap();
    assert_eq!(site.pinned(), [pair("acme/nginx", "1.2.0")]);
    assert_eq!(
        updated.changes[0].1,
        Change::Updated {
            from: "1.1.0".into()
        }
    );

    // A requirement the pin no longer fits moves it on the next install,
    // and --locked refuses to.
    site.require(&require("^2"));
    let err = site.install(Mode::Locked).unwrap_err();
    assert_eq!(
        err,
        "meridian.lock is out of date with meridian.yaml (acme/nginx would change); run \
         `meridian module install` and commit the result"
    );
    assert_eq!(site.pinned(), [pair("acme/nginx", "1.2.0")]);
    let err = Modules::load(&site.project()).unwrap_err().to_string();
    assert_eq!(
        err,
        "meridian.lock is out of date with meridian.yaml: acme/nginx is pinned at 1.2.0 and \
         required as ^2; run `meridian module install`"
    );
    site.install(Mode::Install).unwrap();
    assert_eq!(site.pinned(), [pair("acme/nginx", "2.0.0")]);

    // No longer required: the pin goes, and the report says so.
    site.require("");
    let report = site.install(Mode::Install).unwrap();
    assert_eq!(report.changes[0].1, Change::Removed);
    assert!(report.lock.modules.is_empty());
}

#[test]
fn a_requirement_nothing_fits_names_who_asked_and_what_exists() {
    let root = scratch("conflict");
    let nginx = nginx_repo(&root);
    let base = Repo::new(root.join("base.git"));
    for version in ["1.0.0", "1.3.0", "2.1.0"] {
        base.release(
            &format!("v{version}"),
            &[("meridian.yaml", &manifest("acme/base", version, ""))],
        );
    }
    let app = Repo::new(root.join("app.git"));
    let needs_base = |req: &str| {
        format!(
            "    - name: acme/base\n      version: \"{req}\"\n      git: {}\n",
            base.url()
        )
    };
    app.release(
        "v1.0.0",
        &[(
            "meridian.yaml",
            &manifest("acme/app", "1.0.0", &needs_base("^1")),
        )],
    );
    let app_entry = format!(
        "    - name: acme/app\n      version: \"1\"\n      git: {}\n",
        app.url()
    );

    // The project alone would take base 2.1.0; app narrows it to 1.x, and
    // one version of base serves both.
    let site = Site::new(&root, &format!("{}{app_entry}", needs_base(">=1")));
    let report = site.install(Mode::Install).unwrap();
    assert_eq!(
        site.pinned(),
        [pair("acme/app", "1.0.0"), pair("acme/base", "1.3.0")]
    );
    assert_eq!(report.lock.get("acme/app").unwrap().requires, ["acme/base"]);
    assert!(site.lock_text().contains("    requires: [acme/base]\n"));

    // Nothing is both 2.x and 1.x.
    site.require(&format!("{}{app_entry}", needs_base("^2")));
    assert_eq!(
        site.install(Mode::Install).unwrap_err(),
        "no version of acme/base satisfies every requirement on it\n    \
         ^2   required by the project\n    \
         ^1   required by acme/app 1.0.0\n    \
         available: 1.0.0, 1.3.0, 2.1.0"
    );

    // The same name from somewhere else is a conflict too.
    site.require(&format!(
        "    - name: acme/base\n      version: \"1\"\n      git: {}\n{app_entry}",
        nginx.url()
    ));
    let err = site.install(Mode::Install).unwrap_err();
    assert!(
        err.starts_with("acme/base is required from two different sources\n"),
        "{err}"
    );

    // And what a source holds must be what it was required as.
    site.require(&format!(
        "    - name: acme/web\n      version: \"1\"\n      git: {}\n",
        nginx.url()
    ));
    let err = site.install(Mode::Install).unwrap_err();
    assert!(
        err.ends_with("holds the module acme/nginx, and it was required as acme/web"),
        "{err}"
    );
}

#[test]
fn a_changed_module_is_caught_wherever_the_change_was_made() {
    let root = scratch("tamper");
    let repo = nginx_repo(&root);
    let site = Site::new(
        &root,
        &format!(
            "    - name: acme/nginx\n      version: \"=1.1.0\"\n      git: {}\n",
            repo.url()
        ),
    );
    site.install(Mode::Install).unwrap();
    assert!(verify(&site.project())
        .unwrap()
        .iter()
        .all(|c| c.problem.is_none()));
    let stored = site.state.join("modules/acme/nginx/1.1.0");

    // A file edited in the store.
    fs::write(stored.join("documents/proxy.yaml"), "# edited\n").unwrap();
    let err = Modules::load(&site.project()).unwrap_err().to_string();
    assert!(
        err.starts_with(&format!(
            "acme/nginx 1.1.0 in {} does not match meridian.lock\n    pinned: h1:",
            stored.display()
        )),
        "{err}"
    );
    assert!(
        err.ends_with("Delete that directory and run `meridian module install`."),
        "{err}"
    );
    let checks = verify(&site.project()).unwrap();
    assert!(checks[0]
        .problem
        .as_ref()
        .unwrap()
        .contains("does not match"));

    // Installing puts back what was pinned.
    site.install(Mode::Install).unwrap();
    assert_eq!(
        fs::read_to_string(stored.join("documents/proxy.yaml")).unwrap(),
        "# nginx 1.1.0\n"
    );

    // Not there at all.
    fs::remove_dir_all(&stored).unwrap();
    assert_eq!(
        Modules::load(&site.project()).unwrap_err().to_string(),
        "acme/nginx 1.1.0 is not installed; run `meridian module install`"
    );
    // A fresh machine fetches exactly the pinned commit.
    let lock_before = site.lock_text();
    site.install(Mode::Locked).unwrap();
    assert_eq!(site.lock_text(), lock_before);
    Modules::load(&site.project()).unwrap();

    // The tag moved to other content after it was pinned. The commit that
    // was pinned is still what is fetched, so nothing changes...
    repo.release(
        "v1.1.0",
        &[
            ("meridian.yaml", &manifest("acme/nginx", "1.1.0", "")),
            ("documents/proxy.yaml", "# not what was published\n"),
        ],
    );
    fs::remove_dir_all(&stored).unwrap();
    site.install(Mode::Locked).unwrap();
    assert_eq!(
        fs::read_to_string(stored.join("documents/proxy.yaml")).unwrap(),
        "# nginx 1.1.0\n"
    );

    // ...and if the lockfile's own hash is edited, the fetch is refused.
    let lock = site.lock_text();
    let start = lock.find("integrity: h1:").unwrap() + "integrity: h1:".len();
    let mut forged = lock.clone();
    forged.replace_range(start..start + 4, "0000");
    fs::write(site.dir.join(LOCK_FILE), &forged).unwrap();
    fs::remove_dir_all(&stored).unwrap();
    let err = site.install(Mode::Install).unwrap_err();
    assert!(
        err.starts_with("acme/nginx 1.1.0: what git+file://")
            && err.contains("does not match meridian.lock\n    pinned:  h1:0000"),
        "{err}"
    );
    assert!(
        err.ends_with("run `meridian module update acme/nginx`."),
        "{err}"
    );
}

#[test]
fn a_ref_pins_a_commit_until_it_is_updated() {
    let root = scratch("ref");
    let repo = Repo::new(root.join("tools.git"));
    let first = repo.release(
        "",
        &[
            ("modules/tools/meridian.yaml", &manifest("tools", "", "")),
            ("modules/tools/data/defaults.yaml", "a: 1\n"),
            ("unrelated.txt", "not part of the module\n"),
        ],
    );
    let site = Site::new(
        &root,
        &format!(
            "    - name: tools\n      git: {}\n      ref: main\n      subdir: modules/tools\n",
            repo.url()
        ),
    );
    let report = site.install(Mode::Install).unwrap();
    let pin = &report.lock.modules[0];
    assert_eq!(pin.resolved, first);
    assert_eq!(pin.version, "");
    assert_eq!(
        pin.source,
        format!("git+{}//modules/tools#main", repo.url())
    );

    let modules = Modules::load(&site.project()).unwrap();
    let stored = modules.root("tools").unwrap();
    assert_eq!(
        stored,
        site.state
            .join(format!("modules/tools/ref-{}", &first[..12]))
    );
    assert!(!stored.join("unrelated.txt").exists());
    assert!(stored.join("data/defaults.yaml").is_file());

    // The branch moves on; the pin does not, even on a machine with nothing
    // fetched yet.
    let second = repo.release("", &[("modules/tools/data/defaults.yaml", "a: 2\n")]);
    fs::remove_dir_all(&site.state).unwrap();
    assert!(!site.install(Mode::Install).unwrap().wrote_lock);
    assert_eq!(
        fs::read_to_string(stored.join("data/defaults.yaml")).unwrap(),
        "a: 1\n"
    );

    let report = site.install(Mode::Update(vec!["tools".into()])).unwrap();
    assert_eq!(report.lock.modules[0].resolved, second);
    assert_eq!(report.changes[0].1, Change::Updated { from: first });

    assert_eq!(
        site.install(Mode::Update(vec!["nope".into()])).unwrap_err(),
        "nope is not a module in meridian.lock, so there is nothing to update"
    );
}

#[test]
fn a_path_module_is_read_where_it_is_and_never_hashed() {
    let root = scratch("path");
    write(
        &root.join("modules/base/meridian.yaml"),
        &manifest(
            "acme/base",
            "0.3.0",
            "    - name: acme/shared\n      path: ../shared\n",
        ),
    );
    write(&root.join("modules/base/documents/users.yaml"), "# users\n");
    write(
        &root.join("modules/shared/meridian.yaml"),
        &manifest("acme/shared", "", ""),
    );
    let site = Site::new(
        &root,
        "    - name: acme/base\n      version: \"0.3\"\n      path: ../modules/base\n",
    );
    site.install(Mode::Install).unwrap();
    assert_eq!(
        site.lock_text(),
        "# Written by `meridian module`. Commit it; do not edit it.\n\
         lockVersion: 1\n\
         modules:\n  \
         - name: acme/base\n    \
         version: \"0.3.0\"\n    \
         source: path+../modules/base\n    \
         requires: [acme/shared]\n  \
         - name: acme/shared\n    \
         source: path+../modules/shared\n"
    );
    // Nothing was copied.
    assert!(!site.state.join("modules/acme").exists());

    // An edit takes effect at once, with no install in between.
    let modules = Modules::load(&site.project()).unwrap();
    let file = modules.resolve("@acme/base:documents/users.yaml").unwrap();
    write(
        &root.join("modules/base/documents/users.yaml"),
        "# edited\n",
    );
    assert_eq!(fs::read_to_string(file).unwrap(), "# edited\n");
    Modules::load(&site.project()).unwrap();

    // A version requirement on a path module is still a requirement.
    site.require("    - name: acme/base\n      version: \"^1\"\n      path: ../modules/base\n");
    let err = site.install(Mode::Install).unwrap_err();
    assert!(
        err.starts_with("acme/base at ")
            && err.ends_with("has version 0.3.0, and the project requires ^1"),
        "{err}"
    );
}

#[test]
fn a_reference_names_exactly_one_file_inside_a_module() {
    let root = scratch("reference");
    write(&root.join("base/meridian.yaml"), &manifest("base", "", ""));
    write(&root.join("base/documents/a.yaml"), "a\n");
    write(&root.join("secret.yaml"), "outside\n");
    let site = Site::new(&root, "    - name: base\n      path: ../base\n");
    site.install(Mode::Install).unwrap();
    let modules = Modules::load(&site.project()).unwrap();
    assert_eq!(modules.names().collect::<Vec<_>>(), ["base"]);
    assert!(Modules::is_reference("@base:documents/a.yaml"));
    assert!(!Modules::is_reference("documents/a.yaml"));

    for (reference, want) in [
        ("@base", "a file in a module is written @<module>:<path>"),
        (
            "@other:documents/a.yaml",
            "other is not a module of this project (it has base)",
        ),
        (
            "@base:../secret.yaml",
            "\"../secret.yaml\" is not a path inside the module base",
        ),
        ("@base:/etc/passwd", "is not a path inside the module base"),
        (
            "@base:documents/missing.yaml",
            "the module base has no file documents/missing.yaml",
        ),
        ("@base:documents", "the module base has no file documents"),
    ] {
        let err = modules.resolve(reference).unwrap_err().to_string();
        assert!(
            err.starts_with(&format!("{reference}: ")) && err.contains(want),
            "{err}"
        );
    }
}

#[test]
fn a_module_is_refused_for_what_it_asks_of_the_core_or_of_the_disk() {
    let root = scratch("refusals");
    let future = Repo::new(root.join("future.git"));
    future.release(
        "v1.0.0",
        &[(
            "meridian.yaml",
            "apiVersion: meridian/v1\nkind: Module\nmetadata:\n  name: future\n  version: 1.0.0\nspec:\n  meridian: \">=9.0.0\"\n",
        )],
    );
    let site = Site::new(
        &root,
        &format!(
            "    - name: future\n      version: \"1\"\n      git: {}\n",
            future.url()
        ),
    );
    assert_eq!(
        site.install(Mode::Install).unwrap_err(),
        "future 1.0.0 needs meridian >=9.0.0, and this is meridian 0.1.0"
    );

    // A fetched module cannot reach onto the disk of whoever fetched it.
    let reaching = Repo::new(root.join("reaching.git"));
    reaching.release(
        "v1.0.0",
        &[(
            "meridian.yaml",
            &manifest(
                "reaching",
                "1.0.0",
                "    - name: victim\n      path: ../../../../etc\n",
            ),
        )],
    );
    site.require(&format!(
        "    - name: reaching\n      version: \"1\"\n      git: {}\n",
        reaching.url()
    ));
    assert_eq!(
        site.install(Mode::Install).unwrap_err(),
        "reaching 1.0.0 requires victim by `path`, which only works between modules on disk; \
         a fetched module must name a `git` or `oci` source"
    );

    // A tag whose manifest says another version is not that version.
    let mislabelled = Repo::new(root.join("mislabelled.git"));
    mislabelled.release(
        "v1.5.0",
        &[("meridian.yaml", &manifest("mislabelled", "1.4.0", ""))],
    );
    site.require(&format!(
        "    - name: mislabelled\n      version: \"1\"\n      git: {}\n",
        mislabelled.url()
    ));
    let err = site.install(Mode::Install).unwrap_err();
    assert!(err.ends_with("holds a manifest that says 1.4.0"), "{err}");

    // A repository that is not there.
    site.require(&format!(
        "    - name: gone\n      version: \"1\"\n      git: file://{}/gone.git\n",
        root.display()
    ));
    let err = site.install(Mode::Install).unwrap_err();
    assert!(err.starts_with("listing the tags of file://"), "{err}");

    // No manifest at all.
    let empty = scratch("refusals-empty");
    let err = install(
        &Project {
            dir: &empty,
            state_dir: &empty.join(".meridian"),
            core_version: CORE,
            signatures: &meridian_module::NO_SIGNATURES,
        },
        Mode::Install,
    )
    .unwrap_err()
    .to_string();
    assert!(
        err.ends_with(
            "meridian.yaml: not found; a module is a directory with a meridian.yaml in it"
        ),
        "{err}"
    );
}
