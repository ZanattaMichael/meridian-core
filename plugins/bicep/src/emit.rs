//! Stage four: a template becomes its files.

use std::collections::BTreeMap;
use std::fmt::Write as _;

use meridian_sdk::Value;

use crate::{is_identifier, Expr, Piece, Template};

/// The Bicep file.
pub const TEMPLATE_PATH: &str = "main.bicep";
/// Where to deploy it, and the document's id for each Azure resource. The
/// runner reads it; Azure never does.
pub const DEPLOYMENT_PATH: &str = "deployment.json";

/// Renders the template. This stage performs no mapping and no ordering.
pub(crate) fn serialise(template: &Template) -> BTreeMap<String, String> {
    let mut out = format!(
        "// Compiled by Meridian from the document {}. Do not edit.\n",
        template.document
    );
    // A secret's parameter: secure, so Azure keeps its value out of the
    // deployment's record and its logs.
    for parameter in &template.parameters {
        let _ = write!(out, "\n@secure()\nparam {parameter} string\n");
    }
    for statement in &template.statements {
        let mut body = statement.body.clone();
        if !statement.depends_on.is_empty() {
            body.push((
                "dependsOn".to_owned(),
                Expr::Array(
                    statement
                        .depends_on
                        .iter()
                        .map(|symbol| Expr::Raw(symbol.clone()))
                        .collect(),
                ),
            ));
        }
        let _ = write!(
            out,
            "\nresource {} '{}' = ",
            statement.symbol, statement.r#type
        );
        write_expr(&mut out, &Expr::Object(body), 0);
        out.push('\n');
    }
    if !template.outputs.is_empty() {
        out.push('\n');
    }
    for (name, value, _) in &template.outputs {
        let _ = write!(out, "output {name} string = ");
        write_expr(&mut out, value, 0);
        out.push('\n');
    }

    // The Azure name of each resource, lower-cased with its type, so a change
    // Azure reports by resource id can be traced back to the document.
    let resources: BTreeMap<String, Value> = template
        .statements
        .iter()
        .filter_map(|s| {
            let name = s.azure_name.as_ref()?;
            let azure_type = s.r#type.split('@').next().unwrap_or(&s.r#type);
            Some((
                format!("{azure_type}/{name}").to_ascii_lowercase(),
                Value::String(s.symbol.clone()),
            ))
        })
        .collect();
    let deployment = Value::Map(BTreeMap::from([
        (
            "name".to_owned(),
            Value::String(template.deployment.name.clone()),
        ),
        (
            "resourceGroup".to_owned(),
            Value::String(template.deployment.resource_group.clone()),
        ),
        (
            "subscription".to_owned(),
            Value::String(template.deployment.subscription.clone()),
        ),
        ("resources".to_owned(), Value::Map(resources)),
    ]));

    BTreeMap::from([
        (TEMPLATE_PATH.to_owned(), out),
        (DEPLOYMENT_PATH.to_owned(), deployment.pretty()),
    ])
}

fn indent(out: &mut String, depth: usize) {
    for _ in 0..depth {
        out.push_str("  ");
    }
}

fn write_expr(out: &mut String, expr: &Expr, depth: usize) {
    match expr {
        Expr::Raw(raw) => out.push_str(raw),
        Expr::Text(pieces) => write_text(out, pieces),
        Expr::Array(items) if items.is_empty() => out.push_str("[]"),
        Expr::Array(items) => {
            out.push_str("[\n");
            for item in items {
                indent(out, depth + 1);
                write_expr(out, item, depth + 1);
                out.push('\n');
            }
            indent(out, depth);
            out.push(']');
        }
        Expr::Object(entries) if entries.is_empty() => out.push_str("{}"),
        Expr::Object(entries) => {
            out.push_str("{\n");
            for (key, value) in entries {
                indent(out, depth + 1);
                if is_identifier(key) {
                    out.push_str(key);
                } else {
                    write_text(out, &[Piece::Literal(key.clone())]);
                }
                out.push_str(": ");
                write_expr(out, value, depth + 1);
                out.push('\n');
            }
            indent(out, depth);
            out.push('}');
        }
    }
}

/// A single-quoted Bicep string. `${` in literal text is escaped so that only
/// what the document meant as an expression is one.
fn write_text(out: &mut String, pieces: &[Piece]) {
    out.push('\'');
    for piece in pieces {
        match piece {
            Piece::Interpolated(expression) => {
                out.push_str("${");
                out.push_str(expression);
                out.push('}');
            }
            Piece::Literal(text) => {
                let mut chars = text.chars().peekable();
                while let Some(c) = chars.next() {
                    match c {
                        '\\' => out.push_str("\\\\"),
                        '\'' => out.push_str("\\'"),
                        '\n' => out.push_str("\\n"),
                        '\r' => out.push_str("\\r"),
                        '\t' => out.push_str("\\t"),
                        '$' if chars.peek() == Some(&'{') => out.push_str("\\$"),
                        c => out.push(c),
                    }
                }
            }
        }
    }
    out.push('\'');
}
