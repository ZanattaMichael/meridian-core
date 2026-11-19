//! Reads one resource for `meridian inspect`, shared with OpenTofu, whose
//! `tofu` takes the same commands.
//!
//! The recorded values come from `show -json`. Whether the resource is as
//! declared is asked with a plan of the compiled configuration targeted at
//! it, which refreshes it from the cloud and saves nothing.

use meridian_sdk::tool::{write_artifact, Tool};
use meridian_sdk::{decode_value, Artifact, Data, Inspection, RunContext, RunError, Value};

/// How the runner starts its tool, for [`inspect`].
pub struct Program<'a> {
    /// `terraform` or `tofu`, as the command is shown.
    pub name: &'a str,
    /// The tool, set up for the context and its cloud credential.
    pub tool: &'a dyn Fn() -> Tool,
    /// The operator's extra arguments for `init` and `plan`.
    pub extra_args: &'a [String],
    /// The configuration file the artifact holds.
    pub config_path: &'a str,
}

/// Reads `resource` from the tool's state and asks a targeted plan whether
/// it is as declared. Writes the configuration, as a plan does; never the
/// state, and takes no state lock.
pub fn inspect(
    program: &Program<'_>,
    context: &RunContext,
    artifact: &Artifact,
    resource: &str,
) -> Result<Inspection, RunError> {
    let name = program.name;
    let show = format!("{name} show -json");
    let not_found = |command: String| Inspection {
        found: false,
        in_desired_state: Some(false),
        command,
        ..Inspection::default()
    };
    if !context.work_dir.join(program.config_path).exists() {
        return Ok(not_found(show));
    }
    let log = &meridian_sdk::NoLog;
    if !artifact.files.contains_key(program.config_path) {
        return Err(RunError::new(format!(
            "the artifact has no {}; it was not compiled by this plugin",
            program.config_path
        )));
    }
    write_artifact(&context.work_dir, artifact)?;
    (program.tool)()
        .args(["init", "-input=false", "-no-color"])
        .args(program.extra_args.iter().cloned())
        .run_ok(&format!("{name} init failed"), log)?;
    let shown = (program.tool)()
        .args(["show", "-json", "-no-color"])
        .capture_stdout()
        .run_ok(&format!("{name} could not show its state"), log)?;
    let Some((address, actual)) = read_resource(&shown.stdout, resource)
        .map_err(|e| RunError::new(format!("{name}'s state could not be read: {e}")))?
    else {
        return Ok(not_found(show));
    };
    let check = format!("{name} plan -target={address} -detailed-exitcode");
    let secrets = if context.secrets.is_empty() {
        None
    } else {
        Some(
            context
                .secrets
                .json_file("secrets.auto.tfvars.json", crate::secret_variable)
                .map_err(|e| RunError::new(format!("writing the variables file: {e}")))?,
        )
    };
    let mut plan = (program.tool)()
        .args(["plan", "-input=false", "-no-color", "-lock=false"])
        .args(program.extra_args.iter().cloned())
        .arg(format!("-target={address}"))
        .arg("-detailed-exitcode");
    if let Some(file) = &secrets {
        plan = plan.arg(format!("-var-file={}", file.path().display()));
    }
    let planned = plan.run(log)?;
    drop(secrets);
    let in_desired_state = match planned.code {
        Some(0) => true,
        Some(2) => false,
        _ => return Err(planned.into_error(format!("{name} plan failed"))),
    };
    Ok(Inspection {
        found: true,
        actual,
        in_desired_state: Some(in_desired_state),
        command: format!("{show}; {check}"),
    })
}

/// The managed resource named `name` in the root module of `show -json`,
/// as its address and its values. A value the state marks sensitive is
/// written as [`meridian_sdk::SENSITIVE`]; a null one is left out.
pub fn read_resource(json: &str, name: &str) -> Result<Option<(String, Data)>, String> {
    if json.trim().is_empty() {
        return Ok(None);
    }
    let state = decode_value(json.as_bytes())?;
    let resources = state
        .as_map()
        .and_then(|s| s.get("values"))
        .and_then(Value::as_map)
        .and_then(|v| v.get("root_module"))
        .and_then(Value::as_map)
        .and_then(|m| m.get("resources"))
        .and_then(Value::as_list)
        .unwrap_or(&[]);
    for entry in resources {
        let Some(entry) = entry.as_map() else {
            continue;
        };
        let text = |key: &str| entry.get(key).and_then(Value::as_str).unwrap_or("");
        if text("mode") != "managed" || entry.contains_key("index") || text("name") != name {
            continue;
        }
        let sensitive = entry.get("sensitive_values").and_then(Value::as_map);
        let values = entry
            .get("values")
            .and_then(Value::as_map)
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .filter(|(_, v)| *v != Value::Null)
            .map(|(k, v)| {
                let hidden = sensitive.and_then(|s| s.get(&k)) == Some(&Value::Bool(true));
                (
                    k,
                    if hidden {
                        Value::from(meridian_sdk::SENSITIVE)
                    } else {
                        v
                    },
                )
            })
            .collect();
        return Ok(Some((text("address").to_owned(), values)));
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;

    const STATE: &str = r#"{"values":{"root_module":{"resources":[
      {"address":"data.aws_ami.web","mode":"data","type":"aws_ami","name":"web","values":{"id":"ami-1"}},
      {"address":"aws_instance.web","mode":"managed","type":"aws_instance","name":"web",
       "values":{"id":"i-0abc","instance_type":"t3.micro","user_data":"secret","tags":null},
       "sensitive_values":{"user_data":true}},
      {"address":"aws_subnet.a[0]","mode":"managed","type":"aws_subnet","name":"a","index":0,"values":{}}
    ]}}}"#;

    #[test]
    fn a_resource_is_read_by_its_name_with_sensitive_values_hidden() {
        let (address, values) = read_resource(STATE, "web").unwrap().unwrap();
        assert_eq!(address, "aws_instance.web");
        assert_eq!(values.get("instance_type"), Some(&Value::from("t3.micro")));
        assert_eq!(
            values.get("user_data"),
            Some(&Value::from(meridian_sdk::SENSITIVE))
        );
        assert!(!values.contains_key("tags"));
    }

    #[test]
    fn a_counted_or_missing_resource_is_not_found() {
        assert_eq!(read_resource(STATE, "a").unwrap(), None);
        assert_eq!(read_resource(STATE, "db").unwrap(), None);
        assert_eq!(read_resource("", "web").unwrap(), None);
    }
}
