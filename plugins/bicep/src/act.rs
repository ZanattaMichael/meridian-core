//! Meridian-run notifies on Azure (decision 0056), shared with the arm
//! target: `az resource invoke-action`, one generic call for every type
//! with an action.

use meridian_sdk::tool::Tool;
use meridian_sdk::{ActSchema, Acted, Log, RunError};

/// Each Azure resource type and the actions Meridian performs on it, by the
/// name of the type's POST action.
pub const ACTIONS: &[(&str, &[&str])] = &[
    ("Microsoft.Web/sites", &["restart", "start", "stop"]),
    ("Microsoft.Web/sites/slots", &["restart", "start", "stop"]),
    (
        "Microsoft.Compute/virtualMachines",
        &["restart", "start", "deallocate", "powerOff"],
    ),
    (
        "Microsoft.ContainerInstance/containerGroups",
        &["restart", "start", "stop"],
    ),
];

/// The shared types that compile to one of [`ACTIONS`]' types.
const SHARED: &[(&str, &str)] = &[("vm", "Microsoft.Compute/virtualMachines")];

/// What a target acts on: each type of [`ACTIONS`] as its documents write
/// it, `suffix` after it (`@*` where a type carries its API version), and
/// the shared types that compile to one.
pub fn acts(suffix: &str) -> Vec<ActSchema> {
    let mut acts: Vec<ActSchema> = ACTIONS
        .iter()
        .map(|(ty, actions)| ActSchema::new(&format!("{ty}{suffix}"), actions))
        .collect();
    for (shared, native) in SHARED {
        if let Some((_, actions)) = ACTIONS.iter().find(|(ty, _)| ty == native) {
            acts.push(ActSchema::new(shared, actions));
        }
    }
    acts
}

/// Runs `az resource invoke-action --action <action>` as `tool` is set up
/// (program, group, subscription), naming the resource with `args` (from
/// [`crate::inspect::show_args`]), then the operator's `extra_args`.
pub fn invoke(
    tool: Tool,
    args: &[String],
    action: &str,
    extra_args: &[String],
    shown_as: String,
    log: &dyn Log,
) -> Result<Acted, RunError> {
    let what = format!("az could not {action} the resource");
    tool.args(["--action", action])
        .args(args.iter().cloned())
        .args(extra_args.iter().cloned())
        .capture_stdout()
        .run_ok(&what, log)
        .map_err(|mut e| {
            e.command = shown_as.clone();
            e
        })?;
    Ok(Acted { command: shown_as })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_type_with_its_api_version_and_the_shared_vm_are_covered() {
        let bicep = acts("@*");
        assert!(meridian_sdk::notify::performs(
            &bicep,
            "Microsoft.Web/sites@2023-01-01",
            "restart"
        ));
        assert!(meridian_sdk::notify::performs(&bicep, "vm", "deallocate"));
        assert!(!meridian_sdk::notify::performs(
            &bicep,
            "Microsoft.Web/sites@2023-01-01",
            "reload"
        ));
        let arm = acts("");
        assert!(meridian_sdk::notify::performs(
            &arm,
            "Microsoft.ContainerInstance/containerGroups",
            "restart"
        ));
        assert!(!meridian_sdk::notify::performs(
            &arm,
            "Microsoft.Web/sites@2023-01-01",
            "restart"
        ));
    }
}
