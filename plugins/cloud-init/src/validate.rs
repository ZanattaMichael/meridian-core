//! Stage three: the constraints that are cloud-init's and not Meridian's.

use std::collections::HashMap;

use meridian_sdk::rules;
use meridian_sdk::CompileError;

use crate::{CloudConfig, NAME};

/// The values cloud-init accepts for a file's `encoding`.
pub(crate) const ENCODINGS: &[&str] = &[
    "b64",
    "base64",
    "gz",
    "gz+b64",
    "gz+base64",
    "gzip",
    "gzip+b64",
    "gzip+base64",
    "text/plain",
];

/// Checks a document before anything is serialised. Every failure names the
/// offending resource and the rule it broke, so the message points at the
/// line the author must change.
pub(crate) fn validate(config: &CloudConfig) -> Result<(), CompileError> {
    if config.host.is_empty() {
        return Err(CompileError {
            target: NAME.to_owned(),
            rule: rules::MISSING_HOST.name.to_owned(),
            hint: rules::MISSING_HOST.hint.to_owned(),
            msg: "the document has no target host".to_owned(),
            ..CompileError::default()
        });
    }

    // cloud-init writes the list in order, so a second entry for a path
    // silently replaces the first unless it appends to it.
    let mut paths: HashMap<&str, &str> = HashMap::new();
    for file in &config.files {
        let entry = &file.item;
        if let Some(previous) = paths.insert(&entry.path, &file.id) {
            if !entry.append {
                return Err(file.refuse(
                    &rules::DUPLICATE_RESOURCE,
                    format!(
                        "path {:?} is already written by resource {:?}; \
                         set append: true if this one adds to it",
                        entry.path, previous
                    ),
                ));
            }
        }
        if !entry.permissions.is_empty() && !is_mode(&entry.permissions) {
            return Err(file.refuse(
                &rules::INVALID_PARAM,
                format!(
                    "mode {:?} is not an octal file mode such as \"0644\"",
                    entry.permissions
                ),
            ));
        }
        if !entry.encoding.is_empty() && !ENCODINGS.contains(&entry.encoding.as_str()) {
            return Err(file.refuse(
                &rules::INVALID_PARAM,
                format!(
                    "encoding {:?} is not one cloud-init reads (accepted: {})",
                    entry.encoding,
                    ENCODINGS.join(", ")
                ),
            ));
        }
        // The two are written as one `owner:group` value.
        for (what, value) in [("owner", &entry.owner), ("group", &entry.group)] {
            if value.contains(':') {
                return Err(file.refuse(
                    &rules::INVALID_PARAM,
                    format!("{what} {value:?} holds a colon; give the owner and the group as separate params"),
                ));
            }
        }
    }

    let mut names: HashMap<&str, &str> = HashMap::new();
    for user in &config.users {
        check_name(user, "user", &user.item.name)?;
        if let Some(previous) = names.insert(&user.item.name, &user.id) {
            return Err(user.refuse(
                &rules::DUPLICATE_RESOURCE,
                format!(
                    "user {:?} is already created by resource {:?}",
                    user.item.name, previous
                ),
            ));
        }
    }
    names.clear();
    for group in &config.groups {
        check_name(group, "group", &group.item.name)?;
        if let Some(previous) = names.insert(&group.item.name, &group.id) {
            return Err(group.refuse(
                &rules::DUPLICATE_RESOURCE,
                format!(
                    "group {:?} is already created by resource {:?}",
                    group.item.name, previous
                ),
            ));
        }
    }
    // cloud-init reads one mapping, so a key written twice would keep one
    // value and lose the other without a word.
    names.clear();
    for module in &config.modules {
        if let Some(previous) = names.insert(&module.item.key, &module.id) {
            return Err(module.refuse(
                &rules::DUPLICATE_RESOURCE,
                format!(
                    "module {:?} is already written by resource {:?}; one resource writes a \
                     module's whole value",
                    module.item.key, previous
                ),
            ));
        }
    }
    names.clear();
    for package in &config.packages {
        // A colon is part of a package name on a multi-architecture system.
        if package.item.name.chars().any(char::is_whitespace) {
            return Err(package.refuse(
                &rules::INVALID_NAME,
                format!("package name {:?} holds a space", package.item.name),
            ));
        }
        if let Some(previous) = names.insert(&package.item.name, &package.id) {
            return Err(package.refuse(
                &rules::DUPLICATE_RESOURCE,
                format!(
                    "package {:?} is already installed by resource {:?}",
                    package.item.name, previous
                ),
            ));
        }
    }
    Ok(())
}

/// Three or four octal digits, as cloud-init's `permissions` takes them.
fn is_mode(mode: &str) -> bool {
    (3..=4).contains(&mode.len()) && mode.bytes().all(|b| (b'0'..=b'7').contains(&b))
}

fn check_name<T>(placed: &crate::Placed<T>, what: &str, name: &str) -> Result<(), CompileError> {
    if name.chars().any(|c| c.is_whitespace() || c == ':') {
        return Err(placed.refuse(
            &rules::INVALID_NAME,
            format!("{what} name {name:?} holds a space or a colon"),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::{Group, Package, Placed, User, WriteFile};
    use meridian_sdk::Position;

    fn placed<T>(id: &str, item: T) -> Placed<T> {
        Placed {
            id: id.to_owned(),
            pos: Position {
                line: 17,
                ..Position::default()
            },
            item,
        }
    }

    fn file(id: &str, path: &str) -> Placed<WriteFile> {
        placed(
            id,
            WriteFile {
                path: path.to_owned(),
                content: "x".to_owned(),
                ..WriteFile::default()
            },
        )
    }

    fn user(id: &str, name: &str) -> Placed<User> {
        placed(
            id,
            User {
                name: name.to_owned(),
                fields: Default::default(),
            },
        )
    }

    fn group(id: &str, name: &str) -> Placed<Group> {
        placed(
            id,
            Group {
                name: name.to_owned(),
                members: Vec::new(),
            },
        )
    }

    fn package(id: &str, name: &str) -> Placed<Package> {
        placed(
            id,
            Package {
                name: name.to_owned(),
                version: String::new(),
            },
        )
    }

    fn config() -> CloudConfig {
        CloudConfig {
            document: "web".to_owned(),
            host: "web01".to_owned(),
            files: vec![file("conf", "/etc/app.conf")],
            groups: vec![group("ops", "ops")],
            users: vec![user("deploy", "deploy")],
            packages: vec![package("pkg", "nginx")],
            commands: Vec::new(),
            modules: Vec::new(),
        }
    }

    #[test]
    fn each_rule_refuses_the_one_mistake_it_is_for() {
        type Break = fn(&mut CloudConfig);
        let table: &[(&str, &str, Break)] = &[
            ("missing-host", "", |c| c.host.clear()),
            ("duplicate-resource", "again", |c| {
                c.files.push(file("again", "/etc/app.conf"));
            }),
            ("invalid-param", "conf", |c| {
                c.files[0].item.permissions = "rw-r--r--".to_owned();
            }),
            ("invalid-param", "conf", |c| {
                c.files[0].item.permissions = "0o644".to_owned()
            }),
            ("invalid-param", "conf", |c| {
                c.files[0].item.encoding = "utf-8".to_owned()
            }),
            ("invalid-param", "conf", |c| {
                c.files[0].item.encoding = "B64".to_owned()
            }),
            ("invalid-param", "conf", |c| {
                c.files[0].item.owner = "root:root".to_owned()
            }),
            ("invalid-param", "conf", |c| {
                c.files[0].item.group = "root:".to_owned()
            }),
            ("invalid-name", "deploy", |c| {
                c.users[0].item.name = "de ploy".to_owned()
            }),
            ("invalid-name", "deploy", |c| {
                c.users[0].item.name = "deploy:x".to_owned()
            }),
            ("duplicate-resource", "again", |c| {
                c.users.push(user("again", "deploy"))
            }),
            ("invalid-name", "ops", |c| {
                c.groups[0].item.name = "ops\tteam".to_owned()
            }),
            ("invalid-name", "ops", |c| {
                c.groups[0].item.name = "ops:1".to_owned()
            }),
            ("duplicate-resource", "again", |c| {
                c.groups.push(group("again", "ops"))
            }),
            ("invalid-name", "pkg", |c| {
                c.packages[0].item.name = "ng inx".to_owned()
            }),
            ("duplicate-resource", "again", |c| {
                c.packages.push(package("again", "nginx"));
            }),
        ];

        assert_eq!(validate(&config()), Ok(()));
        for (i, (rule, resource, break_it)) in table.iter().enumerate() {
            let mut broken = config();
            break_it(&mut broken);
            let err = validate(&broken).expect_err(rule);
            assert_eq!(err.rule, *rule, "row {i}");
            assert_eq!(err.resource, *resource, "row {i}: {rule}");
            assert_eq!(err.target, NAME, "{rule}");
            assert!(!err.msg.is_empty(), "{rule}");
            if !resource.is_empty() {
                assert_eq!(err.pos.line, 17, "{rule} must point at the resource");
            }
        }
    }

    #[test]
    fn what_looks_alike_but_is_allowed_is_accepted() {
        type Change = fn(&mut CloudConfig);
        let table: &[(&str, Change)] = &[
            ("a second entry that appends to a path", |c| {
                let mut more = file("more", "/etc/app.conf");
                more.item.append = true;
                c.files.push(more);
            }),
            ("a user and a group of one name", |c| {
                c.groups.push(group("g", "deploy"))
            }),
            ("a user and a package of one name", |c| {
                c.packages.push(package("p", "deploy"));
            }),
            ("a package named for an architecture", |c| {
                c.packages.push(package("p", "libc6:i386"));
            }),
            ("a document with nothing in it but a host", |c| {
                c.files.clear();
                c.groups.clear();
                c.users.clear();
                c.packages.clear();
            }),
        ];
        for (what, change) in table {
            let mut c = config();
            change(&mut c);
            assert_eq!(validate(&c), Ok(()), "{what}");
        }
    }

    #[test]
    fn every_encoding_cloud_init_reads_is_accepted() {
        for encoding in ENCODINGS {
            let mut c = config();
            c.files[0].item.encoding = (*encoding).to_owned();
            assert_eq!(validate(&c), Ok(()), "{encoding}");
        }
    }

    #[test]
    fn a_mode_is_three_or_four_octal_digits() {
        for good in ["644", "0644", "1777", "000"] {
            assert!(is_mode(good), "{good}");
        }
        for bad in ["", "64", "06444", "0688", "rwxr-xr-x", "u+x", "0o644"] {
            assert!(!is_mode(bad), "{bad}");
        }
    }
}
