//! Stage four: a chart becomes its files.

use std::collections::BTreeMap;

use meridian_sdk::Value;
use meridian_yaml::Yaml;

use crate::transform::ACTION_MARK;
use crate::{path, template, values_yaml, Chart, Manifest, Node, OutputPart};

/// The directory of the artifact that holds the chart.
pub const CHART_DIR: &str = "chart";
/// The directory that holds the Kustomize overlay, when the document has one.
pub const KUSTOMIZE_DIR: &str = "kustomize";
/// How to install the chart: the release's name and settings, which object
/// each resource of the document became, and how to read the outputs. The
/// runner reads it; Helm never does.
pub const RELEASE_PATH: &str = "release.json";
/// The file in the overlay's directory that the runner fills with what Helm
/// rendered. The overlay names it as its one resource.
pub(crate) const RENDERED_FILE: &str = "helm-rendered.yaml";

/// Renders the chart. This stage performs no mapping and no checking. Fields
/// are written in sorted order below each object's `metadata`, so the same
/// document always renders to the same bytes.
pub(crate) fn serialise(chart: &Chart) -> BTreeMap<String, String> {
    let mut files = BTreeMap::new();
    let all: Vec<&Manifest> = chart.manifests.iter().collect();
    chart_files(chart, CHART_DIR, &all, &mut files);
    // With ordering: releases, each release's own chart beside the whole one,
    // which a render reads.
    for (n, part) in chart.parts.iter().enumerate() {
        let manifests: Vec<&Manifest> = chart
            .manifests
            .iter()
            .filter(|m| part.contains(&m.id))
            .collect();
        chart_files(chart, &part_dir(n + 1), &manifests, &mut files);
    }
    finish(chart, &mut files);
    files
}

/// The files of one chart in `dir`: its metadata, its values and a template
/// for each of `manifests`.
fn chart_files(
    chart: &Chart,
    dir: &str,
    manifests: &[&Manifest],
    files: &mut BTreeMap<String, String>,
) {
    let mut about = vec![
        ("apiVersion".to_owned(), Yaml::str("v2")),
        ("name".to_owned(), Yaml::str(&chart.chart.name)),
        (
            "description".to_owned(),
            Yaml::str(&chart.chart.description),
        ),
        ("type".to_owned(), Yaml::str("application")),
        ("version".to_owned(), Yaml::str(&chart.chart.version)),
    ];
    if !chart.chart.app_version.is_empty() {
        about.push(("appVersion".to_owned(), Yaml::str(&chart.chart.app_version)));
    }
    files.insert(
        format!("{dir}/Chart.yaml"),
        meridian_yaml::write(&Yaml::Map(about)),
    );
    files.insert(
        format!("{dir}/values.yaml"),
        meridian_yaml::write(&values_yaml(
            &Value::Map(chart.values.clone()),
            &chart.secrets,
        )),
    );
    for manifest in manifests {
        files.insert(
            format!("{dir}/templates/{}.yaml", manifest.id),
            template_text(manifest),
        );
    }
}

/// The chart of ordered release `n`, counted from 1.
pub fn part_dir(n: usize) -> String {
    format!("{CHART_DIR}-{n}")
}

/// The release name of ordered release `n` of `release`.
pub fn part_release(release: &str, n: usize) -> String {
    format!("{release}-{n}")
}

fn finish(chart: &Chart, files: &mut BTreeMap<String, String>) {
    if let Some(kustomization) = &chart.kustomization {
        let mut fields = vec![
            (
                "apiVersion".to_owned(),
                Yaml::str("kustomize.config.k8s.io/v1beta1"),
            ),
            ("kind".to_owned(), Yaml::str("Kustomization")),
            (
                "resources".to_owned(),
                Yaml::Seq(vec![Yaml::str(RENDERED_FILE)]),
            ),
        ];
        fields.extend(kustomization.fields.iter().cloned());
        files.insert(
            format!("{KUSTOMIZE_DIR}/kustomization.yaml"),
            meridian_yaml::write(&Yaml::Map(fields)),
        );
    }

    files.insert(RELEASE_PATH.to_owned(), release(chart).pretty());
}

/// One object as a template: YAML in which a reference is a template action
/// and everything the author wrote is escaped to render as itself.
fn template_text(manifest: &Manifest) -> String {
    let mut actions = Vec::new();
    let mut fields = vec![
        ("apiVersion".to_owned(), Yaml::str(&manifest.api_version)),
        ("kind".to_owned(), Yaml::str(&manifest.kind)),
    ];
    fields.extend(
        manifest
            .body
            .iter()
            .map(|(key, node)| (template::literal(key), yaml(node, &mut actions))),
    );
    let mut text = meridian_yaml::write(&Yaml::Map(fields));
    // An action is not a YAML string and must not be quoted as one, so the
    // writer is given a plain word in its place and the action is put back.
    for (n, action) in actions.iter().enumerate().rev() {
        text = text.replace(&stand_in(n), action);
    }
    text
}

fn stand_in(n: usize) -> String {
    format!("{ACTION_MARK}{n}__")
}

fn yaml(node: &Node, actions: &mut Vec<String>) -> Yaml {
    match node {
        Node::Plain(scalar) => scalar.clone(),
        Node::Text(text) => Yaml::str(template::literal(text)),
        Node::Action(action) => {
            actions.push(action.clone());
            Yaml::str(stand_in(actions.len() - 1))
        }
        Node::Seq(items) => Yaml::Seq(items.iter().map(|item| yaml(item, actions)).collect()),
        Node::Map(fields) => Yaml::Map(
            fields
                .iter()
                .map(|(key, value)| (template::literal(key), yaml(value, actions)))
                .collect(),
        ),
    }
}

fn release(chart: &Chart) -> Value {
    let text = |s: &str| Value::String(s.to_owned());
    let map = |pairs: Vec<(&str, Value)>| {
        Value::Map(pairs.into_iter().map(|(k, v)| (k.to_owned(), v)).collect())
    };
    let overlay = chart.kustomization.as_ref();

    let resources = Value::Map(
        chart
            .manifests
            .iter()
            .map(|m| {
                // Where the object ends up once the overlay has moved it.
                let (name, namespace) = match overlay {
                    Some(k) => (
                        format!("{}{}{}", k.name_prefix, m.name, k.name_suffix),
                        if k.namespace.is_empty() {
                            m.namespace.clone()
                        } else {
                            k.namespace.clone()
                        },
                    ),
                    None => (m.name.clone(), m.namespace.clone()),
                };
                (
                    m.id.clone(),
                    map(vec![
                        ("apiVersion", text(&m.api_version)),
                        ("kind", text(&m.kind)),
                        ("name", text(&name)),
                        ("namespace", text(&namespace)),
                    ]),
                )
            })
            .collect(),
    );
    let outputs = Value::List(
        chart
            .outputs
            .iter()
            .map(|output| {
                let parts = output
                    .parts
                    .iter()
                    .map(|part| match part {
                        OutputPart::Text(literal) => map(vec![("text", text(literal))]),
                        OutputPart::Lookup { resource, path } => map(vec![
                            ("resource", text(resource)),
                            ("path", path::to_value(path)),
                        ]),
                    })
                    .collect();
                map(vec![
                    ("name", text(&output.name)),
                    ("sensitive", Value::Bool(output.sensitive)),
                    ("parts", Value::List(parts)),
                ])
            })
            .collect(),
    );

    let parts: Vec<Value> = (1..=chart.parts.len())
        .map(|n| {
            map(vec![
                ("release", text(&part_release(&chart.release.name, n))),
                ("chart", text(&part_dir(n))),
            ])
        })
        .collect();
    let mut release = vec![
        ("release", text(&chart.release.name)),
        ("namespace", text(&chart.release.namespace)),
        (
            "createNamespace",
            Value::Bool(chart.release.create_namespace),
        ),
        ("wait", Value::Bool(chart.release.wait)),
        ("atomic", Value::Bool(chart.release.atomic)),
        ("timeout", text(&chart.release.timeout)),
        (
            "chart",
            map(vec![
                ("name", text(&chart.chart.name)),
                ("version", text(&chart.chart.version)),
            ]),
        ),
        ("kustomize", Value::Bool(overlay.is_some())),
        ("resources", resources),
        ("outputs", outputs),
    ];
    if !parts.is_empty() {
        release.push(("parts", Value::List(parts)));
    }
    map(release)
}
