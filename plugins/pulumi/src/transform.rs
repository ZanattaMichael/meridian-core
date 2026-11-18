//! Stage one: each resource becomes an entry of the program, and every
//! reference between resources is checked and written the way Pulumi reads it.

use std::collections::{BTreeMap, BTreeSet};

use meridian_sdk::expr::{self, Part};
use meridian_sdk::rules;
use meridian_sdk::{CompileError, Resource, ResourceGraph, Value};

use crate::{
    refuse, refuse_at, secret_key, Entry, Kind, Program, Settings, INVOKE_OPTIONS, RESOURCE_OPTIONS,
};

/// The one name Pulumi YAML itself gives meaning to at the start of a
/// reference: `${pulumi.stack}`, `${pulumi.project}`, `${pulumi.cwd}`.
pub(crate) const PULUMI_NAMESPACE: &str = "pulumi";

/// The keys a configuration value's declaration may hold.
const CONFIG_KEYS: &[&str] = &["default", "description", "items", "secret", "type", "value"];

/// The settings a `stack` resource may hold.
const STACK_KEYS: &[&str] = &["backend", "name", "secretsProvider"];

/// What a resource type names.
enum Class {
    Stack,
    Entry(Kind, String),
}

fn classify(r: &Resource) -> Result<Class, CompileError> {
    let t = r.r#type.as_str();
    Ok(match t {
        "stack" => Class::Stack,
        "config" => Class::Entry(Kind::Config, String::new()),
        _ => {
            if let Some(function) = t.strip_prefix("invoke.") {
                if is_token(function) {
                    return Ok(Class::Entry(Kind::Invoke, function.to_owned()));
                }
            } else if is_token(t) {
                return Ok(Class::Entry(Kind::Managed, t.to_owned()));
            }
            return Err(refuse(
                r,
                &rules::UNSUPPORTED_RESOURCE_TYPE,
                format!(
                    "{t:?} is not a Pulumi type. Use a provider's own type token, such as \
                     aws:ec2:Instance, or one of: config, stack, invoke.<function>"
                ),
            ));
        }
    })
}

/// A Pulumi type or function token: `<package>:<module>:<Name>`, or
/// `<package>:<Name>` for a package's index module. The package is lower-case
/// words joined by hyphens; the module may hold a path, as in
/// `aws:s3/bucket:Bucket`.
fn is_token(t: &str) -> bool {
    let parts: Vec<&str> = t.split(':').collect();
    if parts.len() != 2 && parts.len() != 3 {
        return false;
    }
    let package = parts[0];
    let package_ok = package.starts_with(|c: char| c.is_ascii_lowercase())
        && package
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-');
    let rest_ok = parts[1..].iter().all(|part| {
        !part.is_empty()
            && part
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || "/._-".contains(c))
    });
    package_ok && rest_ok
}

/// How each resource of the document is read from another: its name in the
/// program, or why it cannot be read.
enum Address {
    Name(String),
    /// A secret's configuration value: read by name like any other, but not
    /// a resource to order after.
    Secret(String),
    /// A provider setting, read by the provider alone.
    ProviderSetting(String),
    Stack,
}

type Addresses = BTreeMap<String, Address>;

/// A param that must be a string, when it is there at all.
fn string_param(r: &Resource, key: &str) -> Result<Option<String>, CompileError> {
    match r.params.get(key) {
        None => Ok(None),
        Some(Value::String(s)) => Ok(Some(s.clone())),
        Some(other) => Err(refuse(
            r,
            &rules::INVALID_PARAM,
            format!("param {key:?} must be a string, not {}", other.describe()),
        )),
    }
}

/// The name a configuration value has in the program.
fn config_name(r: &Resource) -> Result<String, CompileError> {
    Ok(string_param(r, "name")?.unwrap_or_else(|| r.id.clone()))
}

/// Maps the tree onto a program. It does no ordering work.
pub(crate) fn transform(graph: &ResourceGraph) -> Result<Program, CompileError> {
    let mut classes = Vec::with_capacity(graph.resources.len());
    let mut addresses = Addresses::new();
    for r in &graph.resources {
        let class = classify(r)?;
        let address = match &class {
            Class::Stack => Address::Stack,
            Class::Entry(Kind::Config, _) => {
                let name = config_name(r)?;
                if name.contains(':') {
                    Address::ProviderSetting(name)
                } else {
                    Address::Name(name)
                }
            }
            Class::Entry(..) => Address::Name(r.id.clone()),
        };
        addresses.insert(r.id.clone(), address);
        classes.push(class);
    }

    let mut program = Program {
        project: graph.name.clone(),
        ..Program::default()
    };
    // Each secret is a configuration value declared secret, so Pulumi keeps
    // it encrypted in the stack's state and prints `[secret]` in its place.
    for secret in &graph.secrets {
        let key = secret_key(&secret.name);
        addresses.insert(key.clone(), Address::Secret(key.clone()));
        program.entries.push(Entry {
            kind: Kind::Config,
            id: key.clone(),
            name: key,
            token: String::new(),
            body: BTreeMap::from([
                ("type".to_owned(), Value::from("string")),
                ("secret".to_owned(), Value::Bool(true)),
            ]),
            options: BTreeMap::new(),
            refs: BTreeSet::new(),
            pos: meridian_sdk::Position::default(),
        });
    }
    for (r, class) in graph.resources.iter().zip(classes) {
        check_meta(r)?;
        match class {
            Class::Stack => stack(r, &mut program.settings)?,
            Class::Entry(kind, token) => {
                program.entries.push(entry(r, kind, token, &addresses)?);
            }
        }
    }

    for output in &graph.outputs {
        let mut refs = BTreeSet::new();
        let value = rewrite(&Value::String(output.value.clone()), &addresses, &mut refs).map_err(
            |msg| {
                refuse_at(
                    "",
                    &output.pos,
                    &rules::INVALID_REFERENCE,
                    format!("output {:?}: {msg}", output.name),
                )
            },
        )?;
        program
            .outputs
            .push((output.name.clone(), value, output.pos.clone()));
    }
    Ok(program)
}

/// Reads the one `stack` resource into the deployment's settings.
fn stack(r: &Resource, settings: &mut Settings) -> Result<(), CompileError> {
    if let Some((first, _)) = &settings.declared {
        return Err(refuse(
            r,
            &rules::DUPLICATE_RESOURCE,
            format!(
                "a document deploys to one stack; {first:?} already holds its settings, so \
                 put these there"
            ),
        ));
    }
    if let Some(unknown) = r.params.keys().find(|k| !STACK_KEYS.contains(&k.as_str())) {
        return Err(refuse(
            r,
            &rules::INVALID_PARAM,
            format!(
                "a stack has no setting {unknown:?} (settings: {})",
                STACK_KEYS.join(", ")
            ),
        ));
    }
    for key in STACK_KEYS {
        if let Some(Value::String(text)) = r.params.get(*key) {
            if expr::has_expr(text) || expr::parse(text).is_err() {
                return Err(refuse(
                    r,
                    &rules::INVALID_PARAM,
                    format!(
                        "setting {key:?} must be plain text: it is needed before Pulumi runs, \
                         when no resource has a value yet"
                    ),
                ));
            }
        }
    }
    if let Some(name) = string_param(r, "name")? {
        settings.stack = name;
    }
    settings.backend = string_param(r, "backend")?.unwrap_or_default();
    settings.encryption = string_param(r, "secretsProvider")?.unwrap_or_default();
    settings.declared = Some((r.id.clone(), r.pos.clone()));
    Ok(())
}

/// One resource, function call or configuration value.
fn entry(
    r: &Resource,
    kind: Kind,
    token: String,
    addresses: &Addresses,
) -> Result<Entry, CompileError> {
    let options_key = match kind {
        Kind::Managed => Some(RESOURCE_OPTIONS),
        Kind::Invoke => Some(INVOKE_OPTIONS),
        Kind::Config => None,
    };
    let name = if kind == Kind::Config {
        config_name(r)?
    } else {
        r.id.clone()
    };

    let mut refs = BTreeSet::new();
    let mut body = BTreeMap::new();
    let mut options = BTreeMap::new();
    if kind == Kind::Config {
        // A declaration is read before the program runs, so nothing in it is
        // interpolated: it is kept exactly as written.
        body = r.params.clone();
        body.remove("name");
        config(r, &name, &body)?;
    } else {
        for (key, value) in &r.params {
            let rewritten = rewrite(value, addresses, &mut refs).map_err(|msg| {
                refuse(
                    r,
                    &rules::INVALID_REFERENCE,
                    format!("param {key:?}: {msg}"),
                )
            })?;
            if Some(key.as_str()) == options_key {
                let Value::Map(map) = rewritten else {
                    return Err(refuse(
                        r,
                        &rules::INVALID_PARAM,
                        format!(
                            "param {key:?} must be a map of Pulumi's options, not {}",
                            value.describe()
                        ),
                    ));
                };
                options = map;
            } else {
                body.insert(key.clone(), rewritten);
            }
        }
        refs.remove(&r.id);
    }

    Ok(Entry {
        kind,
        id: r.id.clone(),
        name,
        token,
        body,
        options,
        refs,
        pos: r.pos.clone(),
    })
}

/// The rules of a configuration value's declaration.
fn config(r: &Resource, name: &str, body: &BTreeMap<String, Value>) -> Result<(), CompileError> {
    if let Some(unknown) = body.keys().find(|k| !CONFIG_KEYS.contains(&k.as_str())) {
        return Err(refuse(
            r,
            &rules::INVALID_PARAM,
            format!(
                "a configuration value has no setting {unknown:?} (settings: name, {})",
                CONFIG_KEYS.join(", ")
            ),
        ));
    }
    if body.values().any(holds_reference) {
        return Err(refuse(
            r,
            &rules::INVALID_REFERENCE,
            "a configuration value is read before anything exists, so it cannot refer to a \
             resource"
                .to_owned(),
        ));
    }
    if name.contains(':') {
        // `aws:region` and its like belong to a provider, which reads only a
        // plain value.
        if !body.contains_key("value") || body.keys().any(|k| k != "value" && k != "description") {
            return Err(refuse(
                r,
                &rules::INVALID_PARAM,
                format!(
                    "{name:?} is a provider's setting and takes only \"value\"; \"type\" and \
                     \"default\" declare a value of the program's own"
                ),
            ));
        }
    } else if !body.contains_key("type") && !body.contains_key("default") {
        return Err(refuse(
            r,
            &rules::MISSING_PARAM,
            "a configuration value needs a \"type\" or a \"default\", so Pulumi knows what it \
             holds"
                .to_owned(),
        ));
    }
    Ok(())
}

/// Whether any string in a value holds a `${...}`.
fn holds_reference(value: &Value) -> bool {
    match value {
        Value::String(s) => expr::has_expr(s),
        Value::List(items) => items.iter().any(holds_reference),
        Value::Map(map) => map.values().any(holds_reference),
        _ => false,
    }
}

/// The parts of a resource that mean something to other tools and nothing to
/// Pulumi.
fn check_meta(r: &Resource) -> Result<(), CompileError> {
    if !r.runtime_when.is_empty() {
        return Err(refuse(
            r,
            &rules::UNSUPPORTED_RUNTIME_CONDITION,
            format!(
                "resource carries runtimeWhen {:?}, which this target cannot express: a Pulumi \
                 YAML program declares every resource it manages and has no conditional to \
                 compile it into",
                r.runtime_when
            ),
        ));
    }
    match r.state.as_str() {
        "" | "present" => {}
        "absent" => {
            return Err(refuse(
                r,
                &rules::UNSUPPORTED_STATE,
                "state \"absent\" has no meaning on this target: Pulumi deletes whatever its \
                 program no longer declares, so remove the resource from the document"
                    .to_owned(),
            ))
        }
        other => {
            return Err(refuse(
                r,
                &rules::UNSUPPORTED_STATE,
                format!(
                    "resource type {:?} does not accept state {other:?} on this target \
                     (accepted: present)",
                    r.r#type
                ),
            ))
        }
    }
    Ok(())
}

/// Checks every `${id.path}` in a value and writes it the way Pulumi reads
/// it, recording which resources were referred to.
fn rewrite(
    value: &Value,
    addresses: &Addresses,
    refs: &mut BTreeSet<String>,
) -> Result<Value, String> {
    Ok(match value {
        Value::String(s) => Value::String(rewrite_string(s, addresses, refs)?),
        Value::List(items) => Value::List(
            items
                .iter()
                .map(|item| rewrite(item, addresses, refs))
                .collect::<Result<_, _>>()?,
        ),
        Value::Map(map) => Value::Map(
            map.iter()
                .map(|(k, v)| Ok((k.clone(), rewrite(v, addresses, refs)?)))
                .collect::<Result<_, String>>()?,
        ),
        other => other.clone(),
    })
}

fn rewrite_string(
    text: &str,
    addresses: &Addresses,
    refs: &mut BTreeSet<String>,
) -> Result<String, String> {
    let mut out = String::with_capacity(text.len());
    for part in expr::parse(text)? {
        match part {
            // A string of a Pulumi YAML program is interpolated, so literal
            // text that looks like a reference is escaped to stay literal.
            Part::Text(literal) => out.push_str(&literal.replace("${", "$${")),
            Part::Expr(expression) => {
                if !expr::is_path(&expression) {
                    return Err(format!(
                        "${{{expression}}} is not a reference. Pulumi YAML reads only a name \
                         and its properties inside ${{}}; anything computed is written as one \
                         of its fn:: functions"
                    ));
                }
                let (name, rest) = expr::head(&expression);
                match addresses.get(name) {
                    Some(Address::Name(written)) => {
                        refs.insert(name.to_owned());
                        out.push_str(&format!("${{{written}{rest}}}"));
                    }
                    Some(Address::Secret(written)) => {
                        out.push_str(&format!("${{{written}{rest}}}"));
                    }
                    Some(Address::ProviderSetting(setting)) => {
                        return Err(format!(
                            "${{{expression}}} refers to {name:?}, which is the provider \
                             setting {setting:?}; only the provider reads it"
                        ))
                    }
                    Some(Address::Stack) => {
                        return Err(format!(
                            "${{{expression}}} refers to {name:?}, which holds the stack's \
                             settings and has no properties to read; the stack's name is \
                             ${{pulumi.stack}}"
                        ))
                    }
                    // Pulumi's own names are handed over as written.
                    None if name == PULUMI_NAMESPACE => out.push_str(&format!("${{{expression}}}")),
                    None => {
                        return Err(format!(
                            "${{{expression}}} refers to {name:?}, which is not a resource in \
                             this document"
                        ))
                    }
                }
            }
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_token_is_a_package_then_one_or_two_names() {
        for (t, want) in [
            ("aws:ec2:Instance", true),
            ("aws:s3/bucket:Bucket", true),
            ("random:index:RandomPet", true),
            ("random:RandomPet", true),
            ("pulumi:providers:aws", true),
            ("azure-native:resources:ResourceGroup", true),
            ("kubernetes:apps/v1:Deployment", true),
            ("kubernetes:helm.sh/v3:Release", true),
            ("aws:ec2:getAmi", true),
            ("aws_instance", false),
            ("package", false),
            ("AWS::EC2::Instance", false),
            ("aws:ec2:", false),
            ("aws::Instance", false),
            (":ec2:Instance", false),
            ("Aws:ec2:Instance", false),
            ("aws:ec2:Instance:extra", false),
            ("aws:ec2:My Instance", false),
            ("9aws:ec2:Instance", false),
        ] {
            assert_eq!(is_token(t), want, "{t}");
        }
    }

    fn addresses() -> Addresses {
        Addresses::from([
            ("vm".to_owned(), Address::Name("vm".to_owned())),
            ("size".to_owned(), Address::Name("instanceType".to_owned())),
            (
                "region".to_owned(),
                Address::ProviderSetting("aws:region".to_owned()),
            ),
            ("deployment".to_owned(), Address::Stack),
        ])
    }

    fn rewritten(text: &str) -> Result<(String, Vec<String>), String> {
        let mut refs = BTreeSet::new();
        let out = rewrite_string(text, &addresses(), &mut refs)?;
        Ok((out, refs.into_iter().collect()))
    }

    #[test]
    fn a_reference_is_kept_and_recorded() {
        assert_eq!(
            rewritten("http://${ vm.publicIp }:${vm.ports[0]}/").unwrap(),
            (
                "http://${vm.publicIp}:${vm.ports[0]}/".to_owned(),
                vec!["vm".to_owned()]
            )
        );
        assert_eq!(rewritten("plain").unwrap(), ("plain".to_owned(), vec![]));
    }

    #[test]
    fn a_configuration_value_is_read_by_the_name_pulumi_knows() {
        assert_eq!(
            rewritten("${size}").unwrap(),
            ("${instanceType}".to_owned(), vec!["size".to_owned()])
        );
    }

    #[test]
    fn pulumis_own_names_pass_and_literal_text_is_escaped() {
        assert_eq!(
            rewritten("${pulumi.stack}-web").unwrap(),
            ("${pulumi.stack}-web".to_owned(), vec![])
        );
        assert_eq!(
            rewritten("echo $${HOME} costs $5").unwrap(),
            ("echo $${HOME} costs $5".to_owned(), vec![])
        );
    }

    #[test]
    fn what_cannot_be_read_says_why() {
        assert_eq!(
            rewritten("${vn.id}").unwrap_err(),
            "${vn.id} refers to \"vn\", which is not a resource in this document"
        );
        assert!(rewritten("${region}")
            .unwrap_err()
            .contains("only the provider reads it"));
        assert!(rewritten("${deployment.name}")
            .unwrap_err()
            .contains("${pulumi.stack}"));
        assert!(rewritten("${upper(vm.name)}")
            .unwrap_err()
            .contains("fn:: functions"));
        assert!(rewritten("${vm.id").unwrap_err().contains("never closed"));
    }
}
