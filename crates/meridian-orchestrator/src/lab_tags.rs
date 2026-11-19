//! A lab cell's tags on what it makes: every resource whose type takes a
//! `tags` mapping carries `meridian-lab-run`, `meridian-lab-cell` and
//! `meridian-lab-expires`, read from `meridian.lab` in the data. The target
//! writes them through its own tag mechanism, as it does the author's.

use meridian_ir::Data;
use meridian_sdk::infra::{LAB_TAG_CELL, LAB_TAG_EXPIRES, LAB_TAG_RUN};
use meridian_sdk::{ParamKind, ResourceGraph, ResourceSchema, Value};

/// The tags a lab cell's data gives, or none outside a lab.
pub(crate) fn of(data: &Data) -> Vec<(&'static str, String)> {
    let Some(Value::Map(lab)) = data
        .get(meridian_ir::RESERVED_DATA_KEY)
        .and_then(|m| m.as_map())
        .and_then(|m| m.get("lab"))
    else {
        return Vec::new();
    };
    let text = |key: &str| match lab.get(key) {
        Some(Value::String(s)) => Some(s.clone()),
        Some(Value::Uint(n)) => Some(n.to_string()),
        Some(Value::Int(n)) => Some(n.to_string()),
        _ => None,
    };
    [
        (LAB_TAG_RUN, "run"),
        (LAB_TAG_CELL, "cell"),
        (LAB_TAG_EXPIRES, "expires"),
    ]
    .into_iter()
    .filter_map(|(tag, key)| text(key).map(|v| (tag, v)))
    .collect()
}

/// Adds the tags to each resource whose type, as the target describes it,
/// takes a `tags` mapping. An author's own tags are kept; a tag the author
/// gave under one of these names is replaced, as sweep reads it.
pub(crate) fn apply(
    graph: &mut ResourceGraph,
    schemas: &[ResourceSchema],
    tags: &[(&str, String)],
) {
    if tags.is_empty() {
        return;
    }
    let takes_tags = |r#type: &str| {
        schemas.iter().any(|s| {
            s.r#type == r#type
                && s.params
                    .iter()
                    .any(|p| p.name == "tags" && p.kind == ParamKind::Map)
        })
    };
    for resource in &mut graph.resources {
        if !takes_tags(&resource.r#type) {
            continue;
        }
        let entry = resource
            .params
            .entry("tags".to_owned())
            .or_insert_with(|| Value::Map(Default::default()));
        if let Value::Map(map) = entry {
            for (tag, value) in tags {
                map.insert((*tag).to_owned(), Value::String(value.clone()));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use meridian_sdk::{ParamSchema, Resource};
    use std::collections::BTreeMap;

    fn lab_data() -> Data {
        let mut data = Data::new();
        meridian_ir::set_run_id(&mut data, "l0000abcd-2");
        let lab: BTreeMap<String, Value> = [
            ("run".to_owned(), Value::String("l0000abcd".to_owned())),
            ("cell".to_owned(), Value::Uint(2)),
            ("expires".to_owned(), Value::Uint(1_800_000_000)),
        ]
        .into();
        if let Some(Value::Map(m)) = data.get_mut(meridian_ir::RESERVED_DATA_KEY) {
            m.insert("lab".to_owned(), Value::Map(lab));
        }
        data
    }

    fn resource(id: &str, r#type: &str) -> Resource {
        Resource {
            id: id.to_owned(),
            r#type: r#type.to_owned(),
            ..Resource::default()
        }
    }

    #[test]
    fn only_a_type_that_takes_tags_gets_them_and_an_authors_stay() {
        let mut tagged = ResourceSchema::closed("network", "");
        tagged.params = vec![ParamSchema::new("tags", ParamKind::Map, "")];
        let mut graph = ResourceGraph {
            resources: vec![resource("net", "network"), resource("vm", "aws_instance")],
            ..ResourceGraph::default()
        };
        graph.resources[0].params.insert(
            "tags".to_owned(),
            Value::Map([("team".to_owned(), Value::String("web".to_owned()))].into()),
        );
        apply(&mut graph, &[tagged], &of(&lab_data()));
        let tags = graph.resources[0].params["tags"].as_map().unwrap();
        assert_eq!(tags["team"], Value::String("web".to_owned()));
        assert_eq!(tags[LAB_TAG_RUN], Value::String("l0000abcd".to_owned()));
        assert_eq!(tags[LAB_TAG_CELL], Value::String("2".to_owned()));
        assert_eq!(
            tags[LAB_TAG_EXPIRES],
            Value::String("1800000000".to_owned())
        );
        assert!(!graph.resources[1].params.contains_key("tags"));
    }

    #[test]
    fn outside_a_lab_nothing_is_added() {
        let mut data = Data::new();
        meridian_ir::set_run_id(&mut data, "t1");
        assert!(of(&data).is_empty());
        assert!(of(&Data::new()).is_empty());
    }
}
