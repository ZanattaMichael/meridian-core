//! A plugin's schema is advice to an editor; `emit` is the judge. These
//! tests hold every plugin that describes its types to that: what the
//! schema lists must compile, and what it leaves out must be refused. A
//! schema that drifted from its plugin would have an editor offer params
//! the compiler rejects, or mark as wrong a document that compiles.

mod common;

use meridian_ast::ResourceGraph;
use meridian_plugin_host::{to_sdk, EmitError, Registry};
use meridian_sdk::{Data, ParamKind, ParamSchema, ResourceSchema};
use meridian_tooling::catalog::Catalog;

fn sample(param: &ParamSchema) -> String {
    match (param.name.as_str(), param.kind) {
        (_, ParamKind::Bool) => "true".to_owned(),
        (_, ParamKind::Integer | ParamKind::Number) => "1000".to_owned(),
        (_, ParamKind::List) => "[one]".to_owned(),
        (_, ParamKind::Map) => "{a: b}".to_owned(),
        ("path" | "home" | "chdir" | "creates" | "removes", _) => "/tmp/x".to_owned(),
        ("mode", _) => "\"0644\"".to_owned(),
        ("command", _) => "/bin/true".to_owned(),
        ("encoding", _) => "b64".to_owned(),
        _ => "sample".to_owned(),
    }
}

struct Doc<'a> {
    target: &'a str,
    schema: &'a ResourceSchema,
    extra: Vec<(String, String)>,
    without: Option<&'a str>,
    state: Option<&'a str>,
    /// A second resource that notifies the first with this action.
    notify: Option<&'a str>,
}

impl Doc<'_> {
    fn text(&self) -> String {
        let mut params: Vec<(String, String)> = self
            .schema
            .params
            .iter()
            .filter(|p| p.required && Some(p.name.as_str()) != self.without)
            .map(|p| (p.name.clone(), sample(p)))
            .collect();
        for (name, value) in &self.extra {
            if !params.iter().any(|(n, _)| n == name) {
                params.push((name.clone(), value.clone()));
            }
        }
        let mut out = format!(
            "apiVersion: meridian/v1\nkind: ResourceSet\nmetadata:\n  name: t\nspec:\n  \
             target: {}\n  targetHost:\n    from: inline\n    host: h\n  resources:\n    \
             - id: subject\n      type: {}\n",
            self.target, self.schema.r#type
        );
        if let Some(state) = self.state {
            out.push_str(&format!("      state: {state}\n"));
        }
        if !params.is_empty() {
            out.push_str("      params:\n");
            for (name, value) in params {
                out.push_str(&format!("        {name}: {value}\n"));
            }
        }
        if let Some(action) = self.notify {
            out.push_str(&format!(
                "    - id: trigger\n      type: file\n      params:\n        path: /tmp/trigger\n        \
                 content: x\n      notifies:\n        - resource: subject\n          action: {action}\n"
            ));
        }
        out
    }
}

/// Compiles a document with its target, the way the orchestrator does, and
/// returns the rule of the refusal if it was refused.
fn refusal(registry: &Registry, text: &str) -> Option<String> {
    let docs =
        meridian_ir::parse("t.yaml", text.as_bytes()).unwrap_or_else(|e| panic!("{e}\n{text}"));
    let rs = docs[0].as_resource_set().expect("a resource set");
    let graph = ResourceGraph::build(rs, "h").unwrap_or_else(|e| panic!("{e}\n{text}"));
    let target = registry
        .target(&rs.spec.target)
        .expect("the target is loaded");
    match target.emit(&to_sdk(&graph), &Data::new()) {
        Ok(_) => None,
        Err(EmitError::Refused(e)) => Some(e.rule),
        Err(EmitError::Plugin(e)) => panic!("{e}"),
    }
}

#[test]
fn a_schema_matches_what_the_target_compiles() {
    let registry = common::registry();
    let catalog = Catalog::from_registry(&registry);
    let mut checked = 0;
    for target in catalog.targets() {
        for schema in target.types.iter().filter(|t| !t.open) {
            let name = format!("{} {}", target.name, schema.r#type);
            let base = || Doc {
                target: &target.name,
                schema,
                extra: Vec::new(),
                without: None,
                state: None,
                notify: None,
            };

            // A param the schema does not list is refused.
            let bogus = Doc {
                extra: vec![("no_such_param".to_owned(), "x".to_owned())],
                ..base()
            };
            assert_eq!(
                refusal(&registry, &bogus.text()).as_deref(),
                Some("unsupported-param"),
                "{name}: a param outside the schema must be refused"
            );

            // Every param it lists is taken.
            for param in &schema.params {
                let doc = Doc {
                    extra: vec![(param.name.clone(), sample(param))],
                    ..base()
                };
                assert_ne!(
                    refusal(&registry, &doc.text()).as_deref(),
                    Some("unsupported-param"),
                    "{name}: the schema lists param {:?} but the target refuses it",
                    param.name
                );
            }

            // Every required param is really required.
            for param in schema.params.iter().filter(|p| p.required) {
                let doc = Doc {
                    without: Some(&param.name),
                    ..base()
                };
                assert!(
                    refusal(&registry, &doc.text()).is_some(),
                    "{name}: the schema requires {:?} but the target compiles without it",
                    param.name
                );
            }

            // Every state it lists is a state the target knows, and no other.
            for state in &schema.states {
                let doc = Doc {
                    state: Some(state),
                    ..base()
                };
                assert_ne!(
                    refusal(&registry, &doc.text()).as_deref(),
                    Some("unsupported-state"),
                    "{name}: the schema lists state {state:?} but the target refuses it"
                );
            }
            let doc = Doc {
                state: Some("no_such_state"),
                ..base()
            };
            assert_eq!(
                refusal(&registry, &doc.text()).as_deref(),
                Some("unsupported-state"),
                "{name}: a state outside the schema must be refused"
            );
            if !schema.states.is_empty() {
                assert!(
                    schema.states.contains(&schema.default_state),
                    "{name}: the default state is not one of the states"
                );
            }

            // Every action it lists can be notified, and no other.
            for action in &schema.actions {
                let doc = Doc {
                    notify: Some(action),
                    ..base()
                };
                assert_ne!(
                    refusal(&registry, &doc.text()).as_deref(),
                    Some("unsupported-action"),
                    "{name}: the schema lists action {action:?} but the target refuses it"
                );
            }
            {
                let doc = Doc {
                    notify: Some("no_such_action"),
                    ..base()
                };
                assert!(
                    refusal(&registry, &doc.text()).is_some(),
                    "{name}: an action outside the schema must be refused"
                );
            }
            checked += 1;
        }
    }
    // Six targets, six shared types each.
    assert_eq!(checked, 36);
}

#[test]
fn a_target_that_passes_types_through_is_open() {
    let registry = common::registry();
    let catalog = Catalog::from_registry(&registry);
    let terraform = catalog.target("terraform").unwrap();
    assert!(terraform.types.iter().all(|t| t.open));
    assert_eq!(
        terraform.resource("aws_instance").unwrap().r#type,
        "<provider>_<type>"
    );
    assert_eq!(catalog.target("dsc").unwrap().versions, ["2", "3"]);
}
