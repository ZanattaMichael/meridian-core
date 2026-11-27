//! The inverse of a compiled play: the play `meridian destroy` runs to remove
//! what the forward play made.
//!
//! It is derived from the artifact the plugin itself wrote, so nothing about
//! the document is needed again, and it is written in reverse task order so a
//! service is stopped before the package it needs is removed. A task whose
//! module has an absent (or stopped) state becomes that state; a task whose
//! module has none is left in place and reported.

use meridian_import::yaml::{self, Rules};
use meridian_yaml::Yaml;

use crate::emit::value_node;

/// The file the destroy play is written to, beside the forward play.
pub const DESTROY_PLAYBOOK_PATH: &str = "playbook.destroy.yml";

/// A task the inverse cannot remove, and why.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Kept {
    pub resource: String,
    pub reason: String,
}

/// The destroy play, as the YAML to write, and the resources it cannot
/// remove. `playbook` is the text of the compiled `playbook.yml`.
pub(crate) fn destroy_playbook(playbook: &str) -> Result<(String, Vec<Kept>), String> {
    let documents = yaml::read(crate::PLAYBOOK_PATH, playbook, Rules::Yaml11)
        .map_err(|e| format!("reading the compiled playbook: {e}"))?;
    let document = documents
        .into_iter()
        .next()
        .ok_or_else(|| "the compiled playbook is empty".to_owned())?;
    // A playbook is a sequence of plays; the artifact is always one play.
    let play = match document.as_seq().and_then(|plays| plays.first()) {
        Some(play) => play.clone(),
        None => document.clone(),
    };
    let name = play
        .get("name")
        .and_then(|n| n.as_str())
        .unwrap_or_default()
        .to_owned();
    let hosts = play
        .get("hosts")
        .and_then(|n| n.as_str())
        .unwrap_or_default()
        .to_owned();
    let tasks = play
        .get("tasks")
        .and_then(|n| n.as_seq())
        .ok_or_else(|| "the compiled playbook has no tasks".to_owned())?;

    let mut inverse = Vec::with_capacity(tasks.len());
    let mut kept = Vec::new();
    // Reverse order: what was made last is removed first.
    for task in tasks.iter().rev() {
        match inverse_task(task) {
            Ok(node) => inverse.push(node),
            Err((resource, reason)) => kept.push(Kept { resource, reason }),
        }
    }

    let document = Yaml::Seq(vec![Yaml::Map(vec![
        ("name".to_owned(), Yaml::str(name)),
        ("hosts".to_owned(), Yaml::str(hosts)),
        ("tasks".to_owned(), Yaml::Seq(inverse)),
    ])]);
    Ok((format!("---\n{}", meridian_yaml::write(&document)), kept))
}

/// The task that removes what `task` made, or the resource and the reason it
/// cannot.
fn inverse_task(task: &meridian_import::yaml::Node) -> Result<Yaml, (String, String)> {
    let map = task
        .as_map()
        .ok_or_else(|| (String::new(), "a task is not a mapping".to_owned()))?;
    let field = |key: &str| map.iter().find(|(k, _)| k == key).map(|(_, v)| v);
    let resource = field("name")
        .and_then(|n| n.as_str())
        .unwrap_or_default()
        .to_owned();
    // The module is the one key whose value is a mapping; the keys beside it
    // are the structural ones, `name`, `notify`, `when` and `no_log`.
    let (module, args) = map
        .iter()
        .find_map(|(k, v)| v.as_map().map(|m| (k.clone(), m)))
        .ok_or_else(|| (resource.clone(), "the task names no module".to_owned()))?;
    let arg = |key: &str| args.iter().find(|(k, _)| k == key).map(|(_, v)| v);
    let absent = |key: &str| -> Result<Vec<(String, Yaml)>, (String, String)> {
        let value = arg(key).ok_or_else(|| {
            (
                resource.clone(),
                format!("the {module} task has no {key} to identify what it made"),
            )
        })?;
        Ok(vec![
            (key.to_owned(), value_node(&value.value())),
            ("state".to_owned(), Yaml::str("absent")),
        ])
    };

    let (module, args): (String, Vec<(String, Yaml)>) = match module.as_str() {
        "ansible.builtin.service" => {
            let name = arg("name")
                .ok_or_else(|| (resource.clone(), "the service task has no name".to_owned()))?;
            (
                "ansible.builtin.service".to_owned(),
                vec![
                    ("name".to_owned(), value_node(&name.value())),
                    ("state".to_owned(), Yaml::str("stopped")),
                    ("enabled".to_owned(), Yaml::Bool(false)),
                ],
            )
        }
        "ansible.builtin.package"
        | "ansible.builtin.user"
        | "ansible.builtin.group"
        | "ansible.builtin.cron"
        | "ansible.builtin.deb822_repository"
        | "ansible.builtin.yum_repository" => (module.clone(), absent("name")?),
        "ansible.builtin.file" => ("ansible.builtin.file".to_owned(), absent("path")?),
        // A file written with literal content is a `copy`; removing it is the
        // `file` module's absent state on the same path.
        "ansible.builtin.copy" => ("ansible.builtin.file".to_owned(), absent("dest")?),
        other => {
            return Err((
                resource,
                format!("{other} has no absent state, so Meridian cannot remove what it made"),
            ))
        }
    };

    let mut pairs = vec![
        ("name".to_owned(), Yaml::str(resource)),
        (module, Yaml::Map(args)),
    ];
    if let Some(when) = field("when").and_then(|n| n.as_str()) {
        pairs.push(("when".to_owned(), Yaml::str(when)));
    }
    Ok(Yaml::Map(pairs))
}

#[cfg(test)]
mod tests {
    use super::*;

    const PLAY: &str = "\
---
- name: web
  hosts: web01.example.com
  tasks:
    - name: nginx_pkg
      ansible.builtin.package:
        name: nginx
        state: present
    - name: nginx_conf
      ansible.builtin.copy:
        content: server {}
        dest: /etc/nginx/nginx.conf
    - name: nginx_svc
      ansible.builtin.service:
        name: nginx
        state: started
      notify:
        - restart nginx_svc
";

    fn invert(text: &str) -> (String, Vec<Kept>) {
        destroy_playbook(text).expect("the play inverts")
    }

    #[test]
    fn tasks_are_inverted_in_reverse_order() {
        let (yaml, kept) = invert(PLAY);
        assert!(kept.is_empty());
        // The service is stopped before the copy it notified is removed, and
        // that before the package is.
        let service = yaml.find("ansible.builtin.service").unwrap();
        let file = yaml.find("ansible.builtin.file").unwrap();
        let package = yaml.find("ansible.builtin.package").unwrap();
        assert!(service < file && file < package, "order:\n{yaml}");
        assert!(yaml.contains("state: stopped"), "{yaml}");
        assert!(yaml.contains("enabled: false"), "{yaml}");
        assert_eq!(yaml.matches("state: absent").count(), 2, "{yaml}");
        // The handler is never inverted: nothing is left to react to.
        assert!(!yaml.contains("notify"), "{yaml}");
    }

    #[test]
    fn a_module_with_no_absent_state_is_kept_with_its_resource() {
        let play = "\
---
- name: web
  hosts: web01
  tasks:
    - name: migrate
      ansible.builtin.command:
        cmd: /usr/local/bin/migrate
";
        let (yaml, kept) = invert(play);
        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0].resource, "migrate");
        assert!(kept[0].reason.contains("no absent state"));
        assert!(!yaml.contains("migrate"), "{yaml}");
    }
}
