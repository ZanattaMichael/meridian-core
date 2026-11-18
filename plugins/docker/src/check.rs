//! What can be known to be wrong with a Compose file before Docker sees it.
//!
//! Compose would refuse every one of these itself, but only when the project
//! is planned or brought up, and in terms of the compiled file. Checked here
//! they are refused when the document compiles, against the line the author
//! wrote.

use std::collections::BTreeMap;

use meridian_sdk::rules::{self, Rule};
use meridian_sdk::{CompileError, Value, Warning};

use crate::{refuse_at, Composition, Element, Section, NAME};

/// The attributes of a service, from the Compose specification.
const SERVICE_KEYS: &[&str] = &[
    "annotations",
    "attach",
    "blkio_config",
    "build",
    "cap_add",
    "cap_drop",
    "cgroup",
    "cgroup_parent",
    "command",
    "configs",
    "container_name",
    "cpu_count",
    "cpu_percent",
    "cpu_period",
    "cpu_quota",
    "cpu_rt_period",
    "cpu_rt_runtime",
    "cpu_shares",
    "cpus",
    "cpuset",
    "credential_spec",
    "depends_on",
    "deploy",
    "develop",
    "device_cgroup_rules",
    "devices",
    "dns",
    "dns_opt",
    "dns_search",
    "domainname",
    "driver_opts",
    "entrypoint",
    "env_file",
    "environment",
    "expose",
    "extends",
    "external_links",
    "extra_hosts",
    "gpus",
    "group_add",
    "healthcheck",
    "hostname",
    "image",
    "init",
    "ipc",
    "isolation",
    "label_file",
    "labels",
    "links",
    "logging",
    "mac_address",
    "mem_limit",
    "mem_reservation",
    "mem_swappiness",
    "memswap_limit",
    "models",
    "network_mode",
    "networks",
    "oom_kill_disable",
    "oom_score_adj",
    "pid",
    "pids_limit",
    "platform",
    "ports",
    "post_start",
    "pre_stop",
    "privileged",
    "profiles",
    "provider",
    "pull_policy",
    "read_only",
    "restart",
    "runtime",
    "scale",
    "secrets",
    "security_opt",
    "shm_size",
    "stdin_open",
    "stop_grace_period",
    "stop_signal",
    "storage_opt",
    "sysctls",
    "tmpfs",
    "tty",
    "ulimits",
    "use_api_socket",
    "user",
    "userns_mode",
    "uts",
    "volumes",
    "volumes_from",
    "working_dir",
];

const NETWORK_KEYS: &[&str] = &[
    "attachable",
    "driver",
    "driver_opts",
    "enable_ipv4",
    "enable_ipv6",
    "external",
    "internal",
    "ipam",
    "labels",
    "name",
];

const VOLUME_KEYS: &[&str] = &["driver", "driver_opts", "external", "labels", "name"];

const CONFIG_KEYS: &[&str] = &[
    "content",
    "environment",
    "external",
    "file",
    "labels",
    "name",
    "template_driver",
];

const SECRET_KEYS: &[&str] = &[
    "driver",
    "driver_opts",
    "environment",
    "external",
    "file",
    "labels",
    "name",
    "template_driver",
];

/// What a dependency can wait for.
const CONDITIONS: &[&str] = &[
    "service_completed_successfully",
    "service_healthy",
    "service_started",
];

fn keys_of(section: Section) -> &'static [&'static str] {
    match section {
        Section::Service => SERVICE_KEYS,
        Section::Network => NETWORK_KEYS,
        Section::Volume => VOLUME_KEYS,
        Section::Config => CONFIG_KEYS,
        Section::Secret => SECRET_KEYS,
    }
}

/// Refuses an attribute Compose does not know, naming the one that was
/// probably meant. An extension field, which starts with `x-`, is the
/// author's own.
pub(crate) fn attributes(element: &Element) -> Result<(), CompileError> {
    for name in element.body.keys() {
        if !keys_of(element.section).contains(&name.as_str()) && !name.starts_with("x-") {
            return Err(refuse_at(
                &element.id,
                &element.pos,
                &rules::UNSUPPORTED_PARAM,
                unknown_key(element.section, name),
            ));
        }
    }
    Ok(())
}

fn unknown_key(section: Section, name: &str) -> String {
    let nearest = keys_of(section)
        .iter()
        .map(|key| (distance(name, key), *key))
        .filter(|(d, _)| *d <= 2)
        .min();
    let hint = match nearest {
        Some((_, key)) => format!("did you mean {key:?}?"),
        None => "an extension field starts with \"x-\"".to_owned(),
    };
    format!(
        "{name:?} is not an attribute of a Compose {}; {hint}",
        section.type_name()
    )
}

/// The edit distance between two short words.
fn distance(a: &str, b: &str) -> usize {
    let (a, b): (Vec<char>, Vec<char>) = (a.chars().collect(), b.chars().collect());
    let mut row: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.iter().enumerate() {
        let mut diagonal = row[0];
        row[0] = i + 1;
        for (j, cb) in b.iter().enumerate() {
            let substituted = diagonal + usize::from(ca != cb);
            diagonal = row[j + 1];
            row[j + 1] = substituted.min(row[j] + 1).min(diagonal + 1);
        }
    }
    row[b.len()]
}

/// Whether a service declares a healthcheck Compose can wait for.
pub(crate) fn has_healthcheck(service: &Element) -> bool {
    match service.body.get("healthcheck") {
        Some(Value::Map(fields)) => fields.get("disable") != Some(&Value::Bool(true)),
        _ => false,
    }
}

/// The services a service waits for, with the condition it waits on. The
/// short form, a list of names, waits for each to have started.
fn dependencies(service: &Element) -> Vec<(&str, &str)> {
    match service.body.get("depends_on") {
        Some(Value::List(names)) => names
            .iter()
            .filter_map(|name| Some((name.as_str()?, "service_started")))
            .collect(),
        Some(Value::Map(entries)) => entries
            .iter()
            .map(|(name, fields)| {
                let condition = fields
                    .as_map()
                    .and_then(|f| f.get("condition"))
                    .and_then(Value::as_str)
                    .unwrap_or("service_started");
                (name.as_str(), condition)
            })
            .collect(),
        _ => Vec::new(),
    }
}

/// Checks everything a service names: the services it waits for, and the
/// networks, volumes, configs and secrets it uses.
pub(crate) fn references(composition: &Composition) -> Result<(), CompileError> {
    let element = |id: &str| composition.elements.iter().find(|e| e.id == id);
    for service in composition
        .elements
        .iter()
        .filter(|e| e.section == Section::Service)
    {
        let refuse = |rule: &Rule, msg: String| refuse_at(&service.id, &service.pos, rule, msg);
        for (name, condition) in dependencies(service) {
            if name == service.id {
                return Err(refuse(
                    &rules::INVALID_REFERENCE,
                    "a service cannot depend on itself".to_owned(),
                ));
            }
            let other = match element(name) {
                Some(other) if other.section == Section::Service => other,
                Some(other) => {
                    return Err(refuse(
                        &rules::INVALID_REFERENCE,
                        format!(
                            "depends_on names {name:?}, which is a {} and not a service",
                            other.section.type_name()
                        ),
                    ))
                }
                None => {
                    return Err(refuse(
                        &rules::INVALID_REFERENCE,
                        format!(
                            "depends_on names {name:?}, which is not a service in this document"
                        ),
                    ))
                }
            };
            if !CONDITIONS.contains(&condition) {
                return Err(refuse(
                    &rules::INVALID_PARAM,
                    format!(
                        "{condition:?} is not something a dependency can wait for (accepted: {})",
                        CONDITIONS.join(", ")
                    ),
                ));
            }
            if condition == "service_healthy" && !has_healthcheck(other) {
                return Err(refuse(
                    &rules::INVALID_PARAM,
                    format!(
                        "depends_on waits for {name:?} to be healthy, and that service declares \
                         no healthcheck, so it never would be"
                    ),
                ));
            }
        }
        for (section, name) in used(service) {
            // Every service is on the network named default unless it says
            // otherwise, whether or not the file declares it.
            if section == Section::Network && name == "default" {
                continue;
            }
            match element(&name) {
                Some(other) if other.section == section => {}
                Some(other) => {
                    return Err(refuse(
                        &rules::INVALID_REFERENCE,
                        format!(
                            "uses {name:?} as a {}, and the document declares it as a {}",
                            section.type_name(),
                            other.section.type_name()
                        ),
                    ))
                }
                None => {
                    return Err(refuse(
                        &rules::INVALID_REFERENCE,
                        format!(
                            "uses the {kind} {name:?}, which the document does not declare. Add \
                             a resource of type {kind:?} with that id",
                            kind = section.type_name()
                        ),
                    ))
                }
            }
        }
    }

    if let Some(cycle) = find_cycle(composition) {
        let first = element(&cycle[0]).expect("a cycle holds only services of the project");
        return Err(refuse_at(
            &first.id,
            &first.pos,
            &rules::DEPENDENCY_CYCLE,
            format!(
                "the services depend on each other in a circle ({}), so Compose could start \
                 none of them first. A notify counts: the notified service starts after the \
                 one that notifies it",
                cycle.join(" -> ")
            ),
        ));
    }
    Ok(())
}

fn field<'v>(fields: &'v BTreeMap<String, Value>, name: &str) -> Option<&'v str> {
    fields.get(name).and_then(Value::as_str)
}

/// The networks, named volumes, configs and secrets a service says it uses.
fn used(service: &Element) -> Vec<(Section, String)> {
    let mut found = Vec::new();
    let mut add = |section: Section, name: &str| {
        if !name.is_empty() {
            found.push((section, name.to_owned()));
        }
    };

    match service.body.get("networks") {
        Some(Value::List(items)) => {
            for name in items.iter().filter_map(Value::as_str) {
                add(Section::Network, name);
            }
        }
        Some(Value::Map(entries)) => {
            for name in entries.keys() {
                add(Section::Network, name);
            }
        }
        _ => {}
    }

    if let Some(Value::List(mounts)) = service.body.get("volumes") {
        for mount in mounts {
            match mount {
                Value::String(short) => {
                    if let Some(name) = named_volume(short) {
                        add(Section::Volume, name);
                    }
                }
                Value::Map(fields) if field(fields, "type") == Some("volume") => {
                    if let Some(source) = field(fields, "source") {
                        add(Section::Volume, source);
                    }
                }
                _ => {}
            }
        }
    }

    for (attribute, section) in [("configs", Section::Config), ("secrets", Section::Secret)] {
        if let Some(Value::List(items)) = service.body.get(attribute) {
            for item in items {
                match item {
                    Value::String(name) => add(section, name),
                    Value::Map(fields) => {
                        if let Some(source) = field(fields, "source") {
                            add(section, source);
                        }
                    }
                    _ => {}
                }
            }
        }
    }
    found
}

/// The named volume a short-form mount uses, if it uses one.
///
/// `data:/var/lib/data` mounts the volume `data`. `/var/lib/data` alone is an
/// anonymous volume, and a source that is a path (`./data`, `/srv/data`,
/// `~/data`, `C:\data`) is a bind mount.
fn named_volume(short: &str) -> Option<&str> {
    let (source, rest) = short.split_once(':')?;
    let path = source.starts_with(['.', '/', '~', '\\'])
        || (source.len() == 1 && rest.starts_with(['\\', '/']));
    (!path).then_some(source)
}

/// One circle of services that depend on each other, as the ids along it
/// with the first repeated at the end, or `None` when there is none.
fn find_cycle(composition: &Composition) -> Option<Vec<String>> {
    #[derive(Clone, Copy, PartialEq)]
    enum Mark {
        Visiting,
        Done,
    }
    fn visit<'a>(
        id: &'a str,
        composition: &'a Composition,
        marks: &mut BTreeMap<&'a str, Mark>,
        path: &mut Vec<&'a str>,
    ) -> Option<Vec<String>> {
        match marks.get(id) {
            Some(Mark::Done) => return None,
            Some(Mark::Visiting) => {
                let start = path.iter().position(|p| *p == id).unwrap_or(0);
                let mut cycle: Vec<String> =
                    path[start..].iter().map(|p| (*p).to_owned()).collect();
                cycle.push(id.to_owned());
                return Some(cycle);
            }
            None => {}
        }
        marks.insert(id, Mark::Visiting);
        path.push(id);
        if let Some(service) = composition.elements.iter().find(|e| e.id == id) {
            for (name, _) in dependencies(service) {
                if let Some(cycle) = visit(name, composition, marks, path) {
                    return Some(cycle);
                }
            }
        }
        path.pop();
        marks.insert(id, Mark::Done);
        None
    }

    let mut marks = BTreeMap::new();
    composition
        .elements
        .iter()
        .filter(|e| e.section == Section::Service)
        .find_map(|service| visit(&service.id, composition, &mut marks, &mut Vec::new()))
}

fn is_absolute(path: &str) -> bool {
    path.starts_with('/')
        || path.starts_with('~')
        || (path.len() > 2
            && path.as_bytes()[1] == b':'
            && path.as_bytes()[0].is_ascii_alphabetic())
}

/// Whether a path is one Compose would resolve against the project
/// directory. A URL is not a path at all.
fn is_relative(path: &str) -> bool {
    !path.is_empty() && !is_absolute(path) && !path.contains("://") && !path.starts_with("git@")
}

/// A warning for every relative host path an element names.
///
/// Compose resolves one against the project directory, which is the
/// directory Meridian keeps the compiled file in unless the project names
/// another. Nothing the author keeps beside the document is there.
pub(crate) fn relative_paths(element: &Element) -> Vec<Warning> {
    let mut found: Vec<(&'static str, String)> = Vec::new();
    let mut check = |what: &'static str, path: &str| {
        if is_relative(path) {
            found.push((what, path.to_owned()));
        }
    };
    match element.section {
        Section::Service => {
            match element.body.get("build") {
                Some(Value::String(context)) => check("the build context", context),
                Some(Value::Map(fields)) => {
                    if let Some(context) = field(fields, "context") {
                        check("the build context", context);
                    }
                }
                _ => {}
            }
            match element.body.get("env_file") {
                Some(Value::String(path)) => check("the env_file", path),
                Some(Value::List(items)) => {
                    for item in items {
                        match item {
                            Value::String(path) => check("the env_file", path),
                            Value::Map(fields) => {
                                if let Some(path) = field(fields, "path") {
                                    check("the env_file", path);
                                }
                            }
                            _ => {}
                        }
                    }
                }
                _ => {}
            }
            if let Some(Value::List(mounts)) = element.body.get("volumes") {
                for mount in mounts {
                    match mount {
                        // In the short form only a source that starts with a
                        // dot is a path; a bare word is a named volume.
                        Value::String(short) => {
                            if let Some((source, _)) = short.split_once(':') {
                                if source.starts_with('.') {
                                    check("the bind mount", source);
                                }
                            }
                        }
                        Value::Map(fields) if field(fields, "type") == Some("bind") => {
                            if let Some(source) = field(fields, "source") {
                                check("the bind mount", source);
                            }
                        }
                        _ => {}
                    }
                }
            }
        }
        Section::Config | Section::Secret => {
            if let Some(path) = field(&element.body, "file") {
                check("the file", path);
            }
        }
        Section::Network | Section::Volume => {}
    }
    found
        .into_iter()
        .map(|(what, path)| Warning {
            target: NAME.to_owned(),
            resource: element.id.clone(),
            msg: format!(
                "{what} {path:?} is a relative path, which Compose resolves against the \
                 directory Meridian keeps the compiled file in and not the document's. Write an \
                 absolute path, or name the directory with \"directory\" on a resource of type \
                 \"project\""
            ),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_misspelt_attribute_is_matched_to_the_nearest_real_one() {
        assert_eq!(distance("enviroment", "environment"), 1);
        assert_eq!(distance("port", "ports"), 1);
        assert_eq!(distance("image", "image"), 0);
        assert_eq!(
            unknown_key(Section::Service, "enviroment"),
            "\"enviroment\" is not an attribute of a Compose service; did you mean \"environment\"?"
        );
        assert_eq!(
            unknown_key(Section::Volume, "replicas"),
            "\"replicas\" is not an attribute of a Compose volume; an extension field starts with \"x-\""
        );
    }

    #[test]
    fn the_attribute_lists_are_sorted_so_a_suggestion_is_stable() {
        for section in Section::ALL {
            let keys = keys_of(section);
            let mut sorted = keys.to_vec();
            sorted.sort_unstable();
            assert_eq!(keys, sorted, "{}", section.type_name());
        }
    }

    #[test]
    fn a_short_mount_names_a_volume_unless_its_source_is_a_path() {
        for (short, want) in [
            ("data:/var/lib/data", Some("data")),
            ("data:/var/lib/data:ro", Some("data")),
            ("/var/lib/data", None),
            ("./data:/var/lib/data", None),
            ("../data:/var/lib/data", None),
            ("/srv/data:/var/lib/data", None),
            ("~/data:/var/lib/data", None),
            ("C:\\data:/var/lib/data", None),
            ("C:/data:/var/lib/data", None),
        ] {
            assert_eq!(named_volume(short), want, "{short:?}");
        }
    }

    #[test]
    fn a_path_is_relative_unless_it_is_absolute_or_a_url() {
        for (path, want) in [
            ("./app", true),
            ("app", true),
            ("../shared/.env", true),
            ("/srv/app", false),
            ("~/app", false),
            ("C:\\app", false),
            ("https://github.com/acme/app.git", false),
            ("git@github.com:acme/app.git", false),
            ("", false),
        ] {
            assert_eq!(is_relative(path), want, "{path:?}");
        }
    }
}
