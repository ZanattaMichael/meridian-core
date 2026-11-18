//! Stage four: a document becomes its final files.

use std::collections::BTreeMap;
use std::net::IpAddr;

use meridian_sdk::Value;
use meridian_yaml::Yaml;

use crate::{CloudConfig, Group, Module, Package, User, WriteFile};

/// The artifact's file names, shared with the runner. They are the names
/// cloud-init's NoCloud data source looks for, so the artifact is a seed
/// directory as it stands.
pub const USER_DATA_PATH: &str = "user-data";
pub const META_DATA_PATH: &str = "meta-data";

/// The first line cloud-init requires of a cloud-config document. Without
/// it, user-data is taken for a script or ignored.
const HEADER: &str = "#cloud-config";

/// Renders a document. This stage performs no mapping and no ordering:
/// everything it writes was decided by transform and sort, so the same
/// document always renders to the same bytes.
pub(crate) fn serialise(config: &CloudConfig) -> BTreeMap<String, String> {
    let mut document = Vec::new();
    // The keys follow the order of a first boot, as far as one list of files
    // allows: the deferred ones are written after the packages.
    if !config.files.is_empty() {
        let files = config.files.iter().map(|f| file_node(&f.item)).collect();
        document.push(("write_files".to_owned(), Yaml::Seq(files)));
    }
    if !config.groups.is_empty() {
        let groups = config.groups.iter().map(|g| group_node(&g.item)).collect();
        document.push(("groups".to_owned(), Yaml::Seq(groups)));
    }
    if !config.users.is_empty() {
        // Listing any user replaces cloud-init's default list, which holds
        // the image's own login user. It is kept, because losing the account
        // a cloud's console and key injection rely on is not something a
        // document that adds a user asked for.
        let mut users = vec![Yaml::str("default")];
        users.extend(config.users.iter().map(|u| user_node(&u.item)));
        document.push(("users".to_owned(), Yaml::Seq(users)));
    }
    if !config.packages.is_empty() {
        let packages = config
            .packages
            .iter()
            .map(|p| package_node(&p.item))
            .collect();
        document.push(("packages".to_owned(), Yaml::Seq(packages)));
    }
    if !config.commands.is_empty() {
        // cloud-init writes every entry into one script and runs it with sh,
        // which carries on after a failure and reports only the last
        // command's status. This line makes the first failure stop the
        // script, so a resource never runs after one it depends on failed and
        // the boot is reported as failed.
        let mut commands = vec![Yaml::str("set -e")];
        commands.extend(config.commands.iter().map(Yaml::str));
        document.push(("runcmd".to_owned(), Yaml::Seq(commands)));
    }

    // The modules of cloud-init's own follow, under their keys: cloud-init
    // reads the document as one mapping, so their place in it means nothing.
    for module in &config.modules {
        let Module { key, value } = &module.item;
        document.push((key.clone(), value_node(value)));
    }

    let user_data = format!(
        "{HEADER}\n# Compiled by Meridian from the document {:?}. Change the document, not this file.\n{}",
        config.document,
        meridian_yaml::write(&Yaml::Map(document))
    );

    let mut meta = vec![(
        "instance-id".to_owned(),
        Yaml::str(format!("meridian-{}", config.document)),
    )];
    // An address reaches a machine but does not name it.
    if config.host.parse::<IpAddr>().is_err() {
        meta.push(("local-hostname".to_owned(), Yaml::str(&config.host)));
    }

    BTreeMap::from([
        (USER_DATA_PATH.to_owned(), user_data),
        (
            META_DATA_PATH.to_owned(),
            meridian_yaml::write(&Yaml::Map(meta)),
        ),
    ])
}

/// Lays a file out in a fixed key order. Key order is part of the output's
/// identity, so it is decided here once.
fn file_node(file: &WriteFile) -> Yaml {
    let mut pairs = vec![
        ("path".to_owned(), Yaml::str(&file.path)),
        ("content".to_owned(), Yaml::str(&file.content)),
    ];
    // cloud-init takes both in one value and defaults each to root.
    let owner = match (file.owner.as_str(), file.group.as_str()) {
        ("", "") => String::new(),
        (owner, "") => owner.to_owned(),
        ("", group) => format!("root:{group}"),
        (owner, group) => format!("{owner}:{group}"),
    };
    if !owner.is_empty() {
        pairs.push(("owner".to_owned(), Yaml::Str(owner)));
    }
    if !file.permissions.is_empty() {
        pairs.push(("permissions".to_owned(), Yaml::str(&file.permissions)));
    }
    if !file.encoding.is_empty() {
        pairs.push(("encoding".to_owned(), Yaml::str(&file.encoding)));
    }
    if file.append {
        pairs.push(("append".to_owned(), Yaml::Bool(true)));
    }
    if file.defer {
        pairs.push(("defer".to_owned(), Yaml::Bool(true)));
    }
    Yaml::Map(pairs)
}

/// A group is its name, or its name over its members.
fn group_node(group: &Group) -> Yaml {
    if group.members.is_empty() {
        return Yaml::str(&group.name);
    }
    Yaml::Map(vec![(
        group.name.clone(),
        Yaml::Seq(group.members.iter().map(Yaml::str).collect()),
    )])
}

fn user_node(user: &User) -> Yaml {
    let mut pairs = vec![("name".to_owned(), Yaml::str(&user.name))];
    pairs.extend(
        user.fields
            .iter()
            .map(|(key, value)| (key.clone(), value_node(value))),
    );
    Yaml::Map(pairs)
}

/// A package is its name, or a name and version pair.
fn package_node(package: &Package) -> Yaml {
    if package.version.is_empty() {
        return Yaml::str(&package.name);
    }
    Yaml::Seq(vec![Yaml::str(&package.name), Yaml::str(&package.version)])
}

/// Renders a param value. Map keys arrive sorted, so a value serialises
/// identically every time.
fn value_node(value: &Value) -> Yaml {
    match value {
        Value::Null => Yaml::Null,
        Value::Bool(b) => Yaml::Bool(*b),
        Value::Int(i) => Yaml::Int(*i),
        Value::Uint(u) => Yaml::Uint(*u),
        Value::Float(f) => Yaml::Float(*f),
        Value::String(s) => Yaml::str(s),
        // A plugin is never sent a secret as a value; one would be written as its mask.
        Value::Secret(_) => Yaml::str(meridian_sdk::SENSITIVE),
        Value::List(items) => Yaml::Seq(items.iter().map(value_node).collect()),
        Value::Map(map) => Yaml::Map(
            map.iter()
                .map(|(key, value)| (key.clone(), value_node(value)))
                .collect(),
        ),
    }
}
