//! Stage one: each resource becomes a template of the chart or one of its
//! settings, and every reference becomes the template expression that reads
//! it.

use std::collections::BTreeMap;

use meridian_sdk::expr::{self, Part};
use meridian_sdk::rules;
use meridian_sdk::{CompileError, Resource, ResourceGraph, Value};
use meridian_yaml::Yaml;

use crate::path::{self, Step};
use crate::template::{self, Piece, Scalar};
use crate::{
    plain, refuse, refuse_at, Chart, ChartSettings, Kustomization, Manifest, Node, OutputPart,
    OutputSpec, ReleaseSettings,
};

/// Opens the stand-in the serialiser writes where a template action goes. An
/// author's own text may not contain it, so the two can never be confused.
pub(crate) const ACTION_MARK: &str = "__meridian_action_";

/// The fields of a kustomization that Meridian writes itself or that name
/// files outside the artifact, which a compiled overlay cannot carry.
const KUSTOMIZATION_REFUSED: &[&str] = &[
    "bases",
    "components",
    "configurations",
    "crds",
    "generators",
    "helmCharts",
    "helmGlobals",
    "openapi",
    "resources",
    "transformers",
    "validators",
];

/// The resources that hold settings, by their type.
#[derive(Default)]
struct SettingsOf<'a> {
    chart: Option<&'a Resource>,
    kustomization: Option<&'a Resource>,
    release: Option<&'a Resource>,
    values: Option<&'a Resource>,
}

/// What a reference can be resolved against.
struct Scope<'a> {
    settings: SettingsOf<'a>,
    values: Value,
    chart: &'a ChartSettings,
    release: &'a ReleaseSettings,
    /// Every object's kind, by id.
    kinds: BTreeMap<&'a str, &'a str>,
    /// Every object's name, by id. `None` while the names themselves are
    /// being read.
    names: Option<BTreeMap<&'a str, Vec<Piece>>>,
    /// Every object's namespace, by id: its own, or the release's.
    namespaces: BTreeMap<&'a str, Vec<Piece>>,
}

/// What a reference turned out to point at.
enum Found {
    /// Something the chart can read while it renders.
    Pieces(Vec<Piece>),
    /// An object of the document, and a path into it.
    Object { id: String, steps: Vec<Step> },
}

pub(crate) fn transform(graph: &ResourceGraph) -> Result<Chart, CompileError> {
    let mut chart = Chart {
        document: graph.name.clone(),
        chart: ChartSettings {
            name: graph.name.clone(),
            version: "0.1.0".to_owned(),
            description: format!("Compiled by Meridian from the document {}.", graph.name),
            ..ChartSettings::default()
        },
        release: ReleaseSettings {
            name: graph.name.clone(),
            namespace: "default".to_owned(),
            ..ReleaseSettings::default()
        },
        ..Chart::default()
    };

    let mut settings = SettingsOf::default();
    let mut objects: Vec<(&Resource, String, String)> = Vec::new();
    for r in &graph.resources {
        check_meta(r)?;
        let slot = match r.r#type.as_str() {
            "chart" => &mut settings.chart,
            "kustomization" => &mut settings.kustomization,
            "release" => &mut settings.release,
            "values" => &mut settings.values,
            other => {
                let Some((api_version, kind)) = object_type(other) else {
                    return Err(refuse(
                        r,
                        &rules::UNSUPPORTED_RESOURCE_TYPE,
                        format!(
                            "{other:?} is not a Kubernetes object type. Write the object's \
                             apiVersion and kind joined by a slash, such as apps/v1/Deployment \
                             or v1/Service, or use one of the settings types: chart, \
                             kustomization, release, values"
                        ),
                    ));
                };
                check_id(r)?;
                objects.push((r, api_version.to_owned(), kind.to_owned()));
                continue;
            }
        };
        if slot.is_some() {
            return Err(refuse(
                r,
                &rules::DUPLICATE_RESOURCE,
                format!(
                    "a document has one resource of type {:?}; this is the second",
                    r.r#type
                ),
            ));
        }
        *slot = Some(r);
    }

    if let Some(r) = settings.values {
        chart.values = r
            .params
            .iter()
            .map(|(name, value)| Ok((name.clone(), literal_value(value)?)))
            .collect::<Result<_, String>>()
            .map_err(|msg| refuse(r, &rules::INVALID_REFERENCE, msg))?;
    }
    if let Some(r) = settings.chart {
        read_chart(r, &mut chart.chart)?;
    }
    if let Some(r) = settings.release {
        read_release(r, &mut chart.release)?;
    }

    let mut scope = Scope {
        settings,
        values: Value::Map(chart.values.clone()),
        chart: &chart.chart,
        release: &chart.release,
        kinds: objects
            .iter()
            .map(|(r, _, kind)| (r.id.as_str(), kind.as_str()))
            .collect(),
        names: None,
        namespaces: BTreeMap::new(),
    };

    // Names and namespaces first: they are what a reference to an object can
    // read.
    let mut names = BTreeMap::new();
    let mut namespaces = BTreeMap::new();
    for (r, _, _) in &objects {
        names.insert(r.id.as_str(), name_of(r, &scope)?);
        namespaces.insert(r.id.as_str(), namespace_of(r, &scope)?);
    }
    scope.names = Some(names);
    scope.namespaces = namespaces;

    let mut manifests = Vec::with_capacity(objects.len());
    for (r, api_version, kind) in &objects {
        manifests.push(manifest(r, api_version, kind, &scope)?);
    }
    let kustomization = scope
        .settings
        .kustomization
        .map(|r| read_kustomization(r, &scope))
        .transpose()?;

    let mut outputs = Vec::with_capacity(graph.outputs.len());
    for output in &graph.outputs {
        let spec = read_output(&output.name, &output.value, &scope, kustomization.as_ref())
            .map_err(|msg| {
                refuse_at(
                    "",
                    &output.pos,
                    &rules::INVALID_REFERENCE,
                    format!("output {:?}: {msg}", output.name),
                )
            })?;
        outputs.push(spec);
    }

    chart.manifests = manifests;
    chart.kustomization = kustomization;
    chart.outputs = outputs;
    Ok(chart)
}

fn check_meta(r: &Resource) -> Result<(), CompileError> {
    if !r.runtime_when.is_empty() {
        return Err(refuse(
            r,
            &rules::UNSUPPORTED_RUNTIME_CONDITION,
            format!(
                "resource carries runtimeWhen {:?}, which this target cannot express: a chart \
                 is rendered from its values before anything reaches the cluster, not from \
                 the state of the cluster",
                r.runtime_when
            ),
        ));
    }
    match r.state.as_str() {
        "" | "present" => Ok(()),
        "absent" => Err(refuse(
            r,
            &rules::UNSUPPORTED_STATE,
            "state \"absent\" has no meaning on this target: Helm deletes whatever its chart \
             no longer declares, so remove the resource from the document"
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

/// Splits `apps/v1/Deployment` into `apps/v1` and `Deployment`.
fn object_type(t: &str) -> Option<(&str, &str)> {
    let (api_version, kind) = t.rsplit_once('/')?;
    let kind_ok = kind.starts_with(|c: char| c.is_ascii_uppercase())
        && kind.chars().all(|c| c.is_ascii_alphanumeric());
    let version_ok =
        |v: &str| v.starts_with('v') && v.len() > 1 && v.chars().all(|c| c.is_ascii_alphanumeric());
    let api_ok = match api_version.split_once('/') {
        None => version_ok(api_version),
        Some((group, version)) => {
            !group.is_empty()
                && group
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '.' || c == '-')
                && version_ok(version)
        }
    };
    (kind_ok && api_ok).then_some((api_version, kind))
}

/// An object's id names its template file and, by default, the object.
fn check_id(r: &Resource) -> Result<(), CompileError> {
    let ok = r.id.len() <= 63
        && r.id.starts_with(|c: char| c.is_ascii_alphanumeric())
        && r.id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-');
    if ok {
        return Ok(());
    }
    Err(refuse(
        r,
        &rules::INVALID_NAME,
        format!(
            "resource id {:?} cannot name a template of the chart: an id starts with a letter \
             or a digit, holds only letters, digits, underscores and hyphens, and is at most \
             63 characters",
            r.id
        ),
    ))
}

/// A value that must hold no reference, with `$${` turned back into `${`.
fn literal_value(value: &Value) -> Result<Value, String> {
    Ok(match value {
        Value::String(s) => Value::String(literal_text(s)?),
        Value::List(items) => Value::List(
            items
                .iter()
                .map(literal_value)
                .collect::<Result<_, String>>()?,
        ),
        Value::Map(map) => Value::Map(
            map.iter()
                .map(|(k, v)| Ok((literal_text(k)?, literal_value(v)?)))
                .collect::<Result<_, String>>()?,
        ),
        other => other.clone(),
    })
}

fn literal_text(text: &str) -> Result<String, String> {
    let mut out = String::new();
    for part in expr::parse(text)? {
        match part {
            Part::Text(literal) => out.push_str(&literal),
            Part::Expr(expression) => {
                return Err(format!(
                    "${{{expression}}} cannot be used here: settings and values are literal, \
                     and only an object or an output can refer to something"
                ))
            }
        }
    }
    Ok(out)
}

/// The params of a settings resource, read by name and type.
struct Params<'a> {
    r: &'a Resource,
    /// How a message names the resource: "the chart".
    what: &'static str,
    accepted: &'static [&'static str],
}

impl Params<'_> {
    fn check(&self) -> Result<(), CompileError> {
        match self
            .r
            .params
            .keys()
            .find(|name| !self.accepted.contains(&name.as_str()))
        {
            None => Ok(()),
            Some(name) => Err(refuse(
                self.r,
                &rules::UNSUPPORTED_PARAM,
                format!(
                    "resource type {:?} does not accept param {name:?} on this target (accepted: {})",
                    self.r.r#type,
                    self.accepted.join(", ")
                ),
            )),
        }
    }

    fn text(&self, name: &str) -> Result<Option<String>, CompileError> {
        match self.r.params.get(name) {
            None => Ok(None),
            Some(Value::String(s)) => literal_text(s).map(Some).map_err(|msg| {
                refuse(
                    self.r,
                    &rules::INVALID_REFERENCE,
                    format!("param {name:?}: {msg}"),
                )
            }),
            Some(other) => Err(refuse(
                self.r,
                &rules::INVALID_PARAM,
                format!(
                    "param {name:?} of {} must be a string, and this is {}{}",
                    self.what,
                    other.describe(),
                    if matches!(other, Value::Int(_) | Value::Uint(_) | Value::Float(_)) {
                        "; put it in quotes"
                    } else {
                        ""
                    }
                ),
            )),
        }
    }

    fn flag(&self, name: &str) -> Result<Option<bool>, CompileError> {
        match self.r.params.get(name) {
            None => Ok(None),
            Some(Value::Bool(b)) => Ok(Some(*b)),
            Some(other) => Err(refuse(
                self.r,
                &rules::INVALID_PARAM,
                format!(
                    "param {name:?} of {} must be true or false, and this is {}",
                    self.what,
                    other.describe()
                ),
            )),
        }
    }
}

fn read_chart(r: &Resource, chart: &mut ChartSettings) -> Result<(), CompileError> {
    let params = Params {
        r,
        what: "the chart",
        accepted: &["appVersion", "description", "name", "version"],
    };
    params.check()?;
    if let Some(name) = params.text("name")? {
        chart.name = name;
    }
    if let Some(version) = params.text("version")? {
        chart.version = version;
    }
    if let Some(app_version) = params.text("appVersion")? {
        chart.app_version = app_version;
    }
    if let Some(description) = params.text("description")? {
        chart.description = description;
    }
    Ok(())
}

fn read_release(r: &Resource, release: &mut ReleaseSettings) -> Result<(), CompileError> {
    let params = Params {
        r,
        what: "the release",
        accepted: &[
            "atomic",
            "createNamespace",
            "name",
            "namespace",
            "ordering",
            "timeout",
            "wait",
        ],
    };
    params.check()?;
    if let Some(name) = params.text("name")? {
        release.name = name;
    }
    if let Some(namespace) = params.text("namespace")? {
        release.namespace = namespace;
    }
    if let Some(timeout) = params.text("timeout")? {
        release.timeout = timeout;
    }
    release.create_namespace = params.flag("createNamespace")?.unwrap_or(false);
    release.wait = params.flag("wait")?.unwrap_or(false);
    release.atomic = params.flag("atomic")?.unwrap_or(false);
    match params.text("ordering")?.as_deref() {
        None | Some("kind") => {}
        Some("releases") => release.ordered_releases = true,
        Some(other) => {
            return Err(refuse(
                r,
                &rules::INVALID_PARAM,
                format!(
                    "param \"ordering\" of the release is {other:?}; it is \"kind\" (Helm's \
                     own order, the default) or \"releases\" (one release per dependsOn wave)"
                ),
            ))
        }
    }
    Ok(())
}

impl Scope<'_> {
    /// Resolves what stands between `${` and `}`.
    fn find(&self, expression: &str) -> Result<Found, String> {
        let (name, rest) = expr::head(expression);
        if name.is_empty() {
            return Err(format!(
                "${{{expression}}} is not a reference this target can read: write ${{id}} for \
                 an object's name or ${{id.path}} for a value"
            ));
        }
        let steps = path::parse(rest).map_err(|why| format!("${{{expression}}}: {why}"))?;
        let is = |r: Option<&Resource>| r.is_some_and(|r| r.id == name);
        let text = |go: &str, now: &str| {
            Ok(Found::Pieces(vec![Piece::Expr {
                go: go.to_owned(),
                now: Value::String(now.to_owned()),
            }]))
        };

        if is(self.settings.values) {
            let now = path::lookup(&self.values, &steps)
                .map_err(|missing| format!("${{{expression}}}: the values have no {missing}"))?;
            return Ok(Found::Pieces(vec![Piece::Expr {
                go: template::go_path(".Values", &steps),
                now: now.clone(),
            }]));
        }
        if is(self.settings.release) {
            return match steps.as_slice() {
                [Step::Key(key)] if key == "name" => text(".Release.Name", &self.release.name),
                [Step::Key(key)] if key == "namespace" => {
                    text(".Release.Namespace", &self.release.namespace)
                }
                _ => Err(format!(
                    "${{{expression}}} is not something a chart can read from its release \
                     (readable: name, namespace)"
                )),
            };
        }
        if is(self.settings.chart) {
            return match steps.as_slice() {
                [Step::Key(key)] if key == "name" => text(".Chart.Name", &self.chart.name),
                [Step::Key(key)] if key == "version" => text(".Chart.Version", &self.chart.version),
                [Step::Key(key)] if key == "appVersion" && !self.chart.app_version.is_empty() => {
                    text(".Chart.AppVersion", &self.chart.app_version)
                }
                [Step::Key(key)] if key == "appVersion" => {
                    Err(format!("${{{expression}}}: the chart sets no appVersion"))
                }
                _ => Err(format!(
                    "${{{expression}}} is not something a chart can read about itself \
                     (readable: name, version, appVersion)"
                )),
            };
        }
        if is(self.settings.kustomization) {
            return Err(format!(
                "${{{expression}}} refers to the Kustomize overlay, which holds nothing to read"
            ));
        }
        if self.kinds.contains_key(name) {
            return Ok(Found::Object {
                id: name.to_owned(),
                steps,
            });
        }
        Err(format!(
            "${{{expression}}} refers to {name:?}, which is not a resource in this document"
        ))
    }

    /// Resolves the references of a string that a template or the overlay
    /// holds. There, a reference to an object can only be to its name.
    fn pieces(&self, text: &str) -> Result<Vec<Piece>, String> {
        let mut pieces: Vec<Piece> = Vec::new();
        let mut push = |piece: Piece| match (pieces.last_mut(), piece) {
            (Some(Piece::Text(last)), Piece::Text(more)) => last.push_str(&more),
            (_, piece) => pieces.push(piece),
        };
        for part in expr::parse(text)? {
            let expression = match part {
                Part::Text(literal) => {
                    push(Piece::Text(literal));
                    continue;
                }
                Part::Expr(expression) => expression,
            };
            match self.find(&expression)? {
                Found::Pieces(found) => found.into_iter().for_each(&mut push),
                Found::Object { id, steps } => {
                    if is_namespace(&steps) {
                        if let Some(namespace) = self.namespaces.get(id.as_str()) {
                            namespace.iter().cloned().for_each(&mut push);
                            continue;
                        }
                    }
                    if !is_name(&steps) {
                        return Err(format!(
                            "${{{expression}}} reads {} of {id:?}, which only the cluster knows \
                             once the object is applied, so a template cannot hold it. Read it \
                             in an output instead. A template can read ${{{id}}}, the object's \
                             name, and ${{{id}.metadata.namespace}}",
                            path::show(&steps)
                        ));
                    }
                    let Some(names) = &self.names else {
                        return Err(format!(
                            "${{{expression}}}: an object's name and namespace cannot be built \
                             from another object's"
                        ));
                    };
                    names[id.as_str()].iter().cloned().for_each(&mut push);
                }
            }
        }
        Ok(pieces)
    }

    /// A string of a template.
    fn scalar(&self, text: &str) -> Result<Node, String> {
        let pieces = self.pieces(text)?;
        scalar_node(&pieces, text)
    }

    /// A value of a template.
    fn node(&self, value: &Value) -> Result<Node, String> {
        Ok(match value {
            Value::String(s) => self.scalar(s)?,
            Value::List(items) => Node::Seq(
                items
                    .iter()
                    .map(|item| self.node(item))
                    .collect::<Result<_, String>>()?,
            ),
            Value::Map(map) => Node::Map(
                map.iter()
                    .map(|(k, v)| Ok((key(k)?, self.node(v)?)))
                    .collect::<Result<_, String>>()?,
            ),
            other => Node::Plain(plain(other)),
        })
    }

    /// A string as it reads with the document's own values, for the places
    /// that are not templates.
    fn now(&self, text: &str) -> Result<String, String> {
        template::render_now(&self.pieces(text)?).map_err(|kind| not_text(text, kind))
    }

    /// A value with every reference replaced by what it reads now.
    fn now_yaml(&self, value: &Value) -> Result<Yaml, String> {
        Ok(match value {
            Value::String(s) => Yaml::str(self.now(s)?),
            Value::List(items) => Yaml::Seq(
                items
                    .iter()
                    .map(|item| self.now_yaml(item))
                    .collect::<Result<_, String>>()?,
            ),
            Value::Map(map) => Yaml::Map(
                map.iter()
                    .map(|(k, v)| Ok((key(k)?, self.now_yaml(v)?)))
                    .collect::<Result<_, String>>()?,
            ),
            other => plain(other),
        })
    }
}

/// Whether a path into an object reads nothing but its name.
fn is_name(steps: &[Step]) -> bool {
    match steps {
        [] => true,
        [Step::Key(a), Step::Key(b)] => a == "metadata" && b == "name",
        _ => false,
    }
}

/// Whether a path into an object reads its namespace.
fn is_namespace(steps: &[Step]) -> bool {
    matches!(steps, [Step::Key(a), Step::Key(b)] if a == "metadata" && b == "namespace")
}

fn not_text(text: &str, kind: &str) -> String {
    format!(
        "{text:?} puts {kind} inside text; only a string, a number or a boolean can be part \
         of text"
    )
}

fn scalar_node(pieces: &[Piece], text: &str) -> Result<Node, String> {
    match template::scalar(pieces).map_err(|kind| not_text(text, kind))? {
        Scalar::Text(literal) => {
            reserved(&literal)?;
            Ok(Node::Text(literal))
        }
        Scalar::Action(action) => Ok(Node::Action(action)),
    }
}

/// A mapping key: literal, and never a reference.
fn key(text: &str) -> Result<String, String> {
    let mut out = String::new();
    for part in expr::parse(text)? {
        match part {
            Part::Text(literal) => out.push_str(&literal),
            Part::Expr(_) => {
                return Err(format!(
                    "the key {text:?} holds a reference; only a value can"
                ))
            }
        }
    }
    reserved(&out)?;
    Ok(out)
}

fn reserved(text: &str) -> Result<(), String> {
    if text.contains(ACTION_MARK) {
        return Err(format!(
            "{text:?} contains {ACTION_MARK:?}, which this target reserves for itself"
        ));
    }
    Ok(())
}

/// The `metadata` param of an object, which is optional but must be a
/// mapping.
fn metadata(r: &Resource) -> Result<Option<&BTreeMap<String, Value>>, CompileError> {
    match r.params.get("metadata") {
        None => Ok(None),
        Some(Value::Map(map)) => Ok(Some(map)),
        Some(other) => Err(refuse(
            r,
            &rules::INVALID_PARAM,
            format!(
                "param \"metadata\" must be a mapping, and this is {}",
                other.describe()
            ),
        )),
    }
}

/// One string field of an object's metadata.
fn metadata_text<'a>(r: &'a Resource, field: &str) -> Result<Option<&'a str>, CompileError> {
    match metadata(r)?.and_then(|m| m.get(field)) {
        None => Ok(None),
        Some(Value::String(s)) => Ok(Some(s)),
        Some(other) => Err(refuse(
            r,
            &rules::INVALID_PARAM,
            format!(
                "param \"metadata.{field}\" must be a string, and this is {}",
                other.describe()
            ),
        )),
    }
}

/// The name an object is given when its manifest does not say: its id, in the
/// form Kubernetes accepts.
pub(crate) fn default_name(id: &str) -> String {
    id.replace('_', "-")
}

fn name_of(r: &Resource, scope: &Scope) -> Result<Vec<Piece>, CompileError> {
    match metadata_text(r, "name")? {
        None => Ok(vec![Piece::Text(default_name(&r.id))]),
        Some(written) => scope.pieces(written).map_err(|msg| {
            refuse(
                r,
                &rules::INVALID_REFERENCE,
                format!("param \"metadata.name\": {msg}"),
            )
        }),
    }
}

/// The namespace an object ends up in: the one its manifest names, or the
/// release's.
fn namespace_of(r: &Resource, scope: &Scope) -> Result<Vec<Piece>, CompileError> {
    match metadata_text(r, "namespace")? {
        None => Ok(vec![Piece::Expr {
            go: ".Release.Namespace".to_owned(),
            now: Value::String(scope.release.namespace.clone()),
        }]),
        Some(written) => scope.pieces(written).map_err(|msg| {
            refuse(
                r,
                &rules::INVALID_REFERENCE,
                format!("param \"metadata.namespace\": {msg}"),
            )
        }),
    }
}

fn manifest(
    r: &Resource,
    api_version: &str,
    kind: &str,
    scope: &Scope,
) -> Result<Manifest, CompileError> {
    let bad_reference = |name: &str| {
        let name = name.to_owned();
        move |msg: String| {
            refuse(
                r,
                &rules::INVALID_REFERENCE,
                format!("param {name:?}: {msg}"),
            )
        }
    };
    let names = scope.names.as_ref().expect("the names are read first");
    let name_pieces = &names[r.id.as_str()];
    let written_name = metadata_text(r, "name")?.unwrap_or(&r.id);
    let name = template::render_now(name_pieces)
        .map_err(|kind| not_text(written_name, kind))
        .map_err(bad_reference("metadata.name"))?;
    let namespace = match metadata_text(r, "namespace")? {
        None => String::new(),
        Some(written) => scope
            .now(written)
            .map_err(bad_reference("metadata.namespace"))?,
    };

    let mut meta = vec![(
        "name".to_owned(),
        scalar_node(name_pieces, written_name).map_err(bad_reference("metadata.name"))?,
    )];
    for (field, value) in metadata(r)?.into_iter().flatten() {
        if field != "name" {
            let label = format!("metadata.{field}");
            meta.push((
                key(field).map_err(bad_reference(&label))?,
                scope.node(value).map_err(bad_reference(&label))?,
            ));
        }
    }

    let mut body = vec![("metadata".to_owned(), Node::Map(meta))];
    for (param, value) in &r.params {
        match param.as_str() {
            "metadata" => {}
            "apiVersion" | "kind" => {
                return Err(refuse(
                    r,
                    &rules::INVALID_PARAM,
                    format!(
                        "param {param:?} is not needed: the resource's type, {:?}, already \
                         says it",
                        r.r#type
                    ),
                ))
            }
            _ => body.push((
                key(param).map_err(bad_reference(param))?,
                scope.node(value).map_err(bad_reference(param))?,
            )),
        }
    }

    Ok(Manifest {
        id: r.id.clone(),
        api_version: api_version.to_owned(),
        kind: kind.to_owned(),
        name,
        namespace,
        body,
        pos: r.pos.clone(),
    })
}

fn read_kustomization(r: &Resource, scope: &Scope) -> Result<Kustomization, CompileError> {
    let bad_reference = |name: &str| {
        let name = name.to_owned();
        move |msg: String| {
            refuse(
                r,
                &rules::INVALID_REFERENCE,
                format!("param {name:?}: {msg}"),
            )
        }
    };
    let mut kustomization = Kustomization::default();
    for (name, value) in &r.params {
        if name == "apiVersion" || name == "kind" {
            return Err(refuse(
                r,
                &rules::UNSUPPORTED_PARAM,
                format!("param {name:?} is not needed: Meridian writes it"),
            ));
        }
        if KUSTOMIZATION_REFUSED.contains(&name.as_str()) {
            return Err(refuse(
                r,
                &rules::UNSUPPORTED_PARAM,
                format!(
                    "resource type \"kustomization\" does not accept param {name:?} on this \
                     target: the overlay is applied to what the chart renders and to nothing \
                     else, so it cannot name other files. Declare the objects as resources of \
                     this document"
                ),
            ));
        }
        let moved = match name.as_str() {
            "namePrefix" => Some(&mut kustomization.name_prefix),
            "nameSuffix" => Some(&mut kustomization.name_suffix),
            "namespace" => Some(&mut kustomization.namespace),
            _ => None,
        };
        let field = if let Some(slot) = moved {
            let Value::String(written) = value else {
                return Err(refuse(
                    r,
                    &rules::INVALID_PARAM,
                    format!(
                        "param {name:?} of the overlay must be a string, and this is {}",
                        value.describe()
                    ),
                ));
            };
            *slot = scope.now(written).map_err(bad_reference(name))?;
            Yaml::str(slot.clone())
        } else if name == "patches" {
            patches(r, value, scope)?
        } else {
            scope.now_yaml(value).map_err(bad_reference(name))?
        };
        kustomization.fields.push((name.clone(), field));
    }
    Ok(kustomization)
}

/// The overlay's patches. A patch may be written as the YAML it is, not only
/// as a string holding YAML; Kustomize itself reads only the string.
fn patches(r: &Resource, value: &Value, scope: &Scope) -> Result<Yaml, CompileError> {
    let bad = |msg: String| {
        refuse(
            r,
            &rules::INVALID_REFERENCE,
            format!("param \"patches\": {msg}"),
        )
    };
    let Value::List(items) = value else {
        return Err(refuse(
            r,
            &rules::INVALID_PARAM,
            format!(
                "param \"patches\" of the overlay must be a list, and this is {}",
                value.describe()
            ),
        ));
    };
    let mut out = Vec::with_capacity(items.len());
    for item in items {
        let Value::Map(patch) = item else {
            return Err(refuse(
                r,
                &rules::INVALID_PARAM,
                "every patch of the overlay must be a mapping with a \"patch\" and, usually, a \
                 \"target\""
                    .to_owned(),
            ));
        };
        let mut fields = Vec::with_capacity(patch.len());
        for (name, value) in patch {
            let field = match (name.as_str(), value) {
                ("path", _) => {
                    return Err(refuse(
                        r,
                        &rules::UNSUPPORTED_PARAM,
                        "a patch of the overlay cannot name a file with \"path\": write the \
                         patch itself under \"patch\""
                            .to_owned(),
                    ))
                }
                ("patch", Value::Map(_) | Value::List(_)) => {
                    Yaml::str(meridian_yaml::write(&scope.now_yaml(value).map_err(bad)?))
                }
                _ => scope.now_yaml(value).map_err(bad)?,
            };
            fields.push((name.clone(), field));
        }
        out.push(Yaml::Map(fields));
    }
    Ok(Yaml::Seq(out))
}

fn read_output(
    name: &str,
    value: &str,
    scope: &Scope,
    kustomization: Option<&Kustomization>,
) -> Result<OutputSpec, String> {
    let mut spec = OutputSpec {
        name: name.to_owned(),
        parts: Vec::new(),
        sensitive: false,
    };
    let push = |parts: &mut Vec<OutputPart>, text: String| match parts.last_mut() {
        Some(OutputPart::Text(last)) => last.push_str(&text),
        _ => parts.push(OutputPart::Text(text)),
    };
    for part in expr::parse(value)? {
        let expression = match part {
            Part::Text(literal) => {
                push(&mut spec.parts, literal);
                continue;
            }
            Part::Expr(expression) => expression,
        };
        match scope.find(&expression)? {
            Found::Pieces(pieces) => {
                let text = template::render_now(&pieces).map_err(|kind| not_text(value, kind))?;
                push(&mut spec.parts, text);
            }
            Found::Object { id, steps } if is_name(&steps) => {
                // The name the object has in the cluster, which the overlay
                // may have changed.
                let names = scope.names.as_ref().expect("the names are read first");
                let written = template::render_now(&names[id.as_str()])
                    .map_err(|kind| not_text(value, kind))?;
                let (prefix, suffix) = kustomization
                    .map(|k| (k.name_prefix.as_str(), k.name_suffix.as_str()))
                    .unwrap_or_default();
                push(&mut spec.parts, format!("{prefix}{written}{suffix}"));
            }
            Found::Object { id, steps } => {
                spec.sensitive |= scope.kinds[id.as_str()] == "Secret";
                spec.parts.push(OutputPart::Lookup {
                    resource: id,
                    path: steps,
                });
            }
        }
    }
    Ok(spec)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_object_type_is_an_api_version_and_a_kind() {
        assert_eq!(
            object_type("apps/v1/Deployment"),
            Some(("apps/v1", "Deployment"))
        );
        assert_eq!(object_type("v1/Service"), Some(("v1", "Service")));
        assert_eq!(
            object_type("networking.k8s.io/v1/Ingress"),
            Some(("networking.k8s.io/v1", "Ingress"))
        );
        assert_eq!(
            object_type("cert-manager.io/v1alpha2/Certificate"),
            Some(("cert-manager.io/v1alpha2", "Certificate"))
        );
        for bad in [
            "Deployment",
            "apps/v1/deployment",
            "apps/Deployment",
            "v1/",
            "/v1/Service",
            "Apps/v1/Deployment",
            "a/b/v1/Thing",
            "package",
            "AWS::EC2::Instance",
        ] {
            assert_eq!(object_type(bad), None, "{bad}");
        }
    }

    #[test]
    fn an_id_becomes_a_name_kubernetes_accepts() {
        assert_eq!(default_name("web_config"), "web-config");
        assert_eq!(default_name("web-svc2"), "web-svc2");
    }

    #[test]
    fn a_literal_keeps_an_escaped_dollar_and_refuses_a_reference() {
        assert_eq!(literal_text("cost $${5}").unwrap(), "cost ${5}");
        assert!(literal_text("${values.x}")
            .unwrap_err()
            .contains("settings and values are literal"));
    }
}
