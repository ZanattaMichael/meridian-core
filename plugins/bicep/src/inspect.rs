//! Reads one Azure resource for `meridian inspect` with `az resource show`,
//! shared with the arm target. It reads and changes nothing.

use meridian_sdk::tool::Tool;
use meridian_sdk::{decode_value, Data, Inspection, RunError, Value};

/// The properties of a resource `az resource show` prints that a template
/// sets; the id, type and name are what was asked.
const KEPT: &[&str] = &["location", "tags", "sku", "kind", "identity", "properties"];

/// The arguments that name a resource of `type` (`namespace/type[/child...]`)
/// called `name` (`name[/child...]`) to `az resource show`; `None` when the
/// two do not fit together or the name is an expression.
pub fn show_args(r#type: &str, name: &str) -> Option<Vec<String>> {
    let (namespace, types) = r#type.split_once('/')?;
    let types: Vec<&str> = types.split('/').collect();
    let names: Vec<&str> = name.split('/').collect();
    if types.len() != names.len() || name.contains(['[', ']', '$', '(']) || name.is_empty() {
        return None;
    }
    let last = types.len() - 1;
    if last == 0 {
        return Some(vec![
            "--resource-type".into(),
            r#type.into(),
            "--name".into(),
            name.into(),
        ]);
    }
    let parent: Vec<String> = types[..last]
        .iter()
        .zip(&names[..last])
        .map(|(t, n)| format!("{t}/{n}"))
        .collect();
    Some(vec![
        "--namespace".into(),
        namespace.into(),
        "--parent".into(),
        parent.join("/"),
        "--resource-type".into(),
        types[last].into(),
        "--name".into(),
        names[last].into(),
    ])
}

/// Runs `az resource show` as `tool` is set up (program, group,
/// subscription) with the arguments `args` names the resource by, then the
/// operator's `extra_args`, and reads what it prints.
pub fn show(
    tool: Tool,
    args: &[String],
    extra_args: &[String],
    shown_as: String,
) -> Result<Inspection, RunError> {
    let finished = tool
        .args(args.iter().cloned())
        .args(extra_args.iter().cloned())
        .args(["--output", "json"])
        .capture_stdout()
        .run(&meridian_sdk::NoLog)?;
    if !finished.success() {
        let said = format!("{}{}", finished.stderr, finished.stdout);
        if said.contains("ResourceNotFound") || said.contains("ResourceGroupNotFound") {
            return Ok(Inspection {
                found: false,
                in_desired_state: Some(false),
                command: shown_as,
                ..Inspection::default()
            });
        }
        return Err(finished.into_error("az could not show the resource"));
    }
    let read = decode_value(finished.stdout.trim().as_bytes())
        .map_err(|e| RunError::new(format!("the resource could not be read: {e}")))?;
    Ok(Inspection {
        found: true,
        actual: kept(&read),
        in_desired_state: None,
        command: shown_as,
    })
}

fn kept(read: &Value) -> Data {
    read.as_map()
        .map(|m| {
            m.iter()
                .filter(|(k, v)| KEPT.contains(&k.as_str()) && **v != Value::Null)
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect()
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_top_level_resource_is_named_by_its_type_and_name() {
        assert_eq!(
            show_args("microsoft.storage/storageaccounts", "logs1").unwrap(),
            [
                "--resource-type",
                "microsoft.storage/storageaccounts",
                "--name",
                "logs1"
            ]
        );
    }

    #[test]
    fn a_child_resource_is_named_under_its_parent() {
        assert_eq!(
            show_args("microsoft.sql/servers/databases", "srv/db").unwrap(),
            [
                "--namespace",
                "microsoft.sql",
                "--parent",
                "servers/srv",
                "--resource-type",
                "databases",
                "--name",
                "db"
            ]
        );
    }

    #[test]
    fn a_name_that_is_an_expression_cannot_be_shown() {
        assert_eq!(show_args("microsoft.web/sites", "[parameters('n')]"), None);
        assert_eq!(show_args("microsoft.sql/servers/databases", "db"), None);
    }

    #[test]
    fn only_what_a_template_sets_is_kept() {
        let read = decode_value(
            br#"{"id":"/subscriptions/x","name":"logs1","type":"Microsoft.Storage/storageAccounts","location":"westeurope","tags":null,"sku":{"name":"Standard_LRS"},"properties":{"minimumTlsVersion":"TLS1_2"}}"#,
        )
        .unwrap();
        let kept = kept(&read);
        assert_eq!(
            kept.keys().map(String::as_str).collect::<Vec<_>>(),
            ["location", "properties", "sku"]
        );
    }
}
