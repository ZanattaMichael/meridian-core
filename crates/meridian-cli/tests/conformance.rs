//! Cross-target conformance: the same document through every target of a
//! segment.
//!
//! The shared configuration management dialect promises that a resource
//! written once compiles for six tools, "within what each tool can do". This
//! test is where that promise is written down cell by cell. Each case is one
//! small document; it is compiled by every shared-dialect target, and the
//! outcome (`ok`, `ok, warns`, or the rule that refused it) is recorded in
//! `docs/reference/conformance-matrix.md`.
//!
//! Two things are asserted:
//!
//! - the page is what the plugins do today, so a plugin cannot start
//!   accepting or refusing something without the change showing in review;
//! - a case marked [`Same`] comes out the same on every target. Those are the
//!   parts of the dialect that are not allowed to differ.
//!
//! Record the page again with `UPDATE_GOLDEN=1` after a deliberate change.

#![allow(clippy::result_large_err)]

mod support;

use std::fmt::Write as _;
use std::fs;

use meridian_plugin_testkit::{graph_from_file, graph_from_text, repo_root};
use meridian_sdk::{Category, CompileError, Edge, EdgeKind, Emitted, Position, Resource};

use support::{emitter, emitters, no_data, outcome, SHARED_DIALECT};

const PATH: &str = "docs/reference/conformance-matrix.md";

/// Whether the targets may differ on a case.
#[derive(Clone, Copy, PartialEq)]
enum Agreement {
    /// Every shared-dialect target must come out the same.
    Same,
    /// The tools differ here, and the page records how.
    Varies,
}
use Agreement::{Same, Varies};

struct Case {
    group: &'static str,
    name: &'static str,
    agreement: Agreement,
    /// The document's `resources:` block, indented four spaces.
    resources: &'static str,
}

const fn case(
    group: &'static str,
    name: &'static str,
    agreement: Agreement,
    resources: &'static str,
) -> Case {
    Case {
        group,
        name,
        agreement,
        resources,
    }
}

const CASES: &[Case] = &[
    // ---- package -------------------------------------------------------
    case(
        "package",
        "no state (present)",
        Same,
        "    - id: a\n      type: package\n      params:\n        name: nginx\n",
    ),
    case(
        "package",
        "state `present`",
        Same,
        "    - id: a\n      type: package\n      state: present\n      params:\n        name: nginx\n",
    ),
    case(
        "package",
        "state `absent`",
        Varies,
        "    - id: a\n      type: package\n      state: absent\n      params:\n        name: nginx\n",
    ),
    case(
        "package",
        "state `latest`",
        Varies,
        "    - id: a\n      type: package\n      state: latest\n      params:\n        name: nginx\n",
    ),
    case(
        "package",
        "a state no tool has (`purged`)",
        Same,
        "    - id: a\n      type: package\n      state: purged\n      params:\n        name: nginx\n",
    ),
    case(
        "package",
        "param `version`",
        Varies,
        "    - id: a\n      type: package\n      params:\n        name: nginx\n        version: '1.24.0'\n",
    ),
    case(
        "package",
        "param `version` with state `latest`",
        Varies,
        "    - id: a\n      type: package\n      state: latest\n      params:\n        name: nginx\n        version: '1.24.0'\n",
    ),
    case(
        "package",
        "no `name`",
        Same,
        "    - id: a\n      type: package\n      params: {}\n",
    ),
    case(
        "package",
        "no `params` at all",
        Same,
        "    - id: a\n      type: package\n",
    ),
    case(
        "package",
        "a param outside the dialect (`provider`)",
        Same,
        "    - id: a\n      type: package\n      params:\n        name: nginx\n        provider: apt\n",
    ),
    case(
        "package",
        "`name` is a number",
        Varies,
        "    - id: a\n      type: package\n      params:\n        name: 42\n",
    ),
    case(
        "package",
        "`name` is a list",
        Varies,
        "    - id: a\n      type: package\n      params:\n        name: [nginx, curl]\n",
    ),
    // ---- service -------------------------------------------------------
    case(
        "service",
        "no state (running)",
        Same,
        "    - id: a\n      type: service\n      params:\n        name: nginx\n",
    ),
    case(
        "service",
        "state `running`, `enabled: true`",
        Same,
        "    - id: a\n      type: service\n      state: running\n      params:\n        name: nginx\n        enabled: true\n",
    ),
    case(
        "service",
        "state `started`",
        Same,
        "    - id: a\n      type: service\n      state: started\n      params:\n        name: nginx\n",
    ),
    case(
        "service",
        "state `stopped`, `enabled: false`",
        Same,
        "    - id: a\n      type: service\n      state: stopped\n      params:\n        name: nginx\n        enabled: false\n",
    ),
    case(
        "service",
        "state `restarted`",
        Varies,
        "    - id: a\n      type: service\n      state: restarted\n      params:\n        name: nginx\n",
    ),
    case(
        "service",
        "state `reloaded`",
        Varies,
        "    - id: a\n      type: service\n      state: reloaded\n      params:\n        name: nginx\n",
    ),
    case(
        "service",
        "a state no tool has (`paused`)",
        Same,
        "    - id: a\n      type: service\n      state: paused\n      params:\n        name: nginx\n",
    ),
    case(
        "service",
        "no `name`",
        Same,
        "    - id: a\n      type: service\n      params:\n        enabled: true\n",
    ),
    case(
        "service",
        "`enabled` is text, not true or false",
        Varies,
        "    - id: a\n      type: service\n      params:\n        name: nginx\n        enabled: 'yes'\n",
    ),
    case(
        "service",
        "a param outside the dialect (`pattern`)",
        Same,
        "    - id: a\n      type: service\n      params:\n        name: nginx\n        pattern: nginx\n",
    ),
    // ---- file ----------------------------------------------------------
    case(
        "file",
        "content, owner, group and mode",
        Same,
        "    - id: a\n      type: file\n      params:\n        path: /etc/app/app.conf\n        content: \"workers = 8\\n\"\n        owner: root\n        group: root\n        mode: \"0640\"\n",
    ),
    case(
        "file",
        "state `file`",
        Same,
        "    - id: a\n      type: file\n      state: file\n      params:\n        path: /etc/app/app.conf\n        content: \"x\\n\"\n",
    ),
    case(
        "file",
        "state `directory`",
        Same,
        "    - id: a\n      type: file\n      state: directory\n      params:\n        path: /srv/app\n        owner: root\n        mode: \"0755\"\n",
    ),
    case(
        "file",
        "state `directory` with `content`",
        Varies,
        "    - id: a\n      type: file\n      state: directory\n      params:\n        path: /srv/app\n        content: \"x\\n\"\n",
    ),
    case(
        "file",
        "state `absent`",
        Same,
        "    - id: a\n      type: file\n      state: absent\n      params:\n        path: /etc/app/old.conf\n",
    ),
    case(
        "file",
        "state `absent` with owner and mode",
        Varies,
        "    - id: a\n      type: file\n      state: absent\n      params:\n        path: /etc/app/old.conf\n        owner: root\n        mode: \"0644\"\n",
    ),
    case(
        "file",
        "no `content`",
        Varies,
        "    - id: a\n      type: file\n      params:\n        path: /etc/app/app.conf\n        owner: root\n",
    ),
    case(
        "file",
        "empty `content`",
        Varies,
        "    - id: a\n      type: file\n      params:\n        path: /etc/app/app.conf\n        content: \"\"\n",
    ),
    case(
        "file",
        "content with quotes, a backslash and every tool's own interpolation",
        Same,
        "    - id: a\n      type: file\n      params:\n        path: /etc/app/app.conf\n        content: \"a 'b' \\\"c\\\" \\\\ #{ruby} {{ jinja }} {% tag %} %{hiera} $shell `tick`\\n\"\n",
    ),
    case(
        "file",
        "content that is not ASCII",
        Same,
        "    - id: a\n      type: file\n      params:\n        path: /etc/motd\n        content: \"Grüße, 世界 ✓\\n\"\n",
    ),
    case(
        "file",
        "content of several lines",
        Same,
        "    - id: a\n      type: file\n      params:\n        path: /etc/app/app.conf\n        content: |\n          [server]\n          workers = 8\n\n          [log]\n          level = info\n",
    ),
    case(
        "file",
        "`mode` unquoted, so read as a number",
        Varies,
        "    - id: a\n      type: file\n      params:\n        path: /etc/app/app.conf\n        content: \"x\\n\"\n        mode: 0644\n",
    ),
    case(
        "file",
        "no `path`",
        Same,
        "    - id: a\n      type: file\n      params:\n        content: \"x\\n\"\n",
    ),
    case(
        "file",
        "a state no tool has (`touched`)",
        Same,
        "    - id: a\n      type: file\n      state: touched\n      params:\n        path: /etc/app/app.conf\n",
    ),
    case(
        "file",
        "a param outside the dialect (`source`)",
        Same,
        "    - id: a\n      type: file\n      params:\n        path: /etc/app/app.conf\n        source: https://example.com/app.conf\n",
    ),
    // ---- user ----------------------------------------------------------
    case(
        "user",
        "only `name`",
        Same,
        "    - id: a\n      type: user\n      params:\n        name: deploy\n",
    ),
    case(
        "user",
        "uid, home, shell, system and groups",
        Same,
        "    - id: a\n      type: user\n      params:\n        name: deploy\n        uid: 1500\n        home: /home/deploy\n        shell: /bin/bash\n        system: false\n        groups: [www-data, adm]\n",
    ),
    case(
        "user",
        "state `absent`",
        Varies,
        "    - id: a\n      type: user\n      state: absent\n      params:\n        name: deploy\n",
    ),
    case(
        "user",
        "no `name`",
        Same,
        "    - id: a\n      type: user\n      params:\n        uid: 1500\n",
    ),
    case(
        "user",
        "`groups` is text, not a list",
        Varies,
        "    - id: a\n      type: user\n      params:\n        name: deploy\n        groups: adm\n",
    ),
    case(
        "user",
        "a param outside the dialect (`password`)",
        Same,
        "    - id: a\n      type: user\n      params:\n        name: deploy\n        password: hunter2\n",
    ),
    // ---- group ---------------------------------------------------------
    case(
        "group",
        "only `name`",
        Same,
        "    - id: a\n      type: group\n      params:\n        name: deploy\n",
    ),
    case(
        "group",
        "`gid` and `system`",
        Varies,
        "    - id: a\n      type: group\n      params:\n        name: deploy\n        gid: 1500\n        system: true\n",
    ),
    case(
        "group",
        "state `absent`",
        Varies,
        "    - id: a\n      type: group\n      state: absent\n      params:\n        name: deploy\n",
    ),
    case(
        "group",
        "no `name`",
        Same,
        "    - id: a\n      type: group\n      params: {}\n",
    ),
    // ---- exec ----------------------------------------------------------
    case(
        "exec",
        "only `command`",
        Same,
        "    - id: a\n      type: exec\n      params:\n        command: /usr/local/bin/warm-cache --all\n",
    ),
    case(
        "exec",
        "`chdir` and `creates`",
        Same,
        "    - id: a\n      type: exec\n      params:\n        command: /usr/local/bin/warm-cache --all\n        chdir: /srv/app\n        creates: /var/cache/app/warm\n",
    ),
    case(
        "exec",
        "`shell: true` and a pipe",
        Same,
        "    - id: a\n      type: exec\n      params:\n        command: \"ps aux | grep -c nginx > /tmp/count\"\n        shell: true\n",
    ),
    case(
        "exec",
        "a pipe without `shell: true`",
        Varies,
        "    - id: a\n      type: exec\n      params:\n        command: \"ps aux | grep -c nginx\"\n",
    ),
    case(
        "exec",
        "`removes`",
        Varies,
        "    - id: a\n      type: exec\n      params:\n        command: /usr/local/bin/cleanup\n        removes: /var/run/app.lock\n",
    ),
    case(
        "exec",
        "a state (an exec has none)",
        Same,
        "    - id: a\n      type: exec\n      state: present\n      params:\n        command: /bin/true\n",
    ),
    case(
        "exec",
        "no `command`",
        Same,
        "    - id: a\n      type: exec\n      params:\n        chdir: /srv/app\n",
    ),
    case(
        "exec",
        "a param outside the dialect (`timeout`)",
        Same,
        "    - id: a\n      type: exec\n      params:\n        command: /bin/true\n        timeout: 30\n",
    ),
    // ---- types ---------------------------------------------------------
    case(
        "types",
        "a type outside the dialect (`firewall_rule`)",
        Same,
        "    - id: a\n      type: firewall_rule\n      params:\n        port: 80\n",
    ),
    case(
        "types",
        "a type in the wrong case (`Package`)",
        Same,
        "    - id: a\n      type: Package\n      params:\n        name: nginx\n",
    ),
    case(
        "types",
        "all six types in one document",
        Same,
        "    - id: grp\n      type: group\n      params:\n        name: app\n    - id: usr\n      type: user\n      params:\n        name: app\n        groups: [app]\n      dependsOn:\n        - resource: grp\n    - id: pkg\n      type: package\n      params:\n        name: nginx\n    - id: conf\n      type: file\n      params:\n        path: /etc/nginx/nginx.conf\n        content: \"worker_processes 4;\\n\"\n      dependsOn:\n        - resource: pkg\n        - resource: usr\n    - id: svc\n      type: service\n      params:\n        name: nginx\n        enabled: true\n      dependsOn:\n        - resource: conf\n    - id: warm\n      type: exec\n      params:\n        command: /usr/local/bin/warm-cache\n        creates: /var/cache/warm\n      dependsOn:\n        - resource: svc\n",
    ),
    // ---- dependsOn -----------------------------------------------------
    case(
        "dependsOn",
        "a chain written in reverse order",
        Same,
        "    - id: c\n      type: service\n      params:\n        name: zeta\n      dependsOn:\n        - resource: b\n    - id: b\n      type: file\n      params:\n        path: /etc/zeta.conf\n        content: \"x\\n\"\n      dependsOn:\n        - resource: a\n    - id: a\n      type: package\n      params:\n        name: zeta\n",
    ),
    case(
        "dependsOn",
        "a diamond",
        Same,
        "    - id: top\n      type: package\n      params:\n        name: app\n    - id: left\n      type: file\n      params:\n        path: /etc/app/left\n        content: \"l\\n\"\n      dependsOn:\n        - resource: top\n    - id: right\n      type: file\n      params:\n        path: /etc/app/right\n        content: \"r\\n\"\n      dependsOn:\n        - resource: top\n    - id: bottom\n      type: service\n      params:\n        name: app\n      dependsOn:\n        - resource: left\n        - resource: right\n",
    ),
    case(
        "dependsOn",
        "the same dependency named twice",
        Same,
        "    - id: a\n      type: package\n      params:\n        name: app\n    - id: b\n      type: service\n      params:\n        name: app\n      dependsOn:\n        - resource: a\n        - resource: a\n",
    ),
    // ---- notifies ------------------------------------------------------
    case(
        "notifies",
        "a file restarts a service",
        Same,
        "    - id: conf\n      type: file\n      params:\n        path: /etc/app.conf\n        content: \"x\\n\"\n      notifies:\n        - resource: svc\n          action: restart\n    - id: svc\n      type: service\n      params:\n        name: app\n",
    ),
    case(
        "notifies",
        "a file reloads a service",
        Varies,
        "    - id: conf\n      type: file\n      params:\n        path: /etc/app.conf\n        content: \"x\\n\"\n      notifies:\n        - resource: svc\n          action: reload\n    - id: svc\n      type: service\n      params:\n        name: app\n",
    ),
    case(
        "notifies",
        "a file starts a service",
        Varies,
        "    - id: conf\n      type: file\n      params:\n        path: /etc/app.conf\n        content: \"x\\n\"\n      notifies:\n        - resource: svc\n          action: start\n    - id: svc\n      type: service\n      params:\n        name: app\n",
    ),
    case(
        "notifies",
        "a file stops a service",
        Varies,
        "    - id: conf\n      type: file\n      params:\n        path: /etc/app.conf\n        content: \"x\\n\"\n      notifies:\n        - resource: svc\n          action: stop\n    - id: svc\n      type: service\n      params:\n        name: app\n",
    ),
    case(
        "notifies",
        "a file restarts a service whose state is `stopped`",
        Varies,
        "    - id: conf\n      type: file\n      params:\n        path: /etc/app.conf\n        content: \"x\\n\"\n      notifies:\n        - resource: svc\n          action: restart\n    - id: svc\n      type: service\n      state: stopped\n      params:\n        name: app\n",
    ),
    case(
        "notifies",
        "a file runs an exec",
        Same,
        "    - id: conf\n      type: file\n      params:\n        path: /etc/app.conf\n        content: \"x\\n\"\n      notifies:\n        - resource: check\n          action: run\n    - id: check\n      type: exec\n      params:\n        command: /usr/sbin/app --check\n",
    ),
    case(
        "notifies",
        "an action no tool has (`bounce`)",
        Same,
        "    - id: conf\n      type: file\n      params:\n        path: /etc/app.conf\n        content: \"x\\n\"\n      notifies:\n        - resource: svc\n          action: bounce\n    - id: svc\n      type: service\n      params:\n        name: app\n",
    ),
    case(
        "notifies",
        "`restart` aimed at an exec",
        Same,
        "    - id: conf\n      type: file\n      params:\n        path: /etc/app.conf\n        content: \"x\\n\"\n      notifies:\n        - resource: check\n          action: restart\n    - id: check\n      type: exec\n      params:\n        command: /usr/sbin/app --check\n",
    ),
    case(
        "notifies",
        "a package is notified (it cannot be)",
        Same,
        "    - id: conf\n      type: file\n      params:\n        path: /etc/app.conf\n        content: \"x\\n\"\n      notifies:\n        - resource: pkg\n          action: restart\n    - id: pkg\n      type: package\n      params:\n        name: app\n",
    ),
    case(
        "notifies",
        "two files restart the same service",
        Same,
        "    - id: one\n      type: file\n      params:\n        path: /etc/app/one.conf\n        content: \"1\\n\"\n      notifies:\n        - resource: svc\n          action: restart\n    - id: two\n      type: file\n      params:\n        path: /etc/app/two.conf\n        content: \"2\\n\"\n      notifies:\n        - resource: svc\n          action: restart\n    - id: svc\n      type: service\n      params:\n        name: app\n",
    ),
    case(
        "notifies",
        "one file notifies a service and an exec",
        Same,
        "    - id: conf\n      type: file\n      params:\n        path: /etc/app.conf\n        content: \"x\\n\"\n      notifies:\n        - resource: svc\n          action: restart\n        - resource: check\n          action: run\n    - id: svc\n      type: service\n      params:\n        name: app\n    - id: check\n      type: exec\n      params:\n        command: /usr/sbin/app --check\n",
    ),
    case(
        "notifies",
        "a chain: a package runs an exec, which restarts a service",
        Same,
        "    - id: pkg\n      type: package\n      params:\n        name: app\n      notifies:\n        - resource: migrate\n          action: run\n    - id: migrate\n      type: exec\n      params:\n        command: /usr/sbin/app --migrate\n      notifies:\n        - resource: svc\n          action: restart\n    - id: svc\n      type: service\n      params:\n        name: app\n",
    ),
    case(
        "notifies",
        "a resource notifies one it also depends on",
        Varies,
        "    - id: svc\n      type: service\n      params:\n        name: app\n    - id: conf\n      type: file\n      params:\n        path: /etc/app.conf\n        content: \"x\\n\"\n      dependsOn:\n        - resource: svc\n      notifies:\n        - resource: svc\n          action: restart\n",
    ),
    // ---- references ----------------------------------------------------
    case(
        "references",
        "a whole value: `${dir.path}`",
        Same,
        "    - id: dir\n      type: file\n      state: directory\n      params:\n        path: /srv/app\n    - id: run\n      type: exec\n      params:\n        command: /usr/local/bin/build\n        chdir: ${dir.path}\n",
    ),
    case(
        "references",
        "inside text: `${dir.path}/index.html`",
        Same,
        "    - id: dir\n      type: file\n      state: directory\n      params:\n        path: /srv/app\n    - id: index\n      type: file\n      params:\n        path: ${dir.path}/index.html\n        content: \"served from ${dir.path}\\n\"\n",
    ),
    case(
        "references",
        "two references in one value",
        Same,
        "    - id: usr\n      type: user\n      params:\n        name: deploy\n        home: /home/deploy\n    - id: note\n      type: file\n      params:\n        path: ${usr.home}/README\n        content: \"${usr.name} lives in ${usr.home}\\n\"\n",
    ),
    case(
        "references",
        "a reference to a reference",
        Same,
        "    - id: dir\n      type: file\n      state: directory\n      params:\n        path: /srv/app\n    - id: sub\n      type: file\n      state: directory\n      params:\n        path: ${dir.path}/releases\n    - id: keep\n      type: file\n      params:\n        path: ${sub.path}/.keep\n        content: \"kept\\n\"\n",
    ),
    case(
        "references",
        "a whole value that is a number: `${usr.uid}`",
        Same,
        "    - id: usr\n      type: user\n      params:\n        name: deploy\n        uid: 1500\n    - id: grp\n      type: group\n      params:\n        name: deploy\n    - id: note\n      type: file\n      params:\n        path: /etc/deploy.uid\n        content: \"uid=${usr.uid}\\n\"\n",
    ),
    case(
        "references",
        "a resource that is not in the document",
        Same,
        "    - id: a\n      type: file\n      params:\n        path: ${nowhere.path}/x\n        content: \"x\\n\"\n",
    ),
    case(
        "references",
        "a param the resource does not set",
        Same,
        "    - id: dir\n      type: file\n      state: directory\n      params:\n        path: /srv/app\n    - id: a\n      type: file\n      params:\n        path: /srv/app/x\n        content: \"${dir.owner}\\n\"\n",
    ),
    case(
        "references",
        "a list read into the middle of text",
        Same,
        "    - id: usr\n      type: user\n      params:\n        name: deploy\n        groups: [adm, www-data]\n    - id: a\n      type: file\n      params:\n        path: /etc/groups.txt\n        content: \"groups: ${usr.groups}\\n\"\n",
    ),
    case(
        "references",
        "a resource that reads itself",
        Same,
        "    - id: a\n      type: file\n      params:\n        path: /srv/app/x\n        content: \"${a.content}\\n\"\n",
    ),
    case(
        "references",
        "two resources that read each other",
        Same,
        "    - id: a\n      type: file\n      params:\n        path: /srv/a\n        content: \"${b.content}\"\n    - id: b\n      type: file\n      params:\n        path: /srv/b\n        content: \"${a.content}\"\n",
    ),
    case(
        "references",
        "something that is not `id.param`: `${upper(dir.path)}`",
        Same,
        "    - id: dir\n      type: file\n      state: directory\n      params:\n        path: /srv/app\n    - id: a\n      type: file\n      params:\n        path: /srv/app/x\n        content: \"${upper(dir.path)}\\n\"\n",
    ),
    case(
        "references",
        "never closed: `${dir.path`",
        Same,
        "    - id: dir\n      type: file\n      state: directory\n      params:\n        path: /srv/app\n    - id: a\n      type: file\n      params:\n        path: /srv/app/x\n        content: \"${dir.path\\n\"\n",
    ),
    case(
        "references",
        "a literal: `$${HOME}`",
        Same,
        "    - id: a\n      type: exec\n      params:\n        command: \"echo $${HOME} > /tmp/home\"\n        shell: true\n",
    ),
    case(
        "references",
        "a dollar that starts nothing: `$HOME` and `$(id -u)`",
        Same,
        "    - id: a\n      type: exec\n      params:\n        command: \"echo $HOME $(id -u) > /tmp/home\"\n        shell: true\n",
    ),
    // ---- runtimeWhen ---------------------------------------------------
    case(
        "runtimeWhen",
        "on an exec",
        Varies,
        "    - id: a\n      type: exec\n      params:\n        command: /bin/true\n      runtimeWhen: \"1 == 1\"\n",
    ),
    case(
        "runtimeWhen",
        "on a notified service",
        Varies,
        "    - id: conf\n      type: file\n      params:\n        path: /etc/app.conf\n        content: \"x\\n\"\n      notifies:\n        - resource: svc\n          action: restart\n    - id: svc\n      type: service\n      params:\n        name: app\n      runtimeWhen: \"1 == 1\"\n",
    ),
];

fn document(name: &str, resources: &str) -> String {
    format!(
        "apiVersion: meridian/v1\nkind: ResourceSet\nmetadata:\n  name: {name}\nspec:\n  target: ansible\n  targetHost:\n    from: inline\n    host: web01.example.com\n  resources:\n{resources}"
    )
}

fn compile(target: &str, name: &str, resources: &str) -> Result<Emitted, CompileError> {
    let graph = graph_from_text("case.yaml", &document(name, resources), target);
    emitter(target).emit(&graph, &no_data())
}

/// The id a table links to and the heading it sits under.
fn headings() -> Vec<&'static str> {
    let mut groups: Vec<&'static str> = Vec::new();
    for case in CASES {
        if !groups.contains(&case.group) {
            groups.push(case.group);
        }
    }
    groups
}

const INTRODUCTION: &str = "\
# Conformance matrix

What happens to the same document on each target that shares the
[configuration management resource types](resource-types.md). This page is
generated: `crates/meridian-cli/tests/conformance.rs` compiles one small
document per row with every target and writes down how each compile came out,
and it fails when this page is out of date. So the page cannot drift from the
code, and a plugin cannot start accepting or refusing something without the
change showing here.

How to read a cell:

- **ok**: the document compiled.
- **ok, warns**: it compiled, with a warning the plan shows. Nothing is dropped
  silently; the warning says what the tool will do differently.
- a name such as `unsupported-state`: the target refused the document, and that
  is the rule the refusal carries. The message that goes with it names the
  resource, the line, and what the target does accept.

The **same** column says whether the targets are allowed to differ. A row
marked *yes* is a part of the dialect that must come out the same everywhere,
and the test fails if one target breaks ranks. A row marked *no* is a place
where the tools themselves differ; the cells say how, and the
[resource types](resource-types.md) page and each target's own page say why.

";

fn matrix() -> String {
    let mut out = INTRODUCTION.to_owned();
    for group in headings() {
        let _ = writeln!(out, "## `{group}`\n");
        let _ = write!(out, "| Document | same |");
        for target in SHARED_DIALECT {
            let _ = write!(out, " {target} |");
        }
        let _ = write!(out, "\n|---|---|");
        for _ in SHARED_DIALECT {
            out.push_str("---|");
        }
        out.push('\n');
        for case in CASES.iter().filter(|c| c.group == group) {
            let same = if case.agreement == Same { "yes" } else { "no" };
            let _ = write!(out, "| {} | {same} |", case.name.replace('|', "\\|"));
            for target in SHARED_DIALECT {
                let _ = write!(
                    out,
                    " {} |",
                    outcome(&compile(target, "web", case.resources))
                );
            }
            out.push('\n');
        }
        out.push('\n');
    }
    out
}

#[test]
fn the_documented_matrix_is_what_the_targets_do() {
    let path = repo_root().join(PATH);
    let generated = matrix();
    if std::env::var_os("UPDATE_GOLDEN").is_some() {
        fs::write(&path, &generated).unwrap();
    }
    let documented = fs::read_to_string(&path).unwrap_or_default();
    assert!(
        documented == generated,
        "{PATH} is out of date with what the targets do; run the tests with UPDATE_GOLDEN=1 to record it again, and read the difference"
    );
}

#[test]
fn a_case_marked_same_comes_out_the_same_on_every_target() {
    let mut failures = Vec::new();
    for case in CASES.iter().filter(|c| c.agreement == Same) {
        let outcomes: Vec<(&str, String)> = SHARED_DIALECT
            .iter()
            .map(|target| (*target, outcome(&compile(target, "web", case.resources))))
            .collect();
        // A warning is a target's own remark, so `ok` and `ok, warns` agree.
        let verdict = |o: &str| {
            if o.starts_with("ok") {
                "ok".to_owned()
            } else {
                o.to_owned()
            }
        };
        let first = verdict(&outcomes[0].1);
        if outcomes.iter().any(|(_, o)| verdict(o) != first) {
            failures.push(format!("{} / {}: {outcomes:?}", case.group, case.name));
        }
    }
    assert!(
        failures.is_empty(),
        "these cases must come out the same on every shared-dialect target:\n{}",
        failures.join("\n")
    );
}

#[test]
fn case_names_are_unique_within_a_group() {
    let mut seen = std::collections::BTreeSet::new();
    for case in CASES {
        assert!(
            seen.insert((case.group, case.name)),
            "{} / {} appears twice",
            case.group,
            case.name
        );
    }
}

/// OpenVox reads the language Puppet reads and shares its compiler, so the two
/// may never part ways: not in what they accept, and not in the manifest.
#[test]
fn puppet_and_openvox_compile_every_case_to_the_same_manifest() {
    for case in CASES {
        let puppet = compile("puppet", "web", case.resources);
        let openvox = compile("openvox", "web", case.resources);
        match (puppet, openvox) {
            (Ok(p), Ok(o)) => {
                let manifests = |e: &Emitted| -> Vec<String> {
                    e.artifact
                        .files
                        .iter()
                        .filter(|(path, _)| path.ends_with(".pp"))
                        .map(|(_, content)| content.clone())
                        .collect()
                };
                assert!(!manifests(&p).is_empty(), "{}: no manifest", case.name);
                assert_eq!(
                    manifests(&p),
                    manifests(&o),
                    "{} / {}",
                    case.group,
                    case.name
                );
            }
            (Err(p), Err(o)) => {
                assert_eq!(p.rule, o.rule, "{} / {}", case.group, case.name);
                assert_eq!(p.resource, o.resource, "{} / {}", case.group, case.name);
            }
            (p, o) => panic!(
                "{} / {}: puppet {} and openvox {}",
                case.group,
                case.name,
                outcome(&p),
                outcome(&o)
            ),
        }
    }
}

/// A compile is a pure function of the tree: the same tree twice gives the
/// same bytes and the same warnings, on every target and every case.
#[test]
fn every_case_compiles_to_the_same_bytes_twice() {
    for case in CASES {
        for target in SHARED_DIALECT {
            let first = compile(target, "web", case.resources);
            let second = compile(target, "web", case.resources);
            assert_eq!(first, second, "{target}: {} / {}", case.group, case.name);
        }
    }
}

/// A refusal says which target spoke, which resource it is about and where
/// that resource is written, whichever target raised it.
#[test]
fn every_refusal_names_its_target_its_resource_and_a_line() {
    for case in CASES {
        for target in SHARED_DIALECT {
            let Err(refusal) = compile(target, "web", case.resources) else {
                continue;
            };
            let at = format!("{target}: {} / {}", case.group, case.name);
            assert_eq!(refusal.target, *target, "{at}");
            assert!(!refusal.rule.is_empty(), "{at}: no rule");
            assert!(!refusal.msg.is_empty(), "{at}: no message");
            assert!(
                refusal
                    .rule
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c == '-'),
                "{at}: rule {:?} is not lower-case words joined by hyphens",
                refusal.rule
            );
            if !refusal.resource.is_empty() {
                assert_eq!(refusal.pos.file, "case.yaml", "{at}");
                assert!(refusal.pos.line > 0, "{at}: no line");
            }
        }
    }
}

/// No target leaves a `${...}` it was meant to settle in what it writes: by
/// the time a document compiles, a reference is the value it named.
#[test]
fn a_settled_reference_leaves_no_marker_in_any_output() {
    for case in CASES.iter().filter(|c| c.group == "references") {
        for target in SHARED_DIALECT {
            let Ok(emitted) = compile(target, "web", case.resources) else {
                continue;
            };
            for (path, content) in &emitted.artifact.files {
                for marker in ["${dir.", "${usr.", "${sub."] {
                    assert!(
                        !content.contains(marker),
                        "{target}: {} left {marker} in {path}:\n{content}",
                        case.name
                    );
                }
            }
        }
    }
}

/// The value a reference names reaches every tool's files. `/srv/app` is
/// written once, on `dir`; a target that compiled the reading resource must
/// have written the joined path somewhere.
#[test]
fn the_value_a_reference_names_is_in_every_output() {
    let resources = "    - id: dir\n      type: file\n      state: directory\n      params:\n        path: /srv/app-7f3a\n    - id: index\n      type: file\n      params:\n        path: ${dir.path}/index.html\n        content: \"hello\\n\"\n";
    for target in SHARED_DIALECT {
        let emitted = compile(target, "web", resources).unwrap_or_else(|e| panic!("{target}: {e}"));
        let all: String = emitted.artifact.files.values().cloned().collect();
        assert!(
            all.contains("/srv/app-7f3a/index.html"),
            "{target} did not write the joined path:\n{all}"
        );
    }
}

/// What `dependsOn` promises on every target: a resource is applied after what
/// it depends on. Each resource here carries a name found nowhere else, so its
/// place in a target's main file can be read back.
///
/// cloud-init is left out: a boot runs its modules in an order of its own
/// (packages, then files, then commands) whatever order the keys of the
/// user-data are written in, so the text says nothing about the order.
#[test]
fn a_dependency_is_written_before_what_depends_on_it() {
    // Declared in reverse, so declaration order alone would get it wrong.
    let resources = "    - id: third\n      type: service\n      params:\n        name: marker-cccc\n      dependsOn:\n        - resource: second\n    - id: second\n      type: file\n      params:\n        path: /etc/marker-bbbb.conf\n        content: \"x\\n\"\n      dependsOn:\n        - resource: first\n    - id: first\n      type: package\n      params:\n        name: marker-aaaa\n";
    for target in SHARED_DIALECT.iter().filter(|t| **t != "cloud-init") {
        let emitted = compile(target, "web", resources).unwrap_or_else(|e| panic!("{target}: {e}"));
        // The file that holds all three, whatever the target calls it.
        let (path, content) = emitted
            .artifact
            .files
            .iter()
            .find(|(_, c)| {
                ["marker-aaaa", "marker-bbbb", "marker-cccc"]
                    .iter()
                    .all(|m| c.contains(m))
            })
            .unwrap_or_else(|| panic!("{target}: no file holds all three resources"));
        let at = |marker: &str| content.find(marker).unwrap();
        assert!(
            at("marker-aaaa") < at("marker-bbbb") && at("marker-bbbb") < at("marker-cccc"),
            "{target}: {path} does not hold the resources in dependency order:\n{content}"
        );
    }
}

// ---------------------------------------------------------------------------
// Every target, not only the shared dialect
// ---------------------------------------------------------------------------

/// Every example that holds one document, with the target it names: documents
/// each target is known to compile.
/// Whether a document holds an interpolation outside its comments. An
/// escaped opening, written with two dollars, is not one.
fn interpolates(text: &str) -> bool {
    let (open, escaped) = (["$", "{{"].concat(), ["$$", "{{"].concat());
    text.lines().any(|line| {
        !line.trim_start().starts_with('#') && line.replace(&escaped, "").contains(&open)
    })
}

fn examples() -> Vec<(String, String)> {
    let mut found = Vec::new();
    let root = repo_root();
    for directory in fs::read_dir(root.join("examples")).unwrap() {
        let directory = directory.unwrap().path();
        if !directory.is_dir() {
            continue;
        }
        for file in fs::read_dir(&directory).unwrap() {
            let file = file.unwrap().path();
            if file.extension().is_none_or(|e| e != "yaml") {
                continue;
            }
            let text = fs::read_to_string(&file).unwrap();
            let targets: Vec<&str> = text
                .lines()
                .filter_map(|line| line.strip_prefix("  target: "))
                .collect();
            // A document that decides by `when` needs its data and the
            // condition stage before a tree can be built; `examples.rs`
            // compiles those through the command, with their data.
            if text
                .lines()
                .any(|line| line.trim_start().starts_with("when:"))
            {
                continue;
            }
            // So does one that interpolates: its values are worked out from
            // data, variables or secrets before any target sees it.
            if interpolates(&text) {
                continue;
            }
            // A workflow of several documents is the orchestrator's to test.
            if let [target] = targets[..] {
                let path = file.strip_prefix(&root).unwrap().to_str().unwrap();
                found.push((path.to_owned(), target.trim().to_owned()));
            }
        }
    }
    found.sort();
    found
}

#[test]
fn every_target_has_an_example_of_its_own() {
    let examples = examples();
    for target in emitters() {
        assert!(
            examples.iter().any(|(_, t)| t == target.name()),
            "no example under examples/ compiles for {}",
            target.name()
        );
    }
}

#[test]
fn every_example_compiles_to_the_same_files_twice() {
    for (path, name) in examples() {
        let target = emitter(&name);
        let graph = graph_from_file(&path, &name);
        let first = target
            .emit(&graph, &no_data())
            .unwrap_or_else(|e| panic!("{path}: {e}"));
        assert!(
            !first.artifact.files.is_empty(),
            "{path} compiled to no files"
        );
        for (file, content) in &first.artifact.files {
            assert!(!file.starts_with('/'), "{path}: {file} is absolute");
            assert!(
                !file.split('/').any(|part| part == ".."),
                "{path}: {file} leaves the output directory"
            );
            assert!(!content.is_empty(), "{path}: {file} is empty");
        }
        assert_eq!(Ok(first), target.emit(&graph, &no_data()), "{path}");
    }
}

/// A target's `category` is how the core pairs it with a kind of document, so
/// every shipped target must declare one, and the shared dialect is exactly
/// the six types on six configuration management targets.
#[test]
fn every_target_declares_its_segment_and_its_types() {
    for target in emitters() {
        assert_ne!(
            target.category(),
            Category::Unspecified,
            "{} declares no category",
            target.name()
        );
        let types = target.supported_resource_types();
        assert!(!types.is_empty(), "{} declares no types", target.name());
        let mut sorted = types.clone();
        sorted.sort();
        sorted.dedup();
        assert_eq!(
            types,
            sorted,
            "{}: types are sorted and unique",
            target.name()
        );
    }
    for name in SHARED_DIALECT {
        let target = emitter(name);
        assert_eq!(
            target.category(),
            Category::ConfigurationManagement,
            "{name}"
        );
        assert_eq!(
            target.supported_resource_types(),
            ["exec", "file", "group", "package", "service", "user"],
            "{name}"
        );
    }
}

/// The capability a target declares is a contract in both directions. Where a
/// target says it can express `runtimeWhen`, a resource that carries one is
/// compiled with it, or refused if that one resource cannot take it. Where a
/// target says it cannot, the resource is refused under one rule. No target
/// compiles the document and leaves the condition out.
///
/// Every resource of every example is tried, so a target's settings
/// resources are held to the same rule as its real ones.
#[test]
fn runtime_when_is_never_dropped() {
    const PROBE: &str = "meridian_conformance_probe";
    for (path, name) in examples() {
        let target = emitter(&name);
        let graph = graph_from_file(&path, &name);
        for index in 0..graph.resources.len() {
            let mut graph = graph.clone();
            graph.resources[index].runtime_when = PROBE.to_owned();
            let id = graph.resources[index].id.clone();
            let at = format!("{path}: resource {id:?}");
            match target.emit(&graph, &no_data()) {
                Ok(emitted) => {
                    assert!(
                        target.capabilities().runtime_condition,
                        "{at}: {name} declares no runtimeWhen and compiled one"
                    );
                    assert!(
                        emitted.artifact.files.values().any(|c| c.contains(PROBE)),
                        "{at}: {name} compiled the document and dropped the condition"
                    );
                }
                Err(refusal) => {
                    assert_eq!(
                        refusal.rule, "unsupported-runtime-condition",
                        "{at}: {refusal}"
                    );
                    assert_eq!(refusal.resource, id, "{at}");
                    assert_eq!(refusal.target, name, "{at}");
                }
            }
        }
    }
}

/// A target with no way to react to a change refuses a `notifies`, between
/// any two of a document's resources, and never compiles the document
/// without it.
#[test]
fn notifies_is_refused_by_every_target_that_declares_no_notify() {
    let mut without_notify = 0;
    for (path, name) in examples() {
        let target = emitter(&name);
        let capabilities = target.capabilities();
        if capabilities.native_notify || capabilities.synthetic_notify {
            continue;
        }
        without_notify += 1;
        let graph = graph_from_file(&path, &name);
        let mut said_why = false;
        for from in &graph.resources {
            for to in &graph.resources {
                let mut graph = graph.clone();
                graph.edges.push(Edge {
                    from: from.id.clone(),
                    to: to.id.clone(),
                    kind: EdgeKind::Notify,
                    action: "restart".to_owned(),
                    pos: Position::default(),
                });
                let refusal = target.emit(&graph, &no_data()).expect_err(&format!(
                    "{path}: {name} has no notify and compiled {} notifying {}",
                    from.id, to.id
                ));
                assert_eq!(refusal.target, name, "{path}");
                said_why |= refusal.rule == "unsupported-action";
            }
        }
        assert!(
            said_why,
            "{path}: {name} never refused a notify as unsupported-action"
        );
    }
    assert!(
        without_notify > 0,
        "no example belongs to a target without notify"
    );
}

/// A type the target does not map is refused under one rule on every target.
#[test]
fn an_unknown_type_is_refused_by_every_target_under_one_rule() {
    for (path, name) in examples() {
        let target = emitter(&name);
        let mut graph = graph_from_file(&path, &name);
        graph.resources.push(Resource {
            id: "conformance_probe".to_owned(),
            r#type: "Meridian Conformance Probe!".to_owned(),
            index: graph.resources.len(),
            ..Resource::default()
        });
        let refusal = target
            .emit(&graph, &no_data())
            .expect_err(&format!("{path}: {name} compiled a type that cannot exist"));
        assert_eq!(
            refusal.rule, "unsupported-resource-type",
            "{path}: {refusal}"
        );
        assert_eq!(refusal.resource, "conformance_probe", "{path}");
    }
}

/// Terraform and OpenTofu read one language. A document written for
/// Terraform must compile to the same configuration on both; only the tool's
/// own name may differ. (OpenTofu takes more than Terraform does, so the
/// reverse is not promised.)
#[test]
fn terraform_and_opentofu_compile_one_document_to_the_same_configuration() {
    let mut compared = 0;
    for (path, name) in examples() {
        if name != "terraform" {
            continue;
        }
        compared += 1;
        let terraform = emitter("terraform")
            .emit(&graph_from_file(&path, "terraform"), &no_data())
            .unwrap_or_else(|e| panic!("terraform: {path}: {e}"));
        let opentofu = emitter("opentofu")
            .emit(&graph_from_file(&path, "opentofu"), &no_data())
            .unwrap_or_else(|e| panic!("opentofu: {path}: {e}"));
        let neutral = |e: &Emitted| -> Vec<(String, String)> {
            e.artifact
                .files
                .iter()
                .map(|(p, c)| {
                    (
                        p.replace(".tofu", ".tf"),
                        c.replace("OpenTofu", "Terraform")
                            .replace("opentofu", "terraform")
                            .replace("tofu", "terraform"),
                    )
                })
                .collect()
        };
        assert_eq!(neutral(&terraform), neutral(&opentofu), "{path}");
    }
    assert!(compared > 0, "no Terraform example was found");
}

/// A value that an interpolation places from data reaches a plugin with the
/// escape for a literal: data holding `${web.ip}` is handed over as
/// `$${web.ip}`. What the author wrote in the document is a reference; what
/// the data supplied never becomes one. This holds every target to its half
/// of that rule, at every text param of every example:
///
/// - the text between the braces is still in the output, wherever plain text
///   in that param reaches the output;
/// - the literal is never compiled to what the reference of the same spelling
///   compiles to.
///
/// Every target must have at least one param where this was seen to hold.
#[test]
fn text_given_as_a_literal_is_never_a_reference_on_any_target() {
    use meridian_sdk::Value;

    const PLAIN: &str = "zzqplain";
    const NAME: &str = "zzqlit.attr";
    let literal = format!("$${{{NAME}}}");
    let reference = format!("${{{NAME}}}");
    // A literal in the middle of longer text, as a join gives it.
    let joined = format!("a $${{{NAME}}}/b");

    let files = |emitted: &Emitted| -> String {
        emitted
            .artifact
            .files
            .iter()
            .map(|(path, content)| format!("{path}\n{content}\n"))
            .collect()
    };

    let mut failures = Vec::new();
    for target in emitters() {
        let name = target.name().to_owned();
        let mut carried = 0;
        for (path, example_target) in examples() {
            if example_target != name {
                continue;
            }
            let graph = graph_from_file(&path, &name);
            for index in 0..graph.resources.len() {
                let texts: Vec<String> = graph.resources[index]
                    .params
                    .iter()
                    .filter(|(_, value)| matches!(value, Value::String(_)))
                    .map(|(key, _)| key.clone())
                    .collect();
                for key in texts {
                    let with = |text: &str| {
                        let mut changed = graph.clone();
                        changed.resources[index]
                            .params
                            .insert(key.clone(), Value::String(text.to_owned()));
                        target.emit(&changed, &no_data())
                    };
                    let place = format!(
                        "{name}: {path}: resource {:?} param {key:?}",
                        graph.resources[index].id
                    );
                    let plain = with(PLAIN);
                    for given in [&literal, &joined] {
                        let as_literal = with(given);
                        // A literal is text with a dollar sign and braces in
                        // it, which a name, a path or a command may not be
                        // allowed to hold. Such a refusal is about the text
                        // and is the target's to make.
                        if let (Ok(plain), Ok(as_literal)) = (&plain, &as_literal) {
                            let plain = files(plain);
                            let out = files(as_literal);
                            if plain.contains(PLAIN) {
                                if out.contains(NAME) {
                                    carried += 1;
                                } else {
                                    failures.push(format!(
                                        "{place}: {given:?} is not in the output as text"
                                    ));
                                }
                            }
                            if let Ok(as_reference) = with(&reference) {
                                if given == &literal && files(&as_reference) == out {
                                    failures.push(format!(
                                        "{place}: the literal {given:?} compiles to the \
                                         same files as the reference {reference:?}"
                                    ));
                                }
                            }
                        }
                    }
                }
            }
        }
        if carried == 0 {
            failures.push(format!(
                "{name}: no text param of any example carried a literal into the output, so \
                 nothing was checked for this target"
            ));
        }
    }
    assert!(
        failures.is_empty(),
        "{} failures:\n{}",
        failures.len(),
        failures.join("\n")
    );
}
