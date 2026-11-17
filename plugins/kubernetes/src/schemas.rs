//! Asking the cluster's API server what each kind's fields are.
//!
//! Every API server publishes an OpenAPI document (`kubectl get --raw
//! /openapi/v2`) that describes the built-in kinds and every custom
//! resource definition installed, so an editor can know `apps/v1/Deployment`
//! and the cluster's own `cert-manager.io/v1/Certificate` alike. This half
//! of the plugin reads that document through `kubectl`, against the
//! cluster the documents name, and turns the definitions into resource
//! schemas keyed the way a document writes a type: `<apiVersion>/<Kind>`.
//!
//! The kinds the documents use are described field by field. Every other
//! kind is listed by name with its description, so completion offers it;
//! the next sync after a document starts using one describes it in full.

use meridian_sdk::tool::Tool;
use meridian_sdk::{
    decode_value, version_in, Log, ParamSchema, ResourceGraph, ResourceSchema, RunError,
    SchemaExport, SchemaRequest, SchemaSource, Value,
};

use meridian_sdk::schema::params_from_json_schema;

use crate::runner::{ARGS_VARIABLE, PROGRAM_VARIABLE};

/// The path on the API server that serves the OpenAPI document.
const OPENAPI_PATH: &str = "/openapi/v2";

/// The extension that says which `apiVersion/kind` a definition describes.
const GVK: &str = "x-kubernetes-group-version-kind";

/// The name of the `cluster` resource type in a document.
const CLUSTER_TYPE: &str = "cluster";

/// Asks the cluster for its OpenAPI document.
pub struct KubernetesSchemas {
    /// The program to run: `kubectl`.
    pub program: String,
    /// Arguments given to every `kubectl` call, from the environment.
    pub extra_args: Vec<String>,
}

impl KubernetesSchemas {
    /// The source as the environment configures it, the same variables the
    /// runner reads.
    pub fn from_env() -> Self {
        KubernetesSchemas {
            program: std::env::var(PROGRAM_VARIABLE)
                .ok()
                .filter(|v| !v.trim().is_empty())
                .unwrap_or_else(|| "kubectl".to_owned()),
            extra_args: std::env::var(ARGS_VARIABLE)
                .map(|v| v.split_whitespace().map(str::to_owned).collect())
                .unwrap_or_default(),
        }
    }

    fn kubectl(&self, cluster: &ClusterSettings, operation: &[&str]) -> Tool {
        let mut tool = Tool::new(&self.program)
            .args(operation.iter().copied())
            .capture_stdout();
        for (flag, value) in [
            ("--context", cluster.context.as_str()),
            ("--kubeconfig", cluster.kubeconfig.as_str()),
        ] {
            if !value.is_empty() {
                tool = tool.args([flag, value]);
            }
        }
        tool.args(self.extra_args.iter().cloned())
    }
}

impl SchemaSource for KubernetesSchemas {
    fn export(&self, request: &SchemaRequest, log: &dyn Log) -> Result<SchemaExport, RunError> {
        let cluster = cluster_in(&request.graphs);
        let version = self
            .kubectl(&cluster, &["version", "--output", "json"])
            .run_ok("kubectl version failed", log)?;
        let versions = decode_value(version.stdout.as_bytes())
            .map_err(|e| RunError::new(format!("kubectl's version could not be read: {e}")))?;
        let git_version = |side: &str| {
            versions
                .as_map()
                .and_then(|v| v.get(side))
                .and_then(Value::as_map)
                .and_then(|v| v.get("gitVersion"))
                .and_then(Value::as_str)
                .and_then(version_in)
                .unwrap_or_default()
        };
        let mut export = SchemaExport::new("kubectl", &git_version("clientVersion"));
        let server = git_version("serverVersion");
        if !server.is_empty() {
            export.pin("server", &server);
        }
        if !cluster.context.is_empty() {
            export.pin("context", &cluster.context);
        }

        let document = self
            .kubectl(&cluster, &["get", "--raw", OPENAPI_PATH])
            .run_ok(
                "kubectl could not read the API server's OpenAPI document",
                log,
            )?;
        let document = decode_value(document.stdout.as_bytes())
            .map_err(|e| RunError::new(format!("the OpenAPI document could not be read: {e}")))?;
        let read = read_openapi(&document, &request.types);
        export.schemas = read.schemas;
        if read.named > 0 {
            export.note(format!(
                "{} listed by name only; a kind is described in full once a document uses it \
                 and the schemas are synced again",
                count(read.named, "other kind")
            ));
        }
        for wanted in &request.types {
            if wanted != CLUSTER_TYPE && !export.schemas.iter().any(|s| &s.r#type == wanted) {
                export.note(format!(
                    "the API server does not describe {wanted}; is its API group installed?"
                ));
            }
        }
        export.tidy();
        Ok(export)
    }
}

/// How the documents reach their cluster: the first `cluster` resource's
/// `context` and `kubeconfig`.
#[derive(Debug, Default, PartialEq, Eq)]
struct ClusterSettings {
    context: String,
    kubeconfig: String,
}

fn cluster_in(graphs: &[ResourceGraph]) -> ClusterSettings {
    let mut settings = ClusterSettings::default();
    for resource in graphs.iter().flat_map(|g| g.resources.iter()) {
        if resource.r#type != CLUSTER_TYPE {
            continue;
        }
        let text = |key: &str| {
            resource
                .params
                .get(key)
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_owned()
        };
        settings.context = text("context");
        settings.kubeconfig = text("kubeconfig");
        break;
    }
    settings
}

/// What was read from the OpenAPI document.
#[derive(Debug, Default)]
struct Read {
    schemas: Vec<ResourceSchema>,
    /// How many kinds were listed by name only.
    named: usize,
}

/// Reads an OpenAPI v2 document. Every definition that names a group,
/// version and kind is one resource type; the ones in `full` get their
/// params, the rest their name and description. `*List` kinds, which a
/// document never declares, are left out.
fn read_openapi(document: &Value, full: &[String]) -> Read {
    let mut read = Read::default();
    let Some(definitions) = document
        .as_map()
        .and_then(|d| d.get("definitions"))
        .and_then(Value::as_map)
    else {
        return read;
    };
    let resolve = |reference: &str| -> Option<&Value> {
        reference
            .strip_prefix("#/definitions/")
            .and_then(|name| definitions.get(name))
    };
    for definition in definitions.values() {
        let Some(map) = definition.as_map() else {
            continue;
        };
        let Some(gvks) = map.get(GVK).and_then(Value::as_list) else {
            continue;
        };
        for gvk in gvks {
            let Some(gvk) = gvk.as_map() else {
                continue;
            };
            let text = |key: &str| gvk.get(key).and_then(Value::as_str).unwrap_or("");
            let (group, version, kind) = (text("group"), text("version"), text("kind"));
            if kind.is_empty() || version.is_empty() {
                continue;
            }
            if kind.ends_with("List")
                && map
                    .get("properties")
                    .and_then(Value::as_map)
                    .is_some_and(|p| p.contains_key("items"))
            {
                continue;
            }
            let name = if group.is_empty() {
                format!("{version}/{kind}")
            } else {
                format!("{group}/{version}/{kind}")
            };
            let description = map
                .get("description")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_owned();
            let schema = if full.contains(&name) {
                let (params, open) = params_from_json_schema(definition, &resolve);
                let params = params
                    .into_iter()
                    .filter(|p| p.name != "apiVersion" && p.name != "kind")
                    .map(|mut p| {
                        if p.name == "status" {
                            p = p.computed();
                        }
                        p
                    })
                    .collect::<Vec<ParamSchema>>();
                let mut schema = ResourceSchema::closed(&name, &description).with_params(params);
                schema.open = open;
                schema
            } else {
                read.named += 1;
                ResourceSchema::open(&name, &description)
            };
            read.schemas.push(schema);
        }
    }
    read.schemas.sort_by(|a, b| a.r#type.cmp(&b.r#type));
    read
}

fn count(n: usize, noun: &str) -> String {
    if n == 1 {
        format!("1 {noun}")
    } else {
        format!("{n} {noun}s")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use meridian_sdk::{ParamKind, Resource};

    #[test]
    fn the_cluster_settings_come_from_the_cluster_resource() {
        let mut graph = ResourceGraph::default();
        graph.resources.push(Resource {
            id: "web".to_owned(),
            r#type: "apps/v1/Deployment".to_owned(),
            ..Resource::default()
        });
        graph.resources.push(Resource {
            id: "where".to_owned(),
            r#type: CLUSTER_TYPE.to_owned(),
            params: [
                ("context", "kind-meridian"),
                ("namespace", "web"),
                ("kubeconfig", "/tmp/kc"),
            ]
            .into_iter()
            .map(|(k, v)| (k.to_owned(), Value::String(v.to_owned())))
            .collect(),
            ..Resource::default()
        });
        assert_eq!(
            cluster_in(&[graph]),
            ClusterSettings {
                context: "kind-meridian".to_owned(),
                kubeconfig: "/tmp/kc".to_owned(),
            }
        );
        assert_eq!(cluster_in(&[]), ClusterSettings::default());
    }

    #[test]
    fn the_openapi_definitions_become_resource_schemas() {
        let json = r##"{"swagger":"2.0","definitions":{
            "io.k8s.api.apps.v1.Deployment":{"description":"Deployment enables declarative updates.","type":"object",
                "properties":{
                    "apiVersion":{"type":"string"},"kind":{"type":"string"},
                    "metadata":{"$ref":"#/definitions/io.k8s.apimachinery.pkg.apis.meta.v1.ObjectMeta"},
                    "spec":{"$ref":"#/definitions/io.k8s.api.apps.v1.DeploymentSpec"},
                    "status":{"$ref":"#/definitions/io.k8s.api.apps.v1.DeploymentStatus"}},
                "x-kubernetes-group-version-kind":[{"group":"apps","kind":"Deployment","version":"v1"}]},
            "io.k8s.api.apps.v1.DeploymentList":{"type":"object","properties":{"items":{"type":"array"}},
                "x-kubernetes-group-version-kind":[{"group":"apps","kind":"DeploymentList","version":"v1"}]},
            "io.k8s.api.apps.v1.DeploymentSpec":{"type":"object","required":["selector","template"],
                "properties":{
                    "replicas":{"type":"integer","format":"int32","description":"Number of desired pods."},
                    "selector":{"type":"object","properties":{"matchLabels":{"type":"object","additionalProperties":{"type":"string"}}}},
                    "template":{"type":"object"}}},
            "io.k8s.api.apps.v1.DeploymentStatus":{"type":"object","properties":{"replicas":{"type":"integer"}}},
            "io.k8s.apimachinery.pkg.apis.meta.v1.ObjectMeta":{"type":"object","properties":{"name":{"type":"string"},"labels":{"type":"object","additionalProperties":{"type":"string"}}}},
            "io.k8s.api.core.v1.Service":{"description":"Service is a named abstraction.","type":"object",
                "properties":{"apiVersion":{"type":"string"},"kind":{"type":"string"},"spec":{"type":"object"}},
                "x-kubernetes-group-version-kind":[{"group":"","kind":"Service","version":"v1"}]},
            "io.k8s.api.core.v1.PodSpec":{"type":"object","properties":{"containers":{"type":"array"}}}
        }}"##;
        let document = decode_value(json.as_bytes()).unwrap();
        let read = read_openapi(&document, &["apps/v1/Deployment".to_owned()]);
        let names: Vec<&str> = read.schemas.iter().map(|s| s.r#type.as_str()).collect();
        assert_eq!(
            names,
            ["apps/v1/Deployment", "v1/Service"],
            "no list, no plain object"
        );
        assert_eq!(read.named, 1);

        let deployment = &read.schemas[0];
        assert_eq!(
            deployment.description,
            "Deployment enables declarative updates."
        );
        assert!(!deployment.open);
        let param_names: Vec<&str> = deployment.params.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(param_names, ["metadata", "spec", "status"]);
        let spec = deployment.find_param("spec").unwrap();
        assert_eq!(spec.kind, ParamKind::Map);
        let replicas = spec.find_param("replicas").unwrap();
        assert_eq!(replicas.kind, ParamKind::Integer);
        assert_eq!(replicas.description, "Number of desired pods.");
        assert!(spec.find_param("selector").unwrap().required);
        assert!(
            spec.find_param("selector")
                .unwrap()
                .find_param("matchLabels")
                .unwrap()
                .open
        );
        assert!(deployment.find_param("status").unwrap().computed);
        let metadata = deployment.find_param("metadata").unwrap();
        assert!(
            metadata.find_param("name").is_some(),
            "the $ref was followed"
        );

        let service = &read.schemas[1];
        assert!(service.open && service.params.is_empty());
        assert_eq!(service.description, "Service is a named abstraction.");
    }
}
