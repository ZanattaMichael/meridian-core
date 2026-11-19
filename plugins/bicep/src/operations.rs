//! The failed operations of an Azure deployment, from
//! `az deployment operation group|sub list --output json`. ARM reads them
//! the same way.

use meridian_sdk::decode_value;
use meridian_sdk::failure::{Cause, Failure};
use meridian_sdk::Value;

/// One failure per resource whose operation failed. `resource` maps the
/// operation's target (type, name and full id) to the document's id.
pub fn failed_operations(
    json: &str,
    resource: impl Fn(&str, &str, &str) -> String,
) -> Vec<Failure> {
    let Ok(value) = decode_value(json.trim().as_bytes()) else {
        return Vec::new();
    };
    let operations = value.as_list().unwrap_or(&[]);
    let mut failures: Vec<Failure> = Vec::new();
    for operation in operations {
        let Some(properties) = operation
            .as_map()
            .and_then(|o| o.get("properties"))
            .and_then(Value::as_map)
        else {
            continue;
        };
        let text = |map: Option<&std::collections::BTreeMap<String, Value>>, key: &str| {
            map.and_then(|m| m.get(key))
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned()
        };
        if text(Some(properties), "provisioningState") != "Failed" {
            continue;
        }
        let target = properties.get("targetResource").and_then(Value::as_map);
        let error = properties
            .get("statusMessage")
            .and_then(Value::as_map)
            .and_then(|m| m.get("error"))
            .and_then(Value::as_map);
        let code = text(error, "code");
        let message = text(error, "message");
        let status = text(Some(properties), "statusCode");
        let id = resource(
            &text(target, "resourceType"),
            &text(target, "resourceName"),
            &text(target, "id"),
        );
        if failures.iter().any(|f| f.resource == id) {
            continue;
        }
        let cause = match status.as_str() {
            "Unauthorized" | "Forbidden" => Cause::AuthFailed,
            _ if code.contains("AuthorizationFailed") => Cause::AuthFailed,
            _ if code.contains("InvalidTemplate") || code == "BadRequest" => Cause::InvalidConfig,
            _ if code.contains("NotFound") && code != "ResourceNotFound" => {
                Cause::DependencyMissing
            }
            _ => Cause::from_message(&message).unwrap_or(Cause::ResourceFailed),
        };
        let code = if code.is_empty() { status } else { code };
        failures.push(
            Failure::new(cause, id, message.lines().next().unwrap_or_default())
                .code(code)
                .detail(message.clone()),
        );
    }
    failures
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_failed_operation_is_its_resources_failure_with_azures_code() {
        let json = r#"[
            {"properties": {"provisioningState": "Succeeded", "targetResource": {"resourceType": "Microsoft.Network/virtualNetworks", "resourceName": "vnet"}}},
            {"properties": {"provisioningState": "Failed", "statusCode": "Conflict",
                "statusMessage": {"error": {"code": "StorageAccountAlreadyTaken", "message": "The storage account named shoplogs is already taken."}},
                "targetResource": {"id": "/subscriptions/s/resourceGroups/rg/providers/Microsoft.Storage/storageAccounts/shoplogs", "resourceType": "Microsoft.Storage/storageAccounts", "resourceName": "shoplogs"}}},
            {"properties": {"provisioningState": "Failed", "statusCode": "Forbidden",
                "statusMessage": {"error": {"code": "AuthorizationFailed", "message": "The client does not have authorization"}},
                "targetResource": {"resourceType": "Microsoft.Web/sites", "resourceName": "app"}}}
        ]"#;
        let failures = failed_operations(json, |kind, name, _| {
            if name == "shoplogs" {
                "logs".to_owned()
            } else {
                format!("{kind}/{name}")
            }
        });
        let got: Vec<(&str, Cause, &str)> = failures
            .iter()
            .map(|f| (f.resource.as_str(), f.cause, f.tool_code.as_str()))
            .collect();
        assert_eq!(
            got,
            [
                ("logs", Cause::ResourceFailed, "StorageAccountAlreadyTaken"),
                (
                    "Microsoft.Web/sites/app",
                    Cause::AuthFailed,
                    "AuthorizationFailed"
                ),
            ]
        );
        assert_eq!(
            failures[0].summary,
            "The storage account named shoplogs is already taken."
        );
        assert!(failed_operations("ERROR", |_, _, _| String::new()).is_empty());
    }
}
