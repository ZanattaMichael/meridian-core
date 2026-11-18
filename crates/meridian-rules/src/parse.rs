//! Reading a rules file. It is read strictly: an unknown field, a rule that
//! names a group nobody declared, a pattern that cannot match: each is an
//! error, because a rule that silently does nothing is worse than no rule.

use std::collections::{BTreeMap, BTreeSet};

use crate::model::{
    shipped, Check, DefaultAccess, Group, Rule, Rules, COMMAND_LINE, DEFAULT_USE, SHIPPED,
    UNCLAIMED,
};
use crate::pattern::{KeyPattern, PathGlob};
use crate::{Error, Kind};
use meridian_ir::Value;

/// The only version of the file format there is.
pub const RULES_VERSION: i64 = 1;

impl Rules {
    /// Parses a rules file (YAML or JSON). `name` is used only in messages.
    pub fn parse(name: &str, data: &[u8]) -> Result<Self, Error> {
        let value = meridian_ir::parse_value(name, data)
            .map_err(|e| Error::new(Kind::File, e.to_string()))?;
        from_value(&value).map_err(|e| Error::new(Kind::File, format!("{name}: {e}")))
    }
}

fn from_value(value: &Value) -> Result<Rules, String> {
    let top = fields(
        value,
        "rules file",
        &["rulesVersion", "default", "layers", "use", "rules"],
    )?;
    match top.get("rulesVersion") {
        Some(Value::Int(RULES_VERSION)) => {}
        Some(other) => {
            return Err(format!(
                "rulesVersion: this Meridian reads version {RULES_VERSION}, found {}",
                other.canonical()
            ))
        }
        None => {
            return Err(format!(
                "rulesVersion: required (write `rulesVersion: {RULES_VERSION}`)"
            ))
        }
    }

    let default = match top.get("default") {
        None | Some(Value::Null) => DefaultAccess::Allow,
        Some(Value::String(s)) if s == "allow" => DefaultAccess::Allow,
        Some(Value::String(s)) if s == "deny" => DefaultAccess::Deny,
        Some(other) => {
            return Err(format!(
                "default: expected \"allow\" or \"deny\", found {}",
                other.canonical()
            ))
        }
    };
    let mut rules = Rules {
        default,
        ..Rules::default()
    };
    for (i, item) in list(top.get("layers"), "layers")?.iter().enumerate() {
        let at = format!("layers[{i}]");
        let map = fields(item, &at, &["name", "paths", "capabilities"])?;
        let name = required_string(map, "name", &at)?;
        if name.is_empty() || name.starts_with('(') {
            return Err(format!(
                "{at}.name: {name:?} is not a usable group name (names in brackets are Meridian's own)"
            ));
        }
        if rules.groups.iter().any(|g| g.name == name) {
            return Err(format!("{at}.name: the group {name:?} is declared twice"));
        }
        let paths = strings(map.get("paths"), &format!("{at}.paths"))?;
        if paths.is_empty() {
            return Err(format!("{at}.paths: the group {name:?} claims no layers"));
        }
        let paths = paths
            .iter()
            .map(|p| PathGlob::parse(p).map_err(|e| format!("{at}.paths: {e}")))
            .collect::<Result<_, _>>()?;
        let keys = match map.get("capabilities") {
            None | Some(Value::Null) => None,
            Some(value) => {
                let at = format!("{at}.capabilities");
                let caps = fields(value, &at, &["keys"])?;
                match caps.get("keys") {
                    None | Some(Value::Null) => None,
                    // An empty list is meaningful: the group may set nothing.
                    Some(value) => Some(
                        strings(Some(value), &format!("{at}.keys"))?
                            .iter()
                            .map(|k| KeyPattern::parse(k).map_err(|e| format!("{at}.keys: {e}")))
                            .collect::<Result<_, _>>()?,
                    ),
                }
            }
        };
        rules.groups.push(Group { name, paths, keys });
    }

    if rules.default == DefaultAccess::Deny && rules.groups.is_empty() {
        return Err(
            "default: deny needs groups under `layers`: with none, no layer could set anything"
                .to_owned(),
        );
    }

    // `use:` left out is the default deny list. Written, even as an empty
    // list, it is exactly the sets in force.
    let used = match top.get("use") {
        None => DEFAULT_USE.iter().map(|n| (*n).to_owned()).collect(),
        value => strings(value, "use")?,
    };
    for (i, name) in used.iter().enumerate() {
        let rule = shipped(name).ok_or_else(|| {
            format!(
                "use[{i}]: there is no shipped rule set {name:?} (there are: {})",
                SHIPPED
                    .iter()
                    .map(|(n, _)| *n)
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        })?;
        if rules.rules.iter().any(|r| r.id == rule.id) {
            return Err(format!("use[{i}]: {name:?} is listed twice"));
        }
        rules.rules.push(rule);
    }

    let groups: BTreeSet<String> = rules.group_names().into_iter().map(str::to_owned).collect();
    for (i, item) in list(top.get("rules"), "rules")?.iter().enumerate() {
        let rule = rule(item, &format!("rules[{i}]"), &groups)?;
        if rules.rules.iter().any(|r| r.id == rule.id) {
            return Err(format!("rules[{i}].id: the id {:?} is used twice", rule.id));
        }
        rules.rules.push(rule);
    }
    Ok(rules)
}

fn rule(value: &Value, at: &str, groups: &BTreeSet<String>) -> Result<Rule, String> {
    const COMMON: &[&str] = &["id", "rule", "keys", "message"];
    let any = fields(
        value,
        at,
        &[
            "id",
            "rule",
            "keys",
            "message",
            "setBy",
            "appliesTo",
            "allow",
            "values",
            "matches",
            "ignoreCase",
        ],
    )?;
    let id = required_string(any, "id", at)?;
    if id.is_empty() || id.starts_with("meridian/") {
        return Err(format!(
            "{at}.id: {id:?} is not a usable id (ids starting `meridian/` are the shipped rules')"
        ));
    }
    let kind = required_string(any, "rule", at)?;
    // Each kind takes its own fields. A field another kind takes is refused
    // here, so `appliesTo` on a rule that would ignore it is not left to
    // look as if it narrowed the rule.
    let own: &[&str] = match kind.as_str() {
        "no-override" | "final" => &["setBy", "appliesTo"],
        "restrict" => &["allow"],
        "required" => &["appliesTo"],
        "forbidden-value" => &["appliesTo", "values", "matches", "ignoreCase"],
        "merge-strategy" => &["allow"],
        other => {
            return Err(format!(
            "{at}.rule: unknown rule {other:?} (expected no-override, final, restrict, required, \
                 forbidden-value or merge-strategy)"
        ))
        }
    };
    if let Some(stray) = any
        .keys()
        .find(|k| !COMMON.contains(&k.as_str()) && !own.contains(&k.as_str()))
    {
        return Err(format!(
            "{at}: a {kind} rule does not take the field {stray:?}"
        ));
    }

    let keys = strings(any.get("keys"), &format!("{at}.keys"))?;
    if keys.is_empty() {
        return Err(format!("{at}.keys: required, and cannot be empty"));
    }
    let keys: Vec<KeyPattern> = keys
        .iter()
        .map(|k| KeyPattern::parse(k).map_err(|e| format!("{at}.keys: {e}")))
        .collect::<Result<_, _>>()?;

    let group_list = |field: &str| -> Result<Option<Vec<String>>, String> {
        let Some(value) = any.get(field).filter(|v| **v != Value::Null) else {
            return Ok(None);
        };
        let names = strings(Some(value), &format!("{at}.{field}"))?;
        for name in &names {
            if !groups.contains(name) && name != COMMAND_LINE && name != UNCLAIMED {
                return Err(format!(
                    "{at}.{field}: there is no group {name:?} under `layers`{}",
                    if groups.is_empty() {
                        " (the file declares none)".to_owned()
                    } else {
                        format!(
                            " (there are: {})",
                            groups.iter().cloned().collect::<Vec<_>>().join(", ")
                        )
                    }
                ));
            }
        }
        Ok(Some(names))
    };

    let check = match kind.as_str() {
        "no-override" => Check::NoOverride,
        "final" => Check::Final,
        "restrict" => Check::Restrict {
            // An empty list is meaningful: nobody may set the key.
            allow: group_list("allow")?
                .ok_or_else(|| format!("{at}.allow: required: the groups that may set the keys"))?,
        },
        "required" => {
            if let Some(k) = keys.iter().find(|k| !k.is_exact()) {
                return Err(format!(
                    "{at}.keys: a required rule names exact keys, and {k} has a wildcard"
                ));
            }
            Check::Required
        }
        "forbidden-value" => {
            let values = list(any.get("values"), &format!("{at}.values"))?.to_vec();
            if let Some(v) = values
                .iter()
                .find(|v| matches!(v, Value::List(_) | Value::Map(_)))
            {
                return Err(format!(
                    "{at}.values: a forbidden value is a scalar, and {} is {}",
                    v.canonical(),
                    v.describe()
                ));
            }
            let matches = strings(any.get("matches"), &format!("{at}.matches"))?;
            if values.is_empty() && matches.is_empty() {
                return Err(format!(
                    "{at}: a forbidden-value rule needs `values` or `matches`"
                ));
            }
            let ignore_case = match any.get("ignoreCase") {
                None | Some(Value::Null) => false,
                Some(Value::Bool(b)) => *b,
                Some(other) => {
                    return Err(format!(
                        "{at}.ignoreCase: expected true or false, found {}",
                        other.describe()
                    ))
                }
            };
            Check::ForbiddenValue {
                values,
                matches,
                ignore_case,
            }
        }
        "merge-strategy" => {
            if let Some(k) = keys
                .iter()
                .find(|k| k.exact_path().is_none_or(|p| p.len() != 1))
            {
                return Err(format!(
                    "{at}.keys: merge strategies belong to top-level keys, so a merge-strategy rule \
                     names them exactly, and {k} is not one"
                ));
            }
            let names = strings(any.get("allow"), &format!("{at}.allow"))?;
            if names.is_empty() {
                return Err(format!(
                    "{at}.allow: required: the strategies the keys may merge with"
                ));
            }
            let allow = names;
            Check::MergeStrategy { allow }
        }
        _ => unreachable!("the kind was checked above"),
    };

    Ok(Rule {
        id,
        keys,
        check,
        set_by: group_list("setBy")?,
        applies_to: group_list("appliesTo")?,
        message: match any.get("message") {
            None | Some(Value::Null) => String::new(),
            Some(Value::String(s)) => s.clone(),
            Some(other) => {
                return Err(format!(
                    "{at}.message: expected a string, found {}",
                    other.describe()
                ))
            }
        },
    })
}

fn fields<'v>(
    value: &'v Value,
    what: &str,
    known: &[&str],
) -> Result<&'v BTreeMap<String, Value>, String> {
    let Value::Map(map) = value else {
        return Err(format!(
            "{what}: expected a mapping, found {}",
            value.describe()
        ));
    };
    match map.keys().find(|k| !known.contains(&k.as_str())) {
        Some(unknown) => Err(format!("{what}: unknown field {unknown:?}")),
        None => Ok(map),
    }
}

fn list<'v>(value: Option<&'v Value>, at: &str) -> Result<&'v [Value], String> {
    match value {
        None | Some(Value::Null) => Ok(&[]),
        Some(Value::List(items)) => Ok(items),
        Some(other) => Err(format!("{at}: expected a list, found {}", other.describe())),
    }
}

fn strings(value: Option<&Value>, at: &str) -> Result<Vec<String>, String> {
    list(value, at)?
        .iter()
        .enumerate()
        .map(|(i, item)| match item {
            Value::String(s) => Ok(s.clone()),
            other => Err(format!(
                "{at}[{i}]: expected a string, found {}",
                other.describe()
            )),
        })
        .collect()
}

fn required_string(map: &BTreeMap<String, Value>, field: &str, at: &str) -> Result<String, String> {
    match map.get(field) {
        Some(Value::String(s)) => Ok(s.clone()),
        Some(other) => Err(format!(
            "{at}.{field}: expected a string, found {}",
            other.describe()
        )),
        None => Err(format!("{at}.{field}: required")),
    }
}
