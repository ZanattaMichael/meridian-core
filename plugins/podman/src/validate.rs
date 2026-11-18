//! Stage three: the constraints that are Quadlet's and systemd's, not
//! Meridian's.

use std::collections::BTreeMap;

use meridian_sdk::rules::{self, Rule};
use meridian_sdk::{CompileError, Warning};

use crate::keys::{self, Lookup};
use crate::{refuse_at, Kind, Unit, Units, NAME};

/// The longest unit file stem accepted. systemd's limit for a whole unit name
/// is 255 bytes; this leaves room for the longest suffix Quadlet adds.
const MAX_ID: usize = 200;

/// A resource id that can be a unit file's stem: it starts with a letter or a
/// digit and holds only letters, digits, `_` and `-`. systemd would accept
/// more, but `@` makes a unit a template and a dot would end the name in a
/// `${id.name}` reference.
fn is_unit_stem(id: &str) -> bool {
    id.len() <= MAX_ID
        && id.starts_with(|c: char| c.is_ascii_alphanumeric())
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

/// The keys one of which a kind of unit cannot do without.
fn required(kind: Kind) -> &'static [&'static str] {
    match kind {
        Kind::Container => &["Image", "Rootfs"],
        Kind::Kube => &["Yaml"],
        Kind::Image => &["Image"],
        Kind::Build => &["ImageTag"],
        Kind::Pod | Kind::Volume | Kind::Network => &[],
    }
}

pub(crate) fn validate(units: &Units) -> Result<Vec<Warning>, CompileError> {
    let mut warnings = Vec::new();
    let mut services: BTreeMap<&str, &Unit> = BTreeMap::new();

    for unit in &units.units {
        let refuse = |rule: &Rule, msg: String| refuse_at(&unit.id, &unit.pos, rule, msg);

        if !is_unit_stem(&unit.id) {
            return Err(refuse(
                &rules::INVALID_NAME,
                format!(
                    "resource id {:?} cannot name a unit file: a name starts with a letter or \
                     a digit, holds only letters, digits, underscores and hyphens, and is at \
                     most {MAX_ID} characters long",
                    unit.id
                ),
            ));
        }

        if let Some(other) = services.insert(&unit.service, unit) {
            return Err(refuse(
                &rules::DUPLICATE_RESOURCE,
                format!(
                    "{} and {} would both run as {}; rename one of the resources, or give one \
                     a ServiceName",
                    other.file(),
                    unit.file(),
                    unit.service
                ),
            ));
        }

        let needed = required(unit.kind);
        if !needed.is_empty() && !needed.iter().any(|key| unit.body.contains_key(*key)) {
            return Err(refuse(
                &rules::MISSING_PARAM,
                format!(
                    "resource type {:?} requires param {} on this target",
                    unit.kind.as_str(),
                    needed
                        .iter()
                        .map(|key| format!("{key:?}"))
                        .collect::<Vec<_>>()
                        .join(" or ")
                ),
            ));
        }
        if unit.kind == Kind::Build
            && !unit.body.contains_key("File")
            && !unit.body.contains_key("SetWorkingDirectory")
        {
            return Err(refuse(
                &rules::MISSING_PARAM,
                "resource type \"build\" requires param \"File\" or \"SetWorkingDirectory\" on \
                 this target, to say where the Containerfile is"
                    .to_owned(),
            ));
        }

        for key in unit.body.keys() {
            if keys::lookup(unit.kind, key) == Lookup::Unknown {
                warnings.push(Warning {
                    target: NAME.to_owned(),
                    resource: unit.id.clone(),
                    msg: format!(
                        "{key:?} is not a key this plugin knows for a {} unit. It is written \
                         as given; a Podman that does not know it either will refuse the unit \
                         when it is planned",
                        unit.kind.as_str()
                    ),
                });
            }
        }
    }
    Ok(warnings)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_unit_stem_is_what_systemd_and_quadlet_both_accept() {
        for (id, want) in [
            ("web", true),
            ("web-1", true),
            ("web_db-2", true),
            ("web.v2", false),
            ("9lives", true),
            ("", false),
            ("-web", false),
            ("web@1", false),
            ("web app", false),
            ("web/app", false),
            ("wéb", false),
        ] {
            assert_eq!(is_unit_stem(id), want, "{id:?}");
        }
        assert!(!is_unit_stem(&"a".repeat(MAX_ID + 1)));
    }
}

#[cfg(test)]
mod rule_tables {
    use super::*;
    use meridian_sdk::Position;

    fn unit(kind: Kind, id: &str, keys: &[&str]) -> Unit {
        Unit {
            id: id.to_owned(),
            kind,
            service: format!("{id}.service"),
            runtime_name: None,
            body: keys
                .iter()
                .map(|k| ((*k).to_owned(), vec!["x".to_owned()]))
                .collect(),
            sections: Default::default(),
            linked: Default::default(),
            pos: Position {
                line: 2,
                ..Position::default()
            },
        }
    }

    fn units() -> Units {
        Units {
            document: "site".to_owned(),
            units: vec![
                unit(Kind::Container, "web", &["Image"]),
                unit(Kind::Container, "chroot", &["Rootfs"]),
                unit(Kind::Pod, "site", &[]),
                unit(Kind::Kube, "stack", &["Yaml"]),
                unit(Kind::Volume, "data", &[]),
                unit(Kind::Network, "back", &[]),
                unit(Kind::Image, "base", &["Image"]),
                unit(Kind::Build, "app", &["ImageTag", "File"]),
            ],
            outputs: Vec::new(),
        }
    }

    fn at<'a>(u: &'a mut Units, id: &str) -> &'a mut Unit {
        u.units.iter_mut().find(|x| x.id == id).expect(id)
    }

    #[test]
    fn each_rule_refuses_the_one_mistake_it_is_for() {
        type Break = fn(&mut Units);
        let table: &[(&str, &str, Break)] = &[
            ("invalid-name", "web.v2", |u| {
                at(u, "web").id = "web.v2".to_owned()
            }),
            ("invalid-name", "web@1", |u| {
                at(u, "web").id = "web@1".to_owned()
            }),
            ("duplicate-resource", "chroot", |u| {
                at(u, "chroot").service = "web.service".to_owned();
            }),
            ("missing-param", "web", |u| at(u, "web").body.clear()),
            ("missing-param", "stack", |u| at(u, "stack").body.clear()),
            ("missing-param", "base", |u| at(u, "base").body.clear()),
            ("missing-param", "app", |u| {
                at(u, "app").body.remove("ImageTag");
            }),
            ("missing-param", "app", |u| {
                at(u, "app").body.remove("File");
            }),
        ];

        assert_eq!(validate(&units()), Ok(Vec::new()));
        for (rule, resource, break_it) in table {
            let mut broken = units();
            break_it(&mut broken);
            let err = validate(&broken).expect_err(rule);
            assert_eq!(err.rule, *rule);
            assert_eq!(err.resource, *resource, "{rule}");
            assert_eq!(err.target, NAME, "{rule}");
            assert!(!err.msg.is_empty(), "{rule}");
            assert_eq!(err.pos.line, 2, "{rule} must point at the resource");
        }
    }

    #[test]
    fn what_looks_alike_but_is_allowed_is_accepted() {
        type Change = fn(&mut Units);
        let table: &[(&str, Change)] = &[
            ("a build that names its working directory", |u| {
                let app = at(u, "app");
                app.body.remove("File");
                app.body
                    .insert("SetWorkingDirectory".to_owned(), vec!["unit".to_owned()]);
            }),
            ("a container with both an image and a rootfs", |u| {
                at(u, "web")
                    .body
                    .insert("Rootfs".to_owned(), vec!["/srv".to_owned()]);
            }),
            ("a pod, a volume and a network with nothing set", |_| {}),
            ("nothing at all", |u| u.units.clear()),
        ];
        for (what, change) in table {
            let mut u = units();
            change(&mut u);
            assert_eq!(validate(&u), Ok(Vec::new()), "{what}");
        }
    }

    /// An unknown key is passed on with a warning, one for each, and a key
    /// that is known is passed on in silence.
    #[test]
    fn an_unknown_key_warns_and_does_not_refuse() {
        let mut u = units();
        let web = at(&mut u, "web");
        web.body.insert("Imgae".to_owned(), vec!["x".to_owned()]);
        web.body.insert("NotAKey".to_owned(), vec!["x".to_owned()]);
        at(&mut u, "data")
            .body
            .insert("PublishPort".to_owned(), vec!["80".to_owned()]);

        let warnings = validate(&u).expect("a warning is not a refusal");
        let about: Vec<_> = warnings.iter().map(|w| w.resource.as_str()).collect();
        assert_eq!(about, ["web", "web", "data"]);
        for warning in &warnings {
            assert_eq!(warning.target, NAME);
            assert!(warning.msg.contains("is not a key this plugin knows"));
        }
    }
}
