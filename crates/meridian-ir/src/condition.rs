//! The condition stage: `when` expressions, and what happens to the edges of
//! a resource one of them removes.
//!
//! A `when` is decided by Meridian before anything is compiled, so a resource
//! is either in the compiled output or it is not, and no target ever sees a
//! conditional. The language is Meridian's one expression language, read and
//! decided by `meridian-expr`; this module says what a condition may read
//! (the data given to the command) and what a false one does to a document.
//!
//! Nothing gathers facts from a node yet, so `facts.*` is refused when the
//! condition is read: a condition that could not be decided would be worse
//! than one that cannot be written.

use std::collections::{BTreeMap, HashSet};

use meridian_expr::{Expr, Scope};
use meridian_value::Value;

use crate::error::{Error, Position};
use crate::resolve::{self, Removed};
use crate::types::{IfMissing, Resource};

/// The data a condition is decided against: the same map every target is
/// handed when it compiles.
pub type Data = BTreeMap<String, Value>;

/// A parsed `when` expression.
#[derive(Clone, Debug, PartialEq)]
pub struct Condition(Expr);

impl Condition {
    /// Reads a `when` expression. Nothing is evaluated: this is the check
    /// `meridian validate` makes.
    pub fn parse(text: &str) -> Result<Condition, String> {
        let expr = resolve::condition(text)?;
        expr.check(&resolve::names())?;
        Ok(Condition(expr))
    }

    /// Decides the condition against `data`. Anything that is not plainly
    /// true or false is an error, never a guess.
    pub fn evaluate(&self, data: &Data) -> Result<bool, String> {
        Ok(self.0.truth(&resolve::scope(data))?)
    }
}

/// Decides every `when` in a document's resources and returns the ones that
/// remain, unconditional, in their declared order.
///
/// An edge that pointed at a removed resource follows its `ifMissing`: `skip`
/// drops the edge, and `error` (the default) refuses the document, because a
/// resource that depends on something that is not there is far more often a
/// mistake than an intention. Edges *from* a removed resource go with it.
pub fn prune(resources: &[Resource], data: &Data) -> Result<Vec<Resource>, Error> {
    prune_in(resources, &resolve::scope(data), &mut Vec::new())
}

/// [`prune`], in a scope that may also hold the document's variables and
/// named conditions, recording what each false condition removed. A `when`
/// on an edge is decided here too: a false one drops the entry and leaves
/// both resources.
pub(crate) fn prune_in(
    resources: &[Resource],
    scope: &Scope,
    record: &mut Vec<Removed>,
) -> Result<Vec<Resource>, Error> {
    let mut removed: HashSet<&str> = HashSet::new();
    for (i, r) in resources.iter().enumerate() {
        if r.when.is_empty() {
            continue;
        }
        let keep = resolve::condition(&r.when)
            .and_then(|condition| Ok(condition.truth(scope)?))
            .map_err(|why| {
                Error::new(
                    r.pos.clone(),
                    format!("spec.resources[{i}].when"),
                    format!("resource {:?}: {why}", r.id),
                )
            })?;
        if !keep {
            removed.insert(&r.id);
            record.push(Removed {
                subject: format!("resource {:?}", r.id),
                condition: r.when.clone(),
            });
        }
    }

    let mut kept = Vec::with_capacity(resources.len() - removed.len());
    for (i, r) in resources.iter().enumerate() {
        if removed.contains(r.id.as_str()) {
            continue;
        }
        let mut r = r.clone();
        r.when = String::new();

        let dangling = |verb: &str, field: &str, j: usize, missing: &str, pos| {
            Error::new(
                pos,
                format!("spec.resources[{i}].{field}[{j}].resource"),
                format!(
                    "resource {:?} {verb} {missing:?}, which its `when` removed from this \
                     run; set ifMissing: skip on this entry if the resource should carry \
                     on without it",
                    r.id
                ),
            )
        };
        // An edge's own condition comes first: an entry it drops is gone
        // whatever became of the resource it named.
        let mut dropped_deps: HashSet<usize> = HashSet::new();
        let mut dropped_notifies: HashSet<usize> = HashSet::new();
        let edge = |field: &str, verb: &str, j: usize, when: &str, to: &str, pos: &Position| {
            resolve::condition(when)
                .and_then(|condition| Ok(condition.truth(scope)?))
                .map_err(|why| {
                    Error::new(
                        pos.clone(),
                        format!("spec.resources[{i}].{field}[{j}].when"),
                        format!("resource {:?} {verb} {to:?}: {why}", r.id),
                    )
                })
        };
        for (j, dep) in r.depends_on.iter().enumerate() {
            if !dep.when.is_empty()
                && !edge(
                    "dependsOn",
                    "depends on",
                    j,
                    &dep.when,
                    &dep.resource,
                    &dep.pos,
                )?
            {
                dropped_deps.insert(j);
                record.push(Removed {
                    subject: format!("{:?} depends on {:?}", r.id, dep.resource),
                    condition: dep.when.clone(),
                });
            }
        }
        for (j, n) in r.notifies.iter().enumerate() {
            if !n.when.is_empty() && !edge("notifies", "notifies", j, &n.when, &n.resource, &n.pos)?
            {
                dropped_notifies.insert(j);
                record.push(Removed {
                    subject: format!("{:?} notifies {:?}", r.id, n.resource),
                    condition: n.when.clone(),
                });
            }
        }

        for (j, dep) in r.depends_on.iter().enumerate() {
            if dropped_deps.contains(&j) {
                continue;
            }
            if removed.contains(dep.resource.as_str()) && dep.if_missing != IfMissing::Skip {
                return Err(dangling(
                    "depends on",
                    "dependsOn",
                    j,
                    &dep.resource,
                    dep.pos.clone(),
                ));
            }
        }
        for (j, n) in r.notifies.iter().enumerate() {
            if dropped_notifies.contains(&j) {
                continue;
            }
            if removed.contains(n.resource.as_str()) && n.if_missing != IfMissing::Skip {
                return Err(dangling(
                    "notifies",
                    "notifies",
                    j,
                    &n.resource,
                    n.pos.clone(),
                ));
            }
        }
        let mut j = 0;
        r.depends_on.retain_mut(|dep| {
            let keep = !dropped_deps.contains(&j) && !removed.contains(dep.resource.as_str());
            dep.when.clear();
            j += 1;
            keep
        });
        let mut j = 0;
        r.notifies.retain_mut(|n| {
            let keep = !dropped_notifies.contains(&j) && !removed.contains(n.resource.as_str());
            n.when.clear();
            j += 1;
            keep
        });
        kept.push(r);
    }
    Ok(kept)
}
