//! What `state.sls_id ... test=True` said of one state, for `meridian
//! inspect`.

use meridian_sdk::{decode_value, Data, Value};

/// The state functions whose success means the thing is not there.
const REMOVING: &[&str] = &["absent", "removed", "purged", "dead"];

/// Whether the state's thing exists, what Salt said of it, and whether it
/// is as declared, from the JSON result. A state Salt would change is not
/// as declared; one that failed is an error.
pub(crate) fn read(stdout: &str, resource: &str) -> Result<(bool, Data, Option<bool>), String> {
    let document = decode_value(stdout.trim().as_bytes())
        .map_err(|_| "it printed something that is not its JSON result".to_owned())?;
    let body = match &document {
        Value::Map(map) if map.len() == 1 => map.values().next().expect("one entry"),
        other => other,
    };
    let body = match body {
        Value::Map(map) if map.contains_key("return") => &map["return"],
        other => other,
    };
    let state = match body {
        Value::Map(states) => states.iter().find(|(key, value)| {
            let id = value
                .as_map()
                .and_then(|v| v.get("__id__"))
                .and_then(Value::as_str);
            id == Some(resource) || key.split("_|-").nth(1) == Some(resource)
        }),
        Value::List(reasons) => {
            let text: Vec<String> = reasons
                .iter()
                .map(|r| r.as_str().map_or_else(|| r.canonical(), str::to_owned))
                .collect();
            return Err(text.join("; "));
        }
        Value::String(text) => return Err(text.clone()),
        _ => None,
    };
    let Some((key, result)) = state else {
        return Err(format!("it returned no state {resource:?}"));
    };
    let result = result.as_map().cloned().unwrap_or_default();
    let comment = result
        .get("comment")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_owned();
    let function = key.rsplit("_|-").next().unwrap_or("");
    let removing = REMOVING.contains(&function);
    let (found, in_desired_state) = match result.get("result") {
        Some(Value::Bool(true)) => (!removing, Some(true)),
        // `None` in test mode: the state would change.
        Some(Value::Null) | None => (removing, Some(false)),
        _ => {
            return Err(comment
                .lines()
                .next()
                .unwrap_or("the state failed")
                .to_owned())
        }
    };
    let mut actual = Data::new();
    if !comment.is_empty() {
        actual.insert("comment".to_owned(), Value::String(comment));
    }
    if let Some(Value::Map(changes)) = result.get("changes") {
        if !changes.is_empty() {
            actual.insert("wouldChange".to_owned(), Value::Map(changes.clone()));
        }
    }
    Ok((found, actual, in_desired_state))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_state_in_place_is_as_declared_and_one_salt_would_change_is_not() {
        let same = r#"{"local": {"file_|-app_conf_|-/etc/app.conf_|-managed": {"result": true, "comment": "The file /etc/app.conf is in the correct state", "changes": {}, "__id__": "app_conf"}}}"#;
        let (found, actual, ok) = read(same, "app_conf").unwrap();
        assert!(found);
        assert_eq!(ok, Some(true));
        assert_eq!(
            actual.get("comment"),
            Some(&Value::from(
                "The file /etc/app.conf is in the correct state"
            ))
        );

        let would = r#"{"local": {"file_|-app_conf_|-/etc/app.conf_|-managed": {"result": null, "comment": "The file /etc/app.conf is set to be changed", "changes": {"mode": "0644"}}}}"#;
        let (_, actual, ok) = read(would, "app_conf").unwrap();
        assert_eq!(ok, Some(false));
        assert!(actual.contains_key("wouldChange"));
    }

    #[test]
    fn an_absent_state_in_place_is_not_found_and_a_failure_is_an_error() {
        let gone = r#"{"local": {"file_|-old_|-/tmp/old_|-absent": {"result": true, "comment": "File /tmp/old is not present"}}}"#;
        let (found, _, ok) = read(gone, "old").unwrap();
        assert!(!found);
        assert_eq!(ok, Some(true));

        let failed = r#"{"local": {"pkg_|-p_|-p_|-installed": {"result": false, "comment": "No such package"}}}"#;
        assert_eq!(read(failed, "p").unwrap_err(), "No such package");
        let compile = r#"{"local": ["Rendering SLS 'base:web' failed"]}"#;
        assert_eq!(
            read(compile, "p").unwrap_err(),
            "Rendering SLS 'base:web' failed"
        );
    }
}
