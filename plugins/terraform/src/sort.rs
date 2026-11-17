//! Stage two: Terraform's ordering semantics.

use meridian_sdk::{CompileError, EdgeKind, ResourceGraph, Value, Warning};

use crate::{refuse_at, Config, Kind, NAME};

/// Turns dependency edges into `depends_on` and notify edges into
/// `replace_triggered_by`.
///
/// Terraform builds its own graph from the references between blocks, so an
/// edge that a reference already implies is left out: the configuration then
/// reads the way a Terraform author would have written it. Only an order the
/// arguments do not show needs saying.
pub(crate) fn apply_ordering(
    graph: &ResourceGraph,
    config: &mut Config,
) -> Result<Vec<Warning>, CompileError> {
    let mut warnings = Vec::new();

    for edge in &graph.edges {
        let (Some(from), Some(to)) = (config.block(&edge.from), config.block(&edge.to)) else {
            let missing = if config.block(&edge.from).is_none() {
                &edge.from
            } else {
                &edge.to
            };
            return Err(refuse_at(
                &edge.to,
                &edge.pos,
                "unknown-dependency",
                format!("an edge names resource {missing:?}, which this document does not declare"),
            ));
        };
        if !from.kind.orderable() || !to.kind.orderable() {
            let settings = if from.kind.orderable() { to } else { from };
            return Err(refuse_at(
                &edge.to,
                &edge.pos,
                "unsupported-dependency",
                format!(
                    "{:?} is {}, which Terraform works out before or alongside everything \
                     else; nothing can depend on it or be ordered by it",
                    settings.name,
                    settings.kind.described()
                ),
            ));
        }
        let from_address = from
            .address()
            .expect("a block that can be ordered has an address");

        match edge.kind {
            EdgeKind::HardOrder => {
                if to.refs.contains(&edge.from) {
                    continue;
                }
                let index = config
                    .blocks
                    .iter()
                    .position(|b| b.name == edge.to)
                    .expect("the block was just found");
                push_unique(
                    &mut config.blocks[index].body,
                    "depends_on",
                    from_address,
                    &edge.to,
                    &edge.pos,
                )?;
            }
            EdgeKind::Notify => {
                if edge.action != "replace" {
                    return Err(refuse_at(
                        &edge.from,
                        &edge.pos,
                        "unsupported-action",
                        format!(
                            "notifies resource {:?} with action {:?}, which this target cannot \
                             express: the only thing Terraform can do to a resource when another \
                             changes is replace it (supported: replace)",
                            edge.to, edge.action
                        ),
                    ));
                }
                if from.kind != Kind::Managed || to.kind != Kind::Managed {
                    return Err(refuse_at(
                        &edge.from,
                        &edge.pos,
                        "unsupported-action",
                        format!(
                            "notifies resource {:?}: only a managed resource can be replaced \
                             when another managed resource changes",
                            edge.to
                        ),
                    ));
                }
                warnings.push(Warning {
                    target: NAME.to_owned(),
                    resource: edge.to.clone(),
                    msg: format!(
                        "is destroyed and created again whenever {:?} changes; Terraform has \
                         no gentler way to react to a change (replace_triggered_by)",
                        edge.from
                    ),
                });
                let index = config
                    .blocks
                    .iter()
                    .position(|b| b.name == edge.to)
                    .expect("the block was just found");
                let lifecycle = config.blocks[index]
                    .body
                    .entry("lifecycle".to_owned())
                    .or_insert_with(|| Value::Map(Default::default()));
                let Value::Map(lifecycle) = lifecycle else {
                    return Err(refuse_at(
                        &edge.to,
                        &edge.pos,
                        "invalid-param",
                        "param \"lifecycle\" must be a map".to_owned(),
                    ));
                };
                push_unique(
                    lifecycle,
                    "replace_triggered_by",
                    from_address,
                    &edge.to,
                    &edge.pos,
                )?;
            }
        }
    }
    Ok(warnings)
}

/// Appends an address to a list-valued argument, creating it if needed.
fn push_unique(
    body: &mut std::collections::BTreeMap<String, Value>,
    key: &str,
    address: String,
    resource: &str,
    pos: &meridian_sdk::Position,
) -> Result<(), CompileError> {
    let list = body
        .entry(key.to_owned())
        .or_insert_with(|| Value::List(Vec::new()));
    let Value::List(items) = list else {
        return Err(refuse_at(
            resource,
            pos,
            "invalid-param",
            format!("param {key:?} must be a list"),
        ));
    };
    let address = Value::String(address);
    if !items.contains(&address) {
        items.push(address);
    }
    Ok(())
}
