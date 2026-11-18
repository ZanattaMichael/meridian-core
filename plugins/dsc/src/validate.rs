//! Stage three: the constraints that are the runner's and not Meridian's.

use meridian_sdk::rules::{self, Rule};
use meridian_sdk::CompileError;

use crate::{refuse_at, Configuration, NAME};

pub(crate) fn validate(configuration: &Configuration) -> Result<(), CompileError> {
    let whole = |rule: &Rule, msg: String| CompileError {
        target: NAME.to_owned(),
        rule: rule.name.to_owned(),
        hint: rule.hint.to_owned(),
        msg,
        ..CompileError::default()
    };
    if configuration.instances.is_empty() {
        return Err(whole(
            &rules::EMPTY_DOCUMENT,
            "there is nothing to configure: the document declares no DSC resources".to_owned(),
        ));
    }
    // The node's name is the compiled file's name.
    let node = &configuration.node;
    let safe = !node.is_empty()
        && !node.starts_with('.')
        && node
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.')
        && !node.contains("..");
    if !safe {
        return Err(whole(
            &rules::INVALID_NAME,
            format!(
                "document name {node:?} cannot name a DscPipelineRunner node: it becomes a file \
                 name, so it may hold only letters, digits, hyphens, underscores and periods"
            ),
        ));
    }
    if configuration.settings.target != "Local" && configuration.host.is_empty() {
        return Err(whole(
            &rules::MISSING_PARAM,
            format!(
                "the runner's target is {}, which needs a machine to reach; give the document a \
                 targetHost",
                configuration.settings.target
            ),
        ));
    }
    if configuration.settings.target == "Local" && !configuration.settings.credential.is_empty() {
        return Err(whole(
            &rules::INVALID_PARAM,
            "the runner has a credential but its target is Local, which connects to nothing"
                .to_owned(),
        ));
    }
    for instance in &configuration.instances {
        // The runner reads `Module/Resource/name`, so a slash in the name
        // would name a different resource.
        if instance.id.contains('/') {
            return Err(refuse_at(
                &instance.id,
                &instance.pos,
                &rules::INVALID_NAME,
                "a resource id cannot hold a slash on this target".to_owned(),
            ));
        }
        if instance.pre_condition.contains('\n') {
            return Err(refuse_at(
                &instance.id,
                &instance.pos,
                &rules::INVALID_RUNTIME_CONDITION,
                "runtimeWhen must be one line of PowerShell on this target".to_owned(),
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Instance, Settings};
    use meridian_sdk::Position;

    fn instance(id: &str) -> Instance {
        Instance {
            id: id.to_owned(),
            r#type: "PSDscResources/WindowsFeature".to_owned(),
            properties: Default::default(),
            pre_condition: String::new(),
            reads: Vec::new(),
            depends_on: Vec::new(),
            notify: Vec::new(),
            pos: Position {
                line: 9,
                ..Position::default()
            },
        }
    }

    fn configuration() -> Configuration {
        Configuration {
            node: "web01".to_owned(),
            host: "web01.example.com".to_owned(),
            settings: Settings {
                engine: "DscV3".to_owned(),
                target: "WinRM".to_owned(),
                credential: "deploy".to_owned(),
            },
            instances: vec![instance("iis")],
        }
    }

    #[test]
    fn each_rule_refuses_the_one_mistake_it_is_for() {
        type Break = fn(&mut Configuration);
        let table: &[(&str, &str, Break)] = &[
            ("empty-document", "", |c| c.instances.clear()),
            ("missing-param", "", |c| c.host.clear()),
            ("missing-param", "", |c| {
                c.settings.target = "SSH".to_owned();
                c.host.clear();
            }),
            ("invalid-param", "", |c| {
                c.settings.target = "Local".to_owned()
            }),
            ("invalid-name", "web/iis", |c| {
                c.instances.push(instance("web/iis"));
            }),
            ("invalid-runtime-condition", "iis", |c| {
                c.instances[0].pre_condition = "$true\n$false".to_owned();
            }),
        ];

        assert_eq!(validate(&configuration()), Ok(()));
        for (rule, resource, break_it) in table {
            let mut broken = configuration();
            break_it(&mut broken);
            let err = validate(&broken).expect_err(rule);
            assert_eq!(err.rule, *rule);
            assert_eq!(err.resource, *resource, "{rule}");
            assert_eq!(err.target, NAME, "{rule}");
            assert!(!err.msg.is_empty(), "{rule}");
            if !resource.is_empty() {
                assert_eq!(err.pos.line, 9, "{rule} must point at the resource");
            }
        }
    }

    /// The node's name becomes a file name, so the table is of names.
    #[test]
    fn a_node_name_is_a_safe_file_name() {
        for good in [
            "web01",
            "web-01",
            "web_01",
            "web.example.com",
            "A",
            "0",
            "a.b",
        ] {
            let c = Configuration {
                node: good.to_owned(),
                ..configuration()
            };
            assert_eq!(validate(&c), Ok(()), "{good:?}");
        }
        for bad in [
            "",
            ".",
            "..",
            ".hidden",
            "web..01",
            "a/b",
            "a\\b",
            "web 01",
            "web\n01",
            "wéb",
            "web:01",
            "web*",
            "$(hostname)",
        ] {
            let c = Configuration {
                node: bad.to_owned(),
                ..configuration()
            };
            let err = validate(&c).expect_err(bad);
            assert_eq!(err.rule, "invalid-name", "{bad:?}");
            assert_eq!(err.resource, "", "{bad:?}");
        }
    }

    #[test]
    fn a_local_run_needs_no_host_and_takes_no_credential() {
        let local = |host: &str, credential: &str| Configuration {
            host: host.to_owned(),
            settings: Settings {
                engine: "Auto".to_owned(),
                target: "Local".to_owned(),
                credential: credential.to_owned(),
            },
            ..configuration()
        };
        assert_eq!(validate(&local("", "")), Ok(()));
        assert_eq!(validate(&local("web01", "")), Ok(()));
        assert_eq!(
            validate(&local("", "deploy")).unwrap_err().rule,
            "invalid-param"
        );
        // A remote target may go without a credential.
        let mut remote = configuration();
        remote.settings.credential.clear();
        assert_eq!(validate(&remote), Ok(()));
    }

    #[test]
    fn a_one_line_condition_is_kept_whatever_it_holds() {
        for good in ["$true", "(Get-Service w3svc).Status -eq 'Running'", "a; b"] {
            let mut c = configuration();
            c.instances[0].pre_condition = good.to_owned();
            assert_eq!(validate(&c), Ok(()), "{good}");
        }
    }
}
