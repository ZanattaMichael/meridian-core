//! The shell scripts the runner hands to a host, and what they say back.
//!
//! Everything done on a host is one POSIX shell script per step, read from
//! standard input. The same script runs under `sh` on the machine running
//! Meridian and under `ssh` on another one, so a remote host needs nothing
//! installed but Podman, and there is one code path to test.
//!
//! A script reports through lines that start with `@meridian `. Everything a
//! tool prints for a person goes to standard error, which is relayed as it
//! appears.

use std::collections::{BTreeMap, BTreeSet};

use crate::runner::Scope;

/// What marks a line of a script's output as meant for the runner.
const MARK: &str = "@meridian ";

/// Where Quadlet's generator is installed, by distribution.
const GENERATORS: &[&str] = &[
    "/usr/libexec/podman/quadlet",
    "/usr/lib/podman/quadlet",
    "/usr/local/libexec/podman/quadlet",
    "/usr/lib/systemd/system-generators/podman-system-generator",
];

/// One word of a shell command, quoted so it means itself.
pub(crate) fn quote(word: &str) -> String {
    format!("'{}'", word.replace('\'', "'\\''"))
}

/// The programs and places a script uses on the host.
pub(crate) struct Settings<'a> {
    pub scope: Scope,
    pub unit_dir: Option<&'a str>,
    pub generator: Option<&'a str>,
    pub podman: &'a str,
    pub systemctl: &'a str,
    pub journalctl: &'a str,
    pub sudo: &'a str,
    /// The `--tool-debug` level: see [`crate::runner::DEBUG`].
    pub debug: u32,
}

/// A unit file the document wants on the host.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Desired {
    pub file: String,
    pub service: String,
    pub content: String,
}

impl Settings<'_> {
    /// What every script starts with: where the units live, how systemd is
    /// addressed, and how a command is run with the rights it needs.
    fn preamble(&self) -> String {
        let mut out = format!(
            "set -u\npodman={}\nsystemctl={}\njournalctl={}\nsudo={}\n",
            quote(self.podman),
            quote(self.systemctl),
            quote(self.journalctl),
            quote(self.sudo),
        );
        out.push_str(&format!(
            "lines={}\n",
            if self.debug >= 1 { 100 } else { 30 }
        ));
        match self.scope {
            Scope::System => out.push_str(
                "mode=''\n\
                 dir='/etc/containers/systemd'\n\
                 if [ \"$(id -u)\" -ne 0 ]; then root=1; else root=0; fi\n",
            ),
            Scope::User => out.push_str(
                "mode='--user'\n\
                 dir=\"${XDG_CONFIG_HOME:-${HOME:-}/.config}/containers/systemd\"\n\
                 root=0\n\
                 if [ -z \"${XDG_RUNTIME_DIR:-}\" ]; then\n\
                 \x20 XDG_RUNTIME_DIR=\"/run/user/$(id -u)\"; export XDG_RUNTIME_DIR\n\
                 fi\n",
            ),
        }
        match self.unit_dir {
            Some(dir) => out.push_str(&format!("dir={}\n", quote(dir))),
            None if self.scope == Scope::User => out.push_str(
                "if [ -z \"${XDG_CONFIG_HOME:-}${HOME:-}\" ]; then\n\
                 \x20 echo 'neither HOME nor XDG_CONFIG_HOME is set, so there is nowhere to keep a user'\\''s units' >&2\n\
                 \x20 exit 69\n\
                 fi\n",
            ),
            None => {}
        }
        // `run` is for what changes the host; reading it needs no more
        // rights than the operator has, so a plan never asks for any.
        out.push_str(
            "run() { if [ \"$root\" = 1 ]; then \"$sudo\" -n \"$@\"; else \"$@\"; fi; }\n\
             ctl() { if [ -n \"$mode\" ]; then run \"$systemctl\" \"$mode\" \"$@\"; \
             else run \"$systemctl\" \"$@\"; fi; }\n\
             look() { if [ -n \"$mode\" ]; then \"$systemctl\" \"$mode\" \"$@\"; \
             else \"$systemctl\" \"$@\"; fi; }\n",
        );
        out
    }

    /// Compares the units a document wants with what the host has, asks
    /// systemd which services are running, and has Quadlet itself read the
    /// wanted units. It changes nothing.
    pub fn probe(&self, desired: &[Desired], stale: &[String]) -> String {
        let mut out = self.preamble();
        out.push_str(
            "tmp=$(mktemp -d) || { echo 'could not create a temporary directory' >&2; exit 70; }\n\
             trap 'rm -rf \"$tmp\"' EXIT\n",
        );
        for unit in desired {
            out.push_str(&here_document(
                &format!("cat > \"$tmp\"/{}", quote(&unit.file)),
                &unit.content,
                " || exit 70",
            ));
        }
        if !desired.is_empty() {
            out.push_str(&format!(
                "for f in {}; do\n\
                 \x20 if [ ! -e \"$dir/$f\" ]; then echo \"{MARK}file new $f\"\n\
                 \x20 elif cmp -s \"$tmp/$f\" \"$dir/$f\"; then echo \"{MARK}file same $f\"\n\
                 \x20 else echo \"{MARK}file changed $f\"; fi\n\
                 done\n",
                words(desired.iter().map(|u| u.file.as_str()))
            ));
            out.push_str(&format!(
                "for s in {}; do\n\
                 \x20 state=$(look is-active \"$s\" 2>/dev/null)\n\
                 \x20 echo \"{MARK}active $s ${{state:-unknown}}\"\n\
                 done\n",
                words(desired.iter().map(|u| u.service.as_str()))
            ));
        }
        if !stale.is_empty() {
            out.push_str(&format!(
                "for f in {}; do\n\
                 \x20 if [ -e \"$dir/$f\" ]; then echo \"{MARK}stale $f\"; fi\n\
                 done\n",
                words(stale.iter().map(String::as_str))
            ));
        }

        out.push_str(&format!(
            "echo \"{MARK}podman $(\"$podman\" --version 2>/dev/null)\"\n"
        ));
        // A generator named by the operator that is not there is as missing
        // as one that was never installed.
        out.push_str(&format!(
            "gen={}\nif [ -n \"$gen\" ] && [ ! -x \"$gen\" ]; then gen=''; skip=1; else skip=0; fi\n",
            quote(self.generator.unwrap_or(""))
        ));
        out.push_str(&format!(
            "if [ -z \"$gen\" ] && [ \"$skip\" = 0 ]; then\n\
             \x20 for c in {}; do\n\
             \x20   if [ -x \"$c\" ]; then gen=$c; break; fi\n\
             \x20 done\n\
             fi\n",
            GENERATORS.join(" ")
        ));
        let flag = match self.scope {
            Scope::User => " -user",
            Scope::System => "",
        };
        out.push_str(&format!(
            "if [ -z \"$gen\" ]; then echo \"{MARK}generator missing\"\n\
             else\n\
             \x20 QUADLET_UNIT_DIRS=\"$tmp\" \"$gen\" -dryrun{flag} > \"$tmp/.generated\" 2> \"$tmp/.said\"\n\
             \x20 echo \"{MARK}generator $?\"\n\
             \x20 sed -n 's/^---\\(.*\\)---$/{MARK}generated \\1/p' \"$tmp/.generated\"\n\
             \x20 if [ {show} = 1 ]; then cat \"$tmp/.generated\" >&2; fi\n\
             \x20 grep -v 'Loading source unit file' \"$tmp/.said\" \\\n\
             \x20   | sed -e 's/^quadlet-generator\\[[0-9]*\\]: //' -e \"s|$tmp/||g\" -e 's/^/{MARK}said /'\n\
             fi\n\
             exit 0\n",
            show = u8::from(self.debug >= 3)
        ));
        out
    }

    /// Makes the host match: stops and removes what the document no longer
    /// has, writes what is new or changed, reloads systemd, restarts what
    /// changed and starts everything.
    pub fn apply(&self, steps: &Steps) -> String {
        let mut out = self.preamble();
        out.push_str(&format!("status={}\n", u8::from(self.debug >= 2)));
        out.push_str(
            "fail() {\n\
             \x20 code=$1; shift\n\
             \x20 for s in \"$@\"; do\n\
             \x20   if ! look is-active --quiet \"$s\" 2>/dev/null; then\n\
             \x20     echo \"--- the journal of $s ---\" >&2\n\
             \x20     if [ -n \"$mode\" ]; then run \"$journalctl\" \"$mode\" -u \"$s\" -n \"$lines\" --no-pager >&2\n\
             \x20     else run \"$journalctl\" -u \"$s\" -n \"$lines\" --no-pager >&2; fi\n\
             \x20     if [ \"$status\" = 1 ]; then look status \"$s\" --no-pager >&2 || true; fi\n\
             \x20   fi\n\
             \x20 done\n\
             \x20 exit \"$code\"\n\
             }\n\
             run mkdir -p \"$dir\" >&2 || exit 71\n",
        );
        for (file, service) in &steps.remove {
            // A unit that is already stopped, or was never started, is not a
            // reason to leave its file behind.
            out.push_str(&format!(
                "echo {} >&2\n\
                 ctl stop {} >&2 || true\n\
                 run rm -f \"$dir\"/{} >&2 || exit 72\n\
                 echo \"{MARK}removed {file}\"\n",
                quote(&format!("Stopping {service} and removing {file}")),
                quote(service),
                quote(file),
            ));
        }
        for unit in &steps.write {
            out.push_str(&here_document(
                &format!("run tee \"$dir\"/{} > /dev/null", quote(&unit.file)),
                &unit.content,
                " || exit 72",
            ));
            out.push_str(&format!("echo \"{MARK}wrote {}\"\n", unit.file));
        }
        out.push_str("ctl daemon-reload >&2 || exit 73\n");
        if !steps.restart.is_empty() {
            let services = words(steps.restart.iter().map(String::as_str));
            out.push_str(&format!(
                "echo {} >&2\n\
                 ctl restart {services} >&2 || fail 74 {services}\n",
                quote(&format!("Restarting {}", steps.restart.join(", "))),
            ));
        }
        if !steps.start.is_empty() {
            let services = words(steps.start.iter().map(String::as_str));
            out.push_str(&format!(
                "echo {} >&2\n\
                 ctl start {services} >&2 || fail 75 {services}\n",
                quote(&format!("Starting {}", steps.start.join(", "))),
            ));
        }
        out.push_str(&format!("echo \"{MARK}started\"\nexit 0\n"));
        out
    }

    /// Creates the run's secrets in Podman on the host, one `podman secret
    /// create` each, reading the value from the shell's own `printf`: a
    /// builtin, so the value is on no process's command line, and in the
    /// script alone, which the host reads on standard input. Each `value`
    /// is the text that stands for the value in the script; the runner
    /// fills it in protected memory before the script is sent. A secret
    /// that exists is removed first, so a changed value replaces it, which
    /// works on every Podman that has secrets.
    pub fn secrets(&self, names: &[(String, String)]) -> String {
        let mut out = self.preamble();
        for (name, value) in names {
            out.push_str(&format!(
                "run \"$podman\" secret rm {name} >/dev/null 2>&1 || true\n\
                 printf '%s' '{value}' | run \"$podman\" secret create {name} - >/dev/null || exit 76\n\
                 echo \"{MARK}secret {name}\"\n",
                name = quote(name),
            ));
        }
        out.push_str("exit 0\n");
        out
    }

    /// Reads values from Podman, one `podman <noun> inspect` each.
    pub fn inspect(&self, wanted: &[(String, String, String)]) -> String {
        let mut out = self.preamble();
        for (index, (noun, name, path)) in wanted.iter().enumerate() {
            out.push_str(&format!(
                "v=$(run \"$podman\" {} inspect --format {} {}) || {{ echo \"{MARK}failed {index}\"; exit 80; }}\n\
                 echo \"{MARK}value {index} $v\"\n",
                quote(noun),
                quote(&format!("{{{{.{path}}}}}")),
                quote(name),
            ));
        }
        out.push_str("exit 0\n");
        out
    }
}

/// What an apply does on the host, in the order it does it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Steps {
    /// Unit files to remove, each with the service to stop first.
    pub remove: Vec<(String, String)>,
    pub write: Vec<Desired>,
    pub restart: Vec<String>,
    pub start: Vec<String>,
}

fn words<'a>(items: impl Iterator<Item = &'a str>) -> String {
    items.map(quote).collect::<Vec<_>>().join(" ")
}

/// A command fed `content` exactly, through a here-document whose end marker
/// is no line of the content.
fn here_document(command: &str, content: &str, after: &str) -> String {
    let mut marker = "MERIDIAN_END".to_owned();
    while content.lines().any(|line| line == marker) {
        marker.push('_');
    }
    let newline = if content.ends_with('\n') { "" } else { "\n" };
    format!("{command} <<'{marker}'{after}\n{content}{newline}{marker}\n")
}

/// How a unit file on the host compares with the one the document wants.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum FileState {
    New,
    Changed,
    Same,
}

/// What Quadlet's own generator made of the wanted units.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Generator {
    /// It is not installed where the script looked.
    Missing,
    Ran {
        code: i32,
        /// The services it generated.
        generated: BTreeSet<String>,
        /// What it said, without the lines that only name what it read.
        said: Vec<String>,
    },
}

/// What a probe found.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Probe {
    pub files: BTreeMap<String, FileState>,
    /// The unit files of earlier applies that are still on the host.
    pub stale: BTreeSet<String>,
    /// Each service's state, in systemd's word for it.
    pub active: BTreeMap<String, String>,
    /// `podman --version`, or empty when there is no podman to ask.
    pub podman: String,
    pub generator: Generator,
}

/// Reads a probe's report. A script that was cut short reports no generator,
/// which is an error: nothing may be planned from half an answer.
pub(crate) fn read_probe(stdout: &str) -> Result<Probe, String> {
    let mut probe = Probe {
        files: BTreeMap::new(),
        stale: BTreeSet::new(),
        active: BTreeMap::new(),
        podman: String::new(),
        generator: Generator::Missing,
    };
    let mut code = None;
    let mut missing = false;
    let mut generated = BTreeSet::new();
    let mut said = Vec::new();

    for line in stdout.lines() {
        let Some(report) = line.strip_prefix(MARK) else {
            continue;
        };
        let (what, rest) = report.split_once(' ').unwrap_or((report, ""));
        match what {
            "file" => {
                let (state, file) = rest.split_once(' ').unwrap_or((rest, ""));
                let state = match state {
                    "new" => FileState::New,
                    "changed" => FileState::Changed,
                    "same" => FileState::Same,
                    other => return Err(format!("it reported a file as {other:?}")),
                };
                probe.files.insert(file.to_owned(), state);
            }
            "stale" => {
                probe.stale.insert(rest.to_owned());
            }
            "active" => {
                let (service, state) = rest.split_once(' ').unwrap_or((rest, "unknown"));
                probe.active.insert(service.to_owned(), state.to_owned());
            }
            "podman" => probe.podman = rest.trim().to_owned(),
            "generator" if rest == "missing" => missing = true,
            "generator" => {
                code = Some(
                    rest.parse::<i32>()
                        .map_err(|_| format!("it reported the generator's status as {rest:?}"))?,
                )
            }
            "generated" => {
                generated.insert(rest.to_owned());
            }
            "said" => said.push(rest.to_owned()),
            other => return Err(format!("it reported {other:?}, which is not understood")),
        }
    }
    probe.generator = match (code, missing) {
        (Some(code), _) => Generator::Ran {
            code,
            generated,
            said,
        },
        (None, true) => Generator::Missing,
        (None, false) => return Err("it ended before it had looked at everything".to_owned()),
    };
    Ok(probe)
}

/// What an apply script got done before it ended.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Done {
    pub wrote: BTreeSet<String>,
    pub removed: BTreeSet<String>,
    pub started: bool,
}

pub(crate) fn read_done(stdout: &str) -> Done {
    let mut done = Done::default();
    for line in stdout.lines() {
        let Some(report) = line.strip_prefix(MARK) else {
            continue;
        };
        match report.split_once(' ').unwrap_or((report, "")) {
            ("wrote", file) => {
                done.wrote.insert(file.to_owned());
            }
            ("removed", file) => {
                done.removed.insert(file.to_owned());
            }
            ("started", _) => done.started = true,
            _ => {}
        }
    }
    done
}

/// The values an inspect script read, by position.
pub(crate) fn read_values(stdout: &str) -> BTreeMap<usize, String> {
    let mut values = BTreeMap::new();
    for line in stdout.lines() {
        let Some(rest) = line
            .strip_prefix(MARK)
            .and_then(|r| r.strip_prefix("value "))
        else {
            continue;
        };
        let (index, value) = rest.split_once(' ').unwrap_or((rest, ""));
        if let Ok(index) = index.parse() {
            values.insert(index, value.to_owned());
        }
    }
    values
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_quoted_word_means_itself_to_a_shell() {
        assert_eq!(quote("web.service"), "'web.service'");
        assert_eq!(quote("it's; rm -rf /"), "'it'\\''s; rm -rf /'");
        assert_eq!(quote(""), "''");
    }

    #[test]
    fn a_here_document_never_ends_inside_its_content() {
        let text = here_document("cat", "a\nMERIDIAN_END\nb\n", "");
        assert_eq!(
            text,
            "cat <<'MERIDIAN_END_'\na\nMERIDIAN_END\nb\nMERIDIAN_END_\n"
        );
        // Content without a final line break still ends before the marker.
        assert_eq!(
            here_document("cat", "a", " || exit 1"),
            "cat <<'MERIDIAN_END' || exit 1\na\nMERIDIAN_END\n"
        );
    }

    #[test]
    fn a_probe_is_read_line_by_line() {
        let probe = read_probe(
            "noise\n\
             @meridian file new web.container\n\
             @meridian file same data.volume\n\
             @meridian file changed app.network\n\
             @meridian active web.service inactive\n\
             @meridian active data-volume.service active\n\
             @meridian stale old.container\n\
             @meridian podman podman version 5.4.0\n\
             @meridian generator 0\n\
             @meridian generated web.service\n\
             @meridian said Warning: something\n",
        )
        .unwrap();
        assert_eq!(probe.files["web.container"], FileState::New);
        assert_eq!(probe.files["data.volume"], FileState::Same);
        assert_eq!(probe.files["app.network"], FileState::Changed);
        assert_eq!(probe.active["web.service"], "inactive");
        assert!(probe.stale.contains("old.container"));
        assert_eq!(probe.podman, "podman version 5.4.0");
        assert_eq!(
            probe.generator,
            Generator::Ran {
                code: 0,
                generated: BTreeSet::from(["web.service".to_owned()]),
                said: vec!["Warning: something".to_owned()],
            }
        );
    }

    #[test]
    fn half_a_probe_is_not_an_answer() {
        let err = read_probe("@meridian file new web.container\n").unwrap_err();
        assert_eq!(err, "it ended before it had looked at everything");
        assert_eq!(
            read_probe("@meridian generator missing\n")
                .unwrap()
                .generator,
            Generator::Missing
        );
        assert!(read_probe("@meridian file odd x\n").is_err());
    }

    #[test]
    fn what_an_apply_got_done_is_read_even_when_it_was_cut_short() {
        let done = read_done("@meridian removed old.container\n@meridian wrote web.container\n");
        assert!(done.removed.contains("old.container"));
        assert!(done.wrote.contains("web.container"));
        assert!(!done.started);
        assert!(read_done("@meridian started\n").started);
    }

    #[test]
    fn inspected_values_are_read_by_position() {
        let values = read_values("@meridian value 0 10.88.0.5\n@meridian value 2 \n");
        assert_eq!(values[&0], "10.88.0.5");
        assert_eq!(values[&2], "");
        assert!(!values.contains_key(&1));
    }
}
