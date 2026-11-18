//! Stage one: each resource becomes an object, and every reference inside a
//! manifest becomes the literal it stands for.

use std::collections::BTreeMap;

use meridian_sdk::expr::{self, Part};
use meridian_sdk::rules;
use meridian_sdk::{CompileError, Resource, ResourceGraph, Value};

use crate::{refuse, refuse_at, Cluster, Manifests, Object, OutputPart};

/// Splits `apps/v1/Deployment` into `apps/v1` and `Deployment`.
fn split_type(t: &str) -> Option<(&str, &str)> {
    let (api_version, kind) = t.rsplit_once('/')?;
    let version = api_version.rsplit_once('/').map_or(api_version, |(_, v)| v);
    let ok_kind = kind.starts_with(|c: char| c.is_ascii_uppercase())
        && kind.chars().all(|c| c.is_ascii_alphanumeric());
    let ok_version = version.starts_with('v')
        && version.len() > 1
        && version.chars().all(|c| c.is_ascii_alphanumeric());
    let ok_group = api_version
        .chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '.' || c == '-' || c == '/');
    (ok_kind && ok_version && ok_group && api_version.matches('/').count() <= 1)
        .then_some((api_version, kind))
}

pub(crate) fn transform(graph: &ResourceGraph) -> Result<Manifests, CompileError> {
    let mut manifests = Manifests::default();
    let mut cluster_seen = false;

    // First the objects and their names, so that a reference can be resolved
    // whichever way round the two resources were declared.
    let mut sources: Vec<&Resource> = Vec::new();
    for r in &graph.resources {
        check_meta(r)?;
        if r.r#type == "cluster" {
            if cluster_seen {
                return Err(refuse(
                    r,
                    &rules::DUPLICATE_RESOURCE,
                    "a document goes to one cluster; this one has two resources of type \"cluster\""
                        .to_owned(),
                ));
            }
            cluster_seen = true;
            read_cluster(r, &mut manifests.cluster)?;
            continue;
        }
        let Some((api_version, kind)) = split_type(&r.r#type) else {
            return Err(refuse(
                r,
                &rules::UNSUPPORTED_RESOURCE_TYPE,
                format!(
                    "{:?} is not a Kubernetes object type. Write the API version and the kind, \
                     such as apps/v1/Deployment or v1/Service, or \"cluster\" for where the \
                     manifests go",
                    r.r#type
                ),
            ));
        };

        let metadata = match r.params.get("metadata") {
            None => BTreeMap::new(),
            Some(Value::Map(metadata)) => metadata.clone(),
            Some(_) => {
                return Err(refuse(
                    r,
                    &rules::INVALID_PARAM,
                    "param \"metadata\" must be a map".to_owned(),
                ))
            }
        };
        let field = |key: &str| -> Result<Option<String>, CompileError> {
            match metadata.get(key) {
                None => Ok(None),
                Some(Value::String(s)) if !expr::has_expr(s) => Ok(Some(s.clone())),
                Some(_) => Err(refuse(
                    r,
                    &rules::INVALID_PARAM,
                    format!("metadata.{key} must be a plain string"),
                )),
            }
        };
        // An id is written with underscores and a Kubernetes name cannot hold
        // one, so the default name swaps them for hyphens.
        let name = field("name")?.unwrap_or_else(|| r.id.replace('_', "-"));
        let namespace = field("namespace")?.unwrap_or_default();

        manifests.objects.push(Object {
            id: r.id.clone(),
            api_version: api_version.to_owned(),
            kind: kind.to_owned(),
            name,
            namespace,
            body: BTreeMap::new(),
            pos: r.pos.clone(),
        });
        sources.push(r);
    }

    for (index, r) in sources.iter().enumerate() {
        let mut body = BTreeMap::new();
        for (key, value) in &r.params {
            if key == "apiVersion" || key == "kind" {
                return Err(refuse(
                    r,
                    &rules::UNSUPPORTED_PARAM,
                    format!(
                        "param {key:?} is given by the resource's type and cannot be set again"
                    ),
                ));
            }
            let rewritten = rewrite(value, &manifests.objects).map_err(|msg| {
                refuse(
                    r,
                    &rules::INVALID_REFERENCE,
                    format!("param {key:?}: {msg}"),
                )
            })?;
            body.insert(key.clone(), rewritten);
        }
        let name = manifests.objects[index].name.clone();
        let metadata = body
            .entry("metadata".to_owned())
            .or_insert_with(|| Value::Map(BTreeMap::new()));
        if let Value::Map(metadata) = metadata {
            metadata.insert("name".to_owned(), Value::String(name));
        }
        manifests.objects[index].body = body;
    }

    for output in &graph.outputs {
        let fail = |msg: String| {
            refuse_at(
                "",
                &output.pos,
                &rules::INVALID_REFERENCE,
                format!("output {:?}: {msg}", output.name),
            )
        };
        let mut parts = Vec::new();
        for part in expr::parse(&output.value).map_err(&fail)? {
            match part {
                Part::Text(text) => parts.push(OutputPart::Text(text)),
                Part::Expr(expression) => {
                    let (name, path) = expr::head(&expression);
                    let Some(object) = manifests.objects.iter().position(|o| o.id == name) else {
                        return Err(fail(format!(
                            "${{{expression}}} refers to {name:?}, which is not an object in this document"
                        )));
                    };
                    if !path.starts_with('.') || !expr::is_path(&expression) {
                        return Err(fail(format!(
                            "${{{expression}}} must be a path into the object, such as \
                             ${{{name}.status.loadBalancer.ingress[0].ip}}"
                        )));
                    }
                    parts.push(OutputPart::Read {
                        object,
                        path: path.to_owned(),
                    });
                }
            }
        }
        manifests.outputs.push((output.name.clone(), parts));
    }
    Ok(manifests)
}

fn check_meta(r: &Resource) -> Result<(), CompileError> {
    if !r.runtime_when.is_empty() {
        return Err(refuse(
            r,
            &rules::UNSUPPORTED_RUNTIME_CONDITION,
            format!(
                "resource carries runtimeWhen {:?}, which this target cannot express: a \
                 manifest is declared state, with no condition for the cluster to evaluate",
                r.runtime_when
            ),
        ));
    }
    match r.state.as_str() {
        "" | "present" => Ok(()),
        "absent" => Err(refuse(
            r,
            &rules::UNSUPPORTED_STATE,
            "state \"absent\" has no meaning on this target: applying manifests never deletes \
             an object, so delete it with kubectl and remove it from the document"
                .to_owned(),
        )),
        other => Err(refuse(
            r,
            &rules::UNSUPPORTED_STATE,
            format!(
                "resource type {:?} does not accept state {other:?} on this target (accepted: present)",
                r.r#type
            ),
        )),
    }
}

fn read_cluster(r: &Resource, cluster: &mut Cluster) -> Result<(), CompileError> {
    for (name, value) in &r.params {
        let slot = match name.as_str() {
            "context" => &mut cluster.context,
            "namespace" => &mut cluster.namespace,
            "kubeconfig" => &mut cluster.kubeconfig,
            _ => {
                return Err(refuse(
                    r,
                    &rules::UNSUPPORTED_PARAM,
                    format!(
                        "resource type \"cluster\" does not accept param {name:?} on this \
                         target (accepted: context, kubeconfig, namespace)"
                    ),
                ))
            }
        };
        let Value::String(text) = value else {
            return Err(refuse(
                r,
                &rules::INVALID_PARAM,
                format!("param {name:?} of the cluster must be a string"),
            ));
        };
        *slot = text.clone();
    }
    Ok(())
}

/// Replaces every reference in a value with the literal it stands for.
fn rewrite(value: &Value, objects: &[Object]) -> Result<Value, String> {
    Ok(match value {
        Value::String(s) => Value::String(rewrite_string(s, objects)?),
        Value::List(items) => Value::List(
            items
                .iter()
                .map(|item| rewrite(item, objects))
                .collect::<Result<_, _>>()?,
        ),
        Value::Map(map) => Value::Map(
            map.iter()
                .map(|(k, v)| Ok((k.clone(), rewrite(v, objects)?)))
                .collect::<Result<_, String>>()?,
        ),
        other => other.clone(),
    })
}

fn rewrite_string(text: &str, objects: &[Object]) -> Result<String, String> {
    let mut out = String::with_capacity(text.len());
    for part in expr::parse(text)? {
        match part {
            Part::Text(literal) => out.push_str(&literal),
            Part::Expr(expression) => {
                let (name, path) = expr::head(&expression);
                let Some(object) = objects.iter().find(|o| o.id == name) else {
                    return Err(format!(
                        "${{{expression}}} refers to {name:?}, which is not an object in this document"
                    ));
                };
                match path {
                    "" | ".metadata.name" => out.push_str(&object.name),
                    ".metadata.namespace" if !object.namespace.is_empty() => {
                        out.push_str(&object.namespace)
                    }
                    ".metadata.namespace" => {
                        return Err(format!(
                            "${{{expression}}} reads a namespace {name:?} does not set"
                        ))
                    }
                    _ => {
                        return Err(format!(
                            "${{{expression}}} is only known once the object exists, and \
                             Kubernetes cannot wire one object's state into another's manifest. \
                             Inside params a reference can be ${{{name}}}, \
                             ${{{name}.metadata.name}} or ${{{name}.metadata.namespace}}; any \
                             path can be read in an output"
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
    fn a_type_is_an_api_version_and_a_kind() {
        assert_eq!(
            split_type("apps/v1/Deployment"),
            Some(("apps/v1", "Deployment"))
        );
        assert_eq!(split_type("v1/Service"), Some(("v1", "Service")));
        assert_eq!(
            split_type("networking.k8s.io/v1/Ingress"),
            Some(("networking.k8s.io/v1", "Ingress"))
        );
        assert_eq!(
            split_type("batch/v1beta1/CronJob"),
            Some(("batch/v1beta1", "CronJob"))
        );
        for bad in [
            "Deployment",
            "apps/v1/deployment",
            "apps/Deployment",
            "a/b/v1/Kind",
            "package",
        ] {
            assert_eq!(split_type(bad), None, "{bad}");
        }
    }
}
