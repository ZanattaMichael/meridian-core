//! Stage two: cloud-init's ordering semantics.
//!
//! cloud-init does not apply a document from top to bottom. Each module runs
//! at a fixed point of the boot, whatever order the document lists things in:
//!
//! 1. `write_files`
//! 2. `groups`, then `users`
//! 3. `apt` sources and `yum_repos`
//! 4. `packages`
//! 5. `write_files` entries marked `defer`
//! 6. `runcmd`, top to bottom
//!
//! This stage places every resource at one of those points and checks each
//! `dependsOn` against the result. The only choice it has is when a file is
//! written. A file that depends on a user, a group or a package, or is owned
//! by a user or group the document creates, is deferred so that what it needs
//! exists by then. Any other dependency that points backwards through the
//! list above cannot be honoured and is refused.

use std::collections::{HashMap, HashSet};

use meridian_sdk::rules::{self, Rule};
use meridian_sdk::{CompileError, EdgeKind, ResourceGraph};

use crate::{own, CloudConfig, Command, Item, Placed, NAME};

/// The points of a first boot at which this target has work done, in order.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Phase {
    Files,
    Groups,
    Users,
    Repositories,
    Packages,
    DeferredFiles,
    Commands,
}

impl Phase {
    /// What cloud-init does at this point, to finish "cloud-init ... before
    /// it ...".
    fn does(self) -> &'static str {
        match self {
            Phase::Files => "writes files",
            Phase::Groups => "creates groups",
            Phase::Users => "creates users",
            Phase::Repositories => "adds package repositories",
            Phase::Packages => "installs packages",
            Phase::DeferredFiles => "writes the files that wait for users and packages",
            Phase::Commands => "runs commands",
        }
    }

    /// Whether entries at this point are applied in the order listed, so the
    /// order within it has to honour a dependency too. Packages are installed
    /// in one transaction, and groups and users are created as a set.
    fn is_sequence(self) -> bool {
        matches!(self, Phase::Files | Phase::DeferredFiles | Phase::Commands)
    }
}

/// Places every resource and assembles the document cloud-init will read.
/// `handlers` run after every other command.
pub(crate) fn arrange(
    graph: &ResourceGraph,
    mut units: Vec<Placed<Item>>,
    handlers: Vec<Command>,
) -> Result<CloudConfig, CompileError> {
    let created_users: HashSet<&str> = units
        .iter()
        .filter_map(|u| match &u.item {
            Item::User(user) => Some(user.name.as_str()),
            _ => None,
        })
        .collect();
    let created_groups: HashSet<&str> = units
        .iter()
        .filter_map(|u| match &u.item {
            Item::Group(group) => Some(group.name.as_str()),
            _ => None,
        })
        .collect();

    let mut dependencies: HashMap<&str, Vec<&str>> = HashMap::new();
    for edge in graph.edges_of_kind(EdgeKind::HardOrder) {
        dependencies
            .entry(edge.to.as_str())
            .or_default()
            .push(edge.from.as_str());
    }

    // Resources arrive in apply order, so everything a file depends on has
    // been placed by the time the file is reached. A module of cloud-init's
    // own is applied at whatever point cloud-init gives it, which this
    // target does not know, so it is placed nowhere: an edge at either end
    // of one is refused below.
    let mut phases: HashMap<String, Phase> = HashMap::new();
    let mut deferred = Vec::new();
    for (index, unit) in units.iter().enumerate() {
        let phase = match &unit.item {
            Item::Module(_) => continue,
            Item::Group(_) => Phase::Groups,
            Item::User(_) => Phase::Users,
            Item::Package(_) => Phase::Packages,
            Item::Repository(_) => Phase::Repositories,
            Item::Commands(_) => Phase::Commands,
            Item::File(file) => {
                let waits_for_owner = created_users.contains(file.owner.as_str())
                    || created_groups.contains(file.group.as_str());
                let waits_for_dependency = dependencies
                    .get(unit.id.as_str())
                    .into_iter()
                    .flatten()
                    .any(|dependency| {
                        matches!(
                            phases.get(*dependency),
                            Some(
                                Phase::Groups
                                    | Phase::Users
                                    | Phase::Repositories
                                    | Phase::Packages
                                    | Phase::DeferredFiles
                            )
                        )
                    });
                if waits_for_owner || waits_for_dependency {
                    deferred.push(index);
                    Phase::DeferredFiles
                } else {
                    Phase::Files
                }
            }
        };
        phases.insert(unit.id.clone(), phase);
    }
    for index in deferred {
        if let Item::File(file) = &mut units[index].item {
            file.defer = true;
        }
    }

    check_edges(graph, &units, &phases)?;

    let mut config = CloudConfig {
        document: graph.name.clone(),
        host: graph.host.clone(),
        commands: Vec::new(),
        ..CloudConfig::default()
    };
    let mut late_files = Vec::new();
    for Placed { id, pos, item } in units {
        match item {
            Item::File(item) if item.defer => late_files.push(Placed { id, pos, item }),
            Item::File(item) => config.files.push(Placed { id, pos, item }),
            Item::Group(item) => config.groups.push(Placed { id, pos, item }),
            Item::User(item) => config.users.push(Placed { id, pos, item }),
            Item::Package(item) => config.packages.push(Placed { id, pos, item }),
            Item::Repository(item) => config.repositories.push(Placed { id, pos, item }),
            Item::Commands(commands) => config.commands.extend(commands),
            Item::Module(item) => config.modules.push(Placed { id, pos, item }),
        }
    }
    // One list holds both kinds of file. The deferred ones go last so the
    // list reads in the order it is applied.
    config.files.extend(late_files);
    config.commands.extend(handlers);
    Ok(config)
}

/// Confirms the placement honours every `dependsOn`.
fn check_edges(
    graph: &ResourceGraph,
    units: &[Placed<Item>],
    phases: &HashMap<String, Phase>,
) -> Result<(), CompileError> {
    let position: HashMap<&str, usize> = units
        .iter()
        .enumerate()
        .map(|(i, u)| (u.id.as_str(), i))
        .collect();

    for edge in graph.edges_of_kind(EdgeKind::HardOrder) {
        // A hard-order edge points from the dependency to the resource that
        // declared it, so `from` must be applied before `to`.
        let refuse = |rule: &Rule, msg: String| CompileError {
            target: NAME.to_owned(),
            resource: edge.to.clone(),
            rule: rule.name.to_owned(),
            hint: rule.hint.to_owned(),
            msg,
            pos: edge.pos.clone(),
        };
        let (Some(first), Some(then)) = (phases.get(&edge.from), phases.get(&edge.to)) else {
            let module = units
                .iter()
                .find(|u| {
                    matches!(u.item, Item::Module(_)) && (u.id == edge.from || u.id == edge.to)
                })
                .map(|u| u.id.as_str())
                .unwrap_or_default();
            if module.is_empty() {
                continue;
            }
            return Err(refuse(
                &own::UNSATISFIABLE_ORDER,
                format!(
                    "depends on {:?}, but resource {module:?} is a cloud-config module, which \
                     cloud-init applies at its own point of the first boot: a document cannot \
                     order anything before or after it",
                    edge.from
                ),
            ));
        };
        if first > then {
            return Err(refuse(
                &own::UNSATISFIABLE_ORDER,
                format!(
                    "depends on {:?}, but cloud-init {} before it {}: \
                     the order of a first boot is cloud-init's own and a document cannot change it",
                    edge.from,
                    then.does(),
                    first.does()
                ),
            ));
        }
        // The core hands over an order that satisfies every edge. A target
        // that trusts an invariant it never checks fails silently the day
        // the invariant breaks.
        if first == then
            && first.is_sequence()
            && position[edge.from.as_str()] > position[edge.to.as_str()]
        {
            return Err(refuse(
                &rules::ORDERING_VIOLATED,
                format!(
                    "resource {:?} is ordered before its dependency {:?}; \
                     the graph stage produced an order this target cannot keep",
                    edge.to, edge.from
                ),
            ));
        }
    }
    Ok(())
}
