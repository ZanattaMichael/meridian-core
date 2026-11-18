//! Stage one: each resource becomes a template resource, and every reference
//! between resources becomes a template expression.

use std::collections::{BTreeMap, BTreeSet};

use meridian_sdk::expr::{self, Part};
use meridian_sdk::{CompileError, Resource, ResourceGraph, Value};

use crate::expression::{self, quote};
use crate::{refuse, refuse_at, secret_parameter, Entry, Parameter, Scope, Settings, Template};

/// The type of the resource that holds the deployment's own settings.
pub(crate) const DEPLOYMENT: &str = "deployment";
/// The type of a resource that declares a template parameter.
pub(crate) const PARAMETER: &str = "parameter";

/// `Namespace.Provider/type`, with a further `/type` for each level of child.
pub(crate) fn is_resource_type(t: &str) -> bool {
    let mut parts = t.split('/');
    let word = |w: &str, extra: &str| {
        !w.is_empty()
            && w.chars()
                .all(|c| c.is_ascii_alphanumeric() || extra.contains(c))
    };
    let namespace = parts.next().unwrap_or("");
    let mut types = 0;
    for part in parts {
        if !word(part, ".-_") {
            return false;
        }
        types += 1;
    }
    types > 0 && namespace.contains('.') && word(namespace, ".") && !namespace.starts_with('.')
}

/// `2023-09-01`, or the same with a suffix such as `-preview`.
pub(crate) fn is_api_version(v: &str) -> bool {
    let bytes = v.as_bytes();
    let date = bytes.len() >= 10
        && bytes[..10].iter().enumerate().all(|(i, b)| match i {
            4 | 7 => *b == b'-',
            _ => b.is_ascii_digit(),
        });
    date && match &v[10..] {
        "" => true,
        suffix => {
            suffix.len() > 1
                && suffix.starts_with('-')
                && suffix[1..].chars().all(|c| c.is_ascii_lowercase())
        }
    }
}

/// What a name in `${...}` stands for.
#[derive(Clone, Copy)]
enum Symbol {
    Parameter,
    /// A template resource, by its place in the document.
    Resource(usize),
}

/// A value on its way into an expression.
#[derive(Clone, Debug, PartialEq)]
enum Operand {
    /// Text known when the template is compiled.
    Literal(String),
    /// An expression, without its square brackets.
    Expr(String),
}

impl Operand {
    /// Reads a string as a template would.
    fn from_template(text: &str) -> Operand {
        match expression::inner(text) {
            Some(inner) => Operand::Expr(inner.to_owned()),
            // `[[` at the start is how a template writes a literal `[`, and
            // how the core hands over a value from data that opens with
            // one. It is read as the literal whether or not the text ends
            // with `]`, so that what is written back (`[[` only where the
            // text would otherwise be an expression) means the same.
            None if text.starts_with("[[") => Operand::Literal(text[1..].to_owned()),
            None => Operand::Literal(text.to_owned()),
        }
    }

    /// As an argument of a function.
    fn argument(&self) -> String {
        match self {
            Operand::Literal(text) => quote(text),
            Operand::Expr(e) => e.clone(),
        }
    }

    /// As a whole string of the template.
    fn template(&self) -> String {
        match self {
            Operand::Literal(text) if text.starts_with('[') && text.ends_with(']') => {
                format!("[{text}")
            }
            Operand::Literal(text) => text.clone(),
            Operand::Expr(e) => format!("[{e}]"),
        }
    }
}

/// Why a value could not be rewritten: the rule broken, and the message.
struct Fault(&'static str, String);

/// Where a value being rewritten sits.
struct Site<'a> {
    /// The resource it belongs to; empty for an output.
    owner: &'a str,
    /// Whether `reference()` may be used here. Resource Manager allows it in
    /// a resource's `properties` and in outputs, and nowhere else.
    runtime: bool,
}

struct Resolver<'a> {
    graph: &'a ResourceGraph,
    symbols: BTreeMap<&'a str, Symbol>,
    scope: Scope,
    /// The `name` and `scope` of resources, once worked out.
    fields: BTreeMap<(usize, &'static str), Option<Operand>>,
    /// The fields being worked out now, to catch one defined by itself.
    resolving: Vec<(usize, &'static str)>,
}

impl Resolver<'_> {
    /// A resource's `name` or `scope` as an operand. A name left out is the
    /// resource's id; a scope left out is `None`.
    fn field(&mut self, index: usize, key: &'static str) -> Result<Option<Operand>, Fault> {
        if let Some(known) = self.fields.get(&(index, key)) {
            return Ok(known.clone());
        }
        let r = &self.graph.resources[index];
        let written = match (r.params.get(key), key) {
            (Some(Value::String(text)), _) => Some(text.clone()),
            (Some(other), _) => {
                return Err(Fault(
                    "invalid-param",
                    format!(
                        "param {key:?} of resource {:?} must be text, not {}",
                        r.id,
                        other.describe()
                    ),
                ))
            }
            (None, "name") => Some(r.id.clone()),
            (None, _) => None,
        };
        let operand = match written {
            None => None,
            Some(text) => {
                if self.resolving.contains(&(index, key)) {
                    return Err(Fault(
                        "reference-cycle",
                        format!(
                            "the {key} of resource {:?} is defined in terms of itself",
                            r.id
                        ),
                    ));
                }
                self.resolving.push((index, key));
                let site = Site {
                    owner: &r.id,
                    runtime: false,
                };
                let rewritten = self.string(&text, &site, &mut BTreeSet::new());
                self.resolving.pop();
                Some(Operand::from_template(&rewritten?.0))
            }
        };
        self.fields.insert((index, key), operand.clone());
        Ok(operand)
    }

    /// The expression that is a resource's id in Azure.
    fn resource_id(&mut self, index: usize) -> Result<String, Fault> {
        let r = &self.graph.resources[index];
        let segments = r.r#type.split('/').count() - 1;
        let name = self
            .field(index, "name")?
            .expect("a resource always has a name");
        let mut arguments = vec![quote(&r.r#type)];
        match name {
            Operand::Literal(text) => {
                let parts: Vec<&str> = text.split('/').collect();
                if parts.len() != segments || parts.iter().any(|p| p.is_empty()) {
                    return Err(Fault(
                        "invalid-name",
                        format!(
                            "resource {:?} has the type {:?}, whose name has {segments} part{} \
                             separated by \"/\", but its name is {text:?}",
                            r.id,
                            r.r#type,
                            if segments == 1 { "" } else { "s" },
                        ),
                    ));
                }
                arguments.extend(parts.into_iter().map(quote));
            }
            Operand::Expr(e) if segments == 1 => arguments.push(e),
            // A child's name is only known at deployment, so it is taken
            // apart there.
            Operand::Expr(e) => {
                arguments.extend((0..segments).map(|i| format!("split({e}, '/')[{i}]")));
            }
        }
        Ok(match self.field(index, "scope")? {
            // A resource that extends another is addressed through it.
            Some(scope) => format!(
                "extensionResourceId({}, {})",
                scope.argument(),
                arguments.join(", ")
            ),
            None => format!(
                "{}({})",
                match self.scope {
                    Scope::ResourceGroup => "resourceId",
                    Scope::Subscription => "subscriptionResourceId",
                },
                arguments.join(", ")
            ),
        })
    }

    /// One `${...}` resolved, and whether the result is certainly text.
    fn reference(
        &mut self,
        expression: &str,
        site: &Site,
        refs: &mut BTreeSet<String>,
    ) -> Result<(Operand, bool), Fault> {
        let (name, rest) = expr::head(expression);
        let unreadable = |why: &str| {
            Fault(
                "invalid-reference",
                format!("${{{expression}}} is not a reference this target can read: {why}"),
            )
        };
        let checked = |built: String| match expression::check(&built) {
            Ok(()) => Ok(Operand::Expr(built)),
            Err(why) => Err(unreadable(&format!("it would become [{built}], and {why}"))),
        };
        let index = match self.symbols.get(name) {
            None => {
                return Err(Fault(
                    "invalid-reference",
                    format!(
                        "${{{expression}}} refers to {name:?}, which is not a resource or a \
                         parameter of this document. An ARM expression is written in square \
                         brackets, not in ${{}}"
                    ),
                ))
            }
            Some(Symbol::Parameter) => {
                return Ok((
                    checked(format!("parameters({}){rest}", quote(name)))?,
                    false,
                ));
            }
            Some(Symbol::Resource(index)) => *index,
        };
        let r = &self.graph.resources[index];
        let own = name == site.owner;
        if !own {
            refs.insert(name.to_owned());
        }

        let (first, tail) = if rest.is_empty() {
            ("id", "")
        } else {
            let Some(body) = rest.strip_prefix('.') else {
                return Err(unreadable(&format!(
                    "write ${{{name}}} for the resource's id, ${{{name}.name}} for its name or \
                     ${{{name}.properties.<path>}} for its state"
                )));
            };
            let end = body.find(['.', '[', '?']).unwrap_or(body.len());
            body.split_at(end)
        };
        let api_version = || match r.params.get("apiVersion") {
            Some(Value::String(v)) => Ok(v.clone()),
            _ => Err(Fault(
                "missing-param",
                format!("resource {name:?} has no apiVersion to read it with"),
            )),
        };
        match first {
            "id" | "name" | "type" | "apiVersion" if !tail.is_empty() => Err(unreadable(&format!(
                "the {first} of a resource is text and has nothing under it"
            ))),
            "id" => Ok((Operand::Expr(self.resource_id(index)?), true)),
            "name" => Ok((
                self.field(index, "name")?
                    .expect("a resource always has a name"),
                true,
            )),
            "type" => Ok((Operand::Literal(r.r#type.clone()), true)),
            "apiVersion" => Ok((Operand::Literal(api_version()?), true)),
            "" => Err(unreadable("a dot must be followed by a name")),
            _ if !site.runtime => Err(Fault(
                "unsupported-reference",
                format!(
                    "${{{expression}}} reads the state of {name:?}, which needs reference(). \
                     Resource Manager allows that only inside a resource's \"properties\" and in \
                     outputs; here only the id, name, type and apiVersion of a resource can be read"
                ),
            )),
            _ if own => Err(Fault(
                "self-reference",
                format!(
                    "${{{expression}}} reads the state of the resource it is written in, which \
                     Resource Manager refuses as a circular dependency"
                ),
            )),
            _ => {
                let id = self.resource_id(index)?;
                let version = quote(&api_version()?);
                let built = if first == "properties" {
                    format!("reference({id}, {version}){tail}")
                } else {
                    format!("reference({id}, {version}, 'Full').{first}{tail}")
                };
                Ok((checked(built)?, false))
            }
        }
    }

    /// Rewrites one string. Also says whether the result is certainly text,
    /// which is what decides how an output is typed.
    fn string(
        &mut self,
        text: &str,
        site: &Site,
        refs: &mut BTreeSet<String>,
    ) -> Result<(String, bool), Fault> {
        let parts = expr::parse(text).map_err(|why| Fault("invalid-reference", why))?;
        if let [Part::Expr(expression)] = parts.as_slice() {
            let (operand, text) = self.reference(expression, site, refs)?;
            return Ok((operand.template(), text));
        }
        // `[[` at the start is a literal `[`: how a template writes one,
        // and how the core hands over a value from data that opens with
        // one. It is kept where Resource Manager needs it to read the string
        // as text, and dropped where the string could never be an
        // expression, so that the deployed value is the literal either way.
        let escaped = text.starts_with("[[");
        let references = parts.iter().any(|p| matches!(p, Part::Expr(_)));
        if !references {
            let mut literal: String = parts
                .iter()
                .map(|p| match p {
                    Part::Text(t) | Part::Expr(t) => t.as_str(),
                })
                .collect();
            if escaped && !literal.ends_with(']') {
                literal.remove(0);
            }
            let text = expression::inner(&literal).is_none();
            return Ok((literal, text));
        }

        // References inside an expression the author wrote: each is put in
        // place as an argument.
        let opens = matches!(parts.first(), Some(Part::Text(t)) if t.starts_with('[') && !t.starts_with("[["));
        let closes = matches!(parts.last(), Some(Part::Text(t)) if t.ends_with(']'));
        if opens && closes {
            let mut out = String::new();
            for part in &parts {
                match part {
                    Part::Text(literal) => out.push_str(literal),
                    Part::Expr(expression) => {
                        out.push_str(&self.reference(expression, site, refs)?.0.argument());
                    }
                }
            }
            return Ok((out, false));
        }

        // References inside plain text: one format(). What format() gives
        // is never read as an expression again, so the escape is dropped.
        let mut pattern = String::new();
        let mut arguments = Vec::new();
        for (i, part) in parts.iter().enumerate() {
            match part {
                Part::Text(literal) => {
                    let literal = if i == 0 && escaped {
                        &literal[1..]
                    } else {
                        literal
                    };
                    pattern.push_str(&literal.replace('{', "{{").replace('}', "}}"));
                }
                Part::Expr(expression) => {
                    pattern.push_str(&format!("{{{}}}", arguments.len()));
                    arguments.push(self.reference(expression, site, refs)?.0.argument());
                }
            }
        }
        Ok((
            format!("[format({}, {})]", quote(&pattern), arguments.join(", ")),
            true,
        ))
    }

    fn value(
        &mut self,
        value: &Value,
        site: &Site,
        refs: &mut BTreeSet<String>,
    ) -> Result<Value, Fault> {
        Ok(match value {
            Value::String(s) => Value::String(self.string(s, site, refs)?.0),
            Value::List(items) => Value::List(
                items
                    .iter()
                    .map(|item| self.value(item, site, refs))
                    .collect::<Result<_, _>>()?,
            ),
            Value::Map(map) => Value::Map(
                map.iter()
                    .map(|(k, v)| Ok((k.clone(), self.value(v, site, refs)?)))
                    .collect::<Result<_, Fault>>()?,
            ),
            other => other.clone(),
        })
    }
}

pub(crate) fn transform(graph: &ResourceGraph) -> Result<Template, CompileError> {
    let mut template = Template {
        document: graph.name.clone(),
        settings: Settings {
            name: graph.name.clone(),
            mode: "Incremental".to_owned(),
            ..Settings::default()
        },
        ..Template::default()
    };

    let mut symbols = BTreeMap::new();
    let mut settings_seen = false;
    for (index, r) in graph.resources.iter().enumerate() {
        check_meta(r)?;
        match r.r#type.as_str() {
            DEPLOYMENT => {
                if settings_seen {
                    return Err(refuse(
                        r,
                        "duplicate-deployment",
                        "a document is one deployment; this one has two resources of type \
                         \"deployment\""
                            .to_owned(),
                    ));
                }
                settings_seen = true;
                read_settings(r, &mut template.settings)?;
            }
            PARAMETER => {
                symbols.insert(r.id.as_str(), Symbol::Parameter);
                template.parameters.push(Parameter {
                    name: r.id.clone(),
                    definition: r.params.clone(),
                    pos: r.pos.clone(),
                });
            }
            t if is_resource_type(t) => {
                symbols.insert(r.id.as_str(), Symbol::Resource(index));
            }
            other => {
                return Err(refuse(
                    r,
                    "unsupported-resource-type",
                    format!(
                        "{other:?} is not an ARM resource type. Use Azure's own type, such as \
                         Microsoft.Storage/storageAccounts, \"parameter\" for a template \
                         parameter, or \"deployment\" for the deployment's settings"
                    ),
                ));
            }
        }
    }

    // Each secret is a securestring parameter, which a param refers to as
    // any parameter: `[parameters('meridian_secret_<name>')]`.
    let secret_parameters: Vec<String> = graph
        .secrets
        .iter()
        .map(|secret| secret_parameter(&secret.name))
        .collect();
    for name in &secret_parameters {
        symbols.insert(name.as_str(), Symbol::Parameter);
        template.parameters.push(Parameter {
            name: name.clone(),
            definition: BTreeMap::from([("type".to_owned(), Value::from("securestring"))]),
            pos: meridian_sdk::Position::default(),
        });
    }

    let mut resolver = Resolver {
        graph,
        symbols,
        scope: template.settings.scope,
        fields: BTreeMap::new(),
        resolving: Vec::new(),
    };

    for (index, r) in graph.resources.iter().enumerate() {
        if matches!(r.r#type.as_str(), DEPLOYMENT | PARAMETER) {
            continue;
        }
        let in_param =
            |key: &str, fault: Fault| refuse(r, fault.0, format!("param {key:?}: {}", fault.1));
        let mut depends = BTreeSet::new();
        let mut body = BTreeMap::new();
        let mut name = r.id.clone();
        let mut api_version = None;
        for (key, value) in &r.params {
            match (key.as_str(), value) {
                ("type" | "dependsOn", _) => {
                    return Err(refuse(
                        r,
                        "reserved-param",
                        format!(
                            "param {key:?} is written by Meridian: the type is the resource's \
                             own type, and an order is given with the resource's dependsOn"
                        ),
                    ))
                }
                ("apiVersion", Value::String(v)) if is_api_version(v) => {
                    api_version = Some(v.clone());
                }
                ("apiVersion", _) => {
                    return Err(refuse(
                        r,
                        "invalid-param",
                        "param \"apiVersion\" must be a version of the resource type's API, \
                         such as 2023-09-01 or 2023-09-01-preview"
                            .to_owned(),
                    ))
                }
                ("name" | "scope", Value::String(text)) => {
                    let site = Site {
                        owner: &r.id,
                        runtime: false,
                    };
                    // Marked as being worked out, so a name defined through
                    // another resource's name that leads back here is caught.
                    let field = if key == "name" { "name" } else { "scope" };
                    resolver.resolving.push((index, field));
                    let rewritten = resolver.string(text, &site, &mut depends);
                    resolver.resolving.pop();
                    let (rewritten, _) = rewritten.map_err(|fault| in_param(key, fault))?;
                    if key == "name" {
                        name = rewritten;
                    } else {
                        body.insert(key.clone(), Value::String(rewritten));
                    }
                }
                ("name" | "scope", other) => {
                    return Err(refuse(
                        r,
                        "invalid-param",
                        format!("param {key:?} must be text, not {}", other.describe()),
                    ))
                }
                _ => {
                    let site = Site {
                        owner: &r.id,
                        runtime: key == "properties",
                    };
                    let rewritten = resolver
                        .value(value, &site, &mut depends)
                        .map_err(|fault| in_param(key, fault))?;
                    body.insert(key.clone(), rewritten);
                }
            }
        }
        let Some(api_version) = api_version else {
            return Err(refuse(
                r,
                "missing-param",
                format!(
                    "resource type {:?} needs param \"apiVersion\": the version of the type's \
                     API to deploy it with, such as 2023-09-01",
                    r.r#type
                ),
            ));
        };
        let resource_id = resolver
            .resource_id(index)
            .map_err(|fault| in_param("name", fault))?;
        template.entries.push(Entry {
            id: r.id.clone(),
            r#type: r.r#type.clone(),
            api_version,
            name,
            resource_id,
            body,
            depends,
            depends_on: Vec::new(),
            pos: r.pos.clone(),
        });
    }

    for output in &graph.outputs {
        let site = Site {
            owner: "",
            runtime: true,
        };
        let (mut value, text) = resolver
            .string(&output.value, &site, &mut BTreeSet::new())
            .map_err(|fault| {
                refuse_at(
                    "",
                    &output.pos,
                    fault.0,
                    format!("output {:?}: {}", output.name, fault.1),
                )
            })?;
        // Every output is declared as a string, so a value that might be
        // something else is turned into one.
        if !text {
            if let Some(inner) = expression::inner(&value) {
                value = format!("[string({inner})]");
            }
        }
        template
            .outputs
            .push((output.name.clone(), value, output.pos.clone()));
    }
    Ok(template)
}

fn check_meta(r: &Resource) -> Result<(), CompileError> {
    if !r.runtime_when.is_empty() {
        return Err(refuse(
            r,
            "unsupported-runtime-condition",
            format!(
                "resource carries runtimeWhen {:?}, which this target cannot express: an ARM \
                 condition is decided from the template's parameters before anything is \
                 deployed, not from the state of what it deploys to",
                r.runtime_when
            ),
        ));
    }
    match r.state.as_str() {
        "" | "present" => Ok(()),
        "absent" => Err(refuse(
            r,
            "unsupported-state",
            "state \"absent\" has no meaning on this target: a template only says what should \
             exist. Remove the resource from the document, and deploy with mode Complete if \
             what the template no longer declares should be deleted"
                .to_owned(),
        )),
        other => Err(refuse(
            r,
            "unsupported-state",
            format!(
                "resource type {:?} does not accept state {other:?} on this target (accepted: present)",
                r.r#type
            ),
        )),
    }
}

fn read_settings(r: &Resource, settings: &mut Settings) -> Result<(), CompileError> {
    for (name, value) in &r.params {
        let Value::String(text) = value else {
            return Err(refuse(
                r,
                if is_setting(name) {
                    "invalid-param"
                } else {
                    "unsupported-param"
                },
                if is_setting(name) {
                    format!("param {name:?} of the deployment must be text")
                } else {
                    unknown_setting(name)
                },
            ));
        };
        match name.as_str() {
            "name" => settings.name = text.clone(),
            "resourceGroup" => settings.resource_group = text.clone(),
            "location" => settings.location = text.clone(),
            "subscription" => settings.subscription = text.clone(),
            "mode" => settings.mode = text.clone(),
            "scope" => {
                settings.scope = match text.as_str() {
                    "resourceGroup" => Scope::ResourceGroup,
                    "subscription" => Scope::Subscription,
                    other => {
                        return Err(refuse(
                            r,
                            "invalid-param",
                            format!(
                                "{other:?} is not a scope this target deploys to (accepted: \
                                 resourceGroup, subscription)"
                            ),
                        ))
                    }
                }
            }
            _ => return Err(refuse(r, "unsupported-param", unknown_setting(name))),
        }
    }
    Ok(())
}

const SETTINGS: &[&str] = &[
    "location",
    "mode",
    "name",
    "resourceGroup",
    "scope",
    "subscription",
];

fn is_setting(name: &str) -> bool {
    SETTINGS.contains(&name)
}

fn unknown_setting(name: &str) -> String {
    format!(
        "resource type \"deployment\" does not accept param {name:?} on this target (accepted: {})",
        SETTINGS.join(", ")
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_resource_type_is_a_namespace_and_at_least_one_type() {
        for (t, want) in [
            ("Microsoft.Storage/storageAccounts", true),
            ("Microsoft.Sql/servers/databases", true),
            ("microsoft.insights/components", true),
            ("Microsoft.Network/virtualNetworks/subnets", true),
            ("Contoso.Custom.Provider/things", true),
            ("azurerm_storage_account", false),
            ("AWS::S3::Bucket", false),
            ("Microsoft.Storage", false),
            ("Microsoft.Storage/", false),
            ("Storage/storageAccounts", false),
            ("Microsoft.Sql//databases", false),
            (".Sql/servers", false),
            ("Microsoft.Sql/ser vers", false),
        ] {
            assert_eq!(is_resource_type(t), want, "{t}");
        }
    }

    #[test]
    fn an_api_version_is_a_date_with_an_optional_suffix() {
        for (v, want) in [
            ("2023-09-01", true),
            ("2021-04-01-preview", true),
            ("2015-01-01-privatepreview", true),
            ("2023-9-1", false),
            ("2023-09-01-", false),
            ("2023-09-01-Preview", false),
            ("2023-09-01preview", false),
            ("latest", false),
            ("", false),
            ("20230901", false),
        ] {
            assert_eq!(is_api_version(v), want, "{v}");
        }
    }

    #[test]
    fn an_operand_reads_and_writes_template_strings() {
        assert_eq!(
            Operand::from_template("[parameters('x')]"),
            Operand::Expr("parameters('x')".into())
        );
        assert_eq!(
            Operand::from_template("web"),
            Operand::Literal("web".into())
        );
        // A literal in brackets is escaped on the way out and read back.
        let literal = Operand::Literal("[web]".into());
        assert_eq!(literal.template(), "[[web]");
        assert_eq!(Operand::from_template("[[web]"), literal);
        assert_eq!(Operand::Literal("it's".into()).argument(), "'it''s'");
        assert_eq!(Operand::Expr("a()".into()).template(), "[a()]");
    }
}
