//! The function library: one set of functions, the same wherever an
//! expression can be written.
//!
//! Every function here is pure and bounded. None reads a file or the network;
//! `env` reads the environment, through the scope when it carries one.

use std::collections::BTreeMap;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

use base64::Engine as _;
use meridian_value::Value;
use sha2::{Digest, Sha256};

use crate::error::{Error, Kind};
use crate::eval::{same, Number};
use crate::Scope;

/// The longest list `range` will build.
const MAX_RANGE: i64 = 10_000;

/// Every function, with the fewest and the most arguments it takes.
const FUNCTIONS: &[(&str, usize, usize)] = &[
    // Presence
    ("defined", 1, 1),
    ("coalesce", 1, usize::MAX),
    ("default", 2, 2),
    ("required", 2, 2),
    // Text
    ("lower", 1, 1),
    ("upper", 1, 1),
    ("title", 1, 1),
    ("trim", 1, 1),
    ("trimPrefix", 2, 2),
    ("trimSuffix", 2, 2),
    ("replace", 3, 3),
    ("split", 2, 2),
    ("join", 2, 2),
    ("format", 1, usize::MAX),
    ("startsWith", 2, 2),
    ("endsWith", 2, 2),
    ("contains", 2, 2),
    ("substr", 2, 3),
    ("indent", 2, 2),
    ("quote", 1, 1),
    ("regexMatch", 2, 2),
    ("regexReplace", 3, 3),
    ("regexFind", 2, 2),
    // Numbers
    ("min", 1, usize::MAX),
    ("max", 1, usize::MAX),
    ("abs", 1, 1),
    ("floor", 1, 1),
    ("ceil", 1, 1),
    ("round", 1, 2),
    ("sum", 1, 1),
    // Lists and mappings
    ("length", 1, 1),
    ("keys", 1, 1),
    ("values", 1, 1),
    ("entries", 1, 1),
    ("merge", 1, usize::MAX),
    ("deepMerge", 1, usize::MAX),
    ("concat", 1, usize::MAX),
    ("flatten", 1, 1),
    ("unique", 1, 1),
    ("sort", 1, 1),
    ("reverse", 1, 1),
    ("first", 1, 1),
    ("last", 1, 1),
    ("slice", 2, 3),
    ("range", 1, 3),
    ("zip", 2, 2),
    ("lookup", 2, 3),
    ("pick", 2, 2),
    ("omit", 2, 2),
    ("any", 1, 1),
    ("all", 1, 1),
    ("map", 3, 3),
    ("filter", 3, 3),
    // Selection
    ("where", 3, 3),
    ("pluck", 2, 2),
    ("groupBy", 2, 2),
    // Types
    ("type", 1, 1),
    ("toString", 1, 1),
    ("toNumber", 1, 1),
    ("toBool", 1, 1),
    ("toJson", 1, 1),
    ("fromJson", 1, 1),
    // Encoding
    ("base64Encode", 1, 1),
    ("base64Decode", 1, 1),
    ("urlEncode", 1, 1),
    ("sha256", 1, 1),
    // Network
    ("cidrHost", 2, 2),
    ("cidrSubnet", 3, 3),
    ("cidrNetmask", 1, 1),
    ("cidrContains", 2, 2),
    // Versions
    ("semverCompare", 2, 2),
    ("semverSatisfies", 2, 2),
    // Paths
    ("basename", 1, 1),
    ("dirname", 1, 1),
    ("joinPath", 1, usize::MAX),
    // Environment
    ("env", 1, 2),
    // Conditions
    ("matchesSelector", 2, 2),
];

/// The name of every function, in the order the reference lists them.
pub fn function_names() -> Vec<&'static str> {
    FUNCTIONS.iter().map(|(name, _, _)| *name).collect()
}

/// What each function is, in the words of the reference
/// (`docs/reference/dsl/expressions.md`): name, how it is called, what it
/// gives, an example, and the section it is listed under. Editors show
/// these beside a completion; a test keeps the table and the reference in
/// step.
const FUNCTION_DOCS: &[(&str, &str, &str, &str, &str)] = &[
    ("defined", "defined(path)", "whether the path is set. Takes a path, not a value", "", "Presence"),
    ("coalesce", "coalesce(a, b, ...)", "the first argument that is set and not null; `null` if none", "", "Presence"),
    ("default", "default(value, fallback)", "`value`, or `fallback` when it is not set or null", "", "Presence"),
    ("required", "required(value, 'message')", "`value`, or an error with your message when it is null", "", "Presence"),
    ("lower", "lower(text)", "the text in one case", "`upper('web')` is `'WEB'`", "Text"),
    ("upper", "upper(text)", "the text in one case", "`upper('web')` is `'WEB'`", "Text"),
    ("title", "title(text)", "each word capitalised", "`title('hello world')` is `'Hello World'`", "Text"),
    ("trim", "trim(text)", "without spaces and line breaks at the ends", "", "Text"),
    ("trimPrefix", "trimPrefix(text, prefix)", "without that start or end, if it has it", "`trimSuffix('a.conf', '.conf')` is `'a'`", "Text"),
    ("trimSuffix", "trimSuffix(text, suffix)", "without that start or end, if it has it", "`trimSuffix('a.conf', '.conf')` is `'a'`", "Text"),
    ("replace", "replace(text, from, to)", "every `from` replaced", "`replace('a-b', '-', '_')` is `'a_b'`", "Text"),
    ("split", "split(text, separator)", "a list of the parts. Empty text gives an empty list", "`split('a,b', ',')` is `['a', 'b']`", "Text"),
    ("join", "join(list, separator)", "the items as one text. Items are text, numbers or booleans", "`join(['a', 'b'], '-')` is `'a-b'`", "Text"),
    ("format", "format(template, values...)", "the template with `%s` (any single value) and `%d` (a whole number) filled in order; `%%` is a percent sign", "`format('%s-%d', 'web', 3)` is `'web-3'`", "Text"),
    ("startsWith", "startsWith(text, prefix)", "true or false", "", "Text"),
    ("endsWith", "endsWith(text, suffix)", "true or false", "", "Text"),
    ("contains", "contains(holder, item)", "whether text holds text, a list holds an item, or a mapping has a key", "`contains([80, 443], 443)` is `true`", "Text"),
    ("substr", "substr(text, start), substr(text, start, length)", "part of the text, counted in characters from 0", "`substr('abcdef', 1, 3)` is `'bcd'`", "Text"),
    ("indent", "indent(text, spaces)", "every line that is not empty moved right", "", "Text"),
    ("quote", "quote(text)", "the text in double quotes, with `\"` and `\\` escaped", "", "Text"),
    ("regexMatch", "regexMatch(text, pattern)", "whether the [pattern](#patterns) matches", "", "Text"),
    ("regexReplace", "regexReplace(text, pattern, to)", "every match replaced; `$1` is the first group", "`regexReplace('web01', '[0-9]+', 'N')` is `'webN'`", "Text"),
    ("regexFind", "regexFind(text, pattern)", "the first match, or `null`", "", "Text"),
    ("min", "min(a, b, ...)", "the smallest or largest. Also takes one list: `max(data.ports)`", "", "Numbers"),
    ("max", "max(a, b, ...)", "the smallest or largest. Also takes one list: `max(data.ports)`", "", "Numbers"),
    ("abs", "abs(n)", "without its sign", "", "Numbers"),
    ("floor", "floor(n)", "the whole number below or above", "", "Numbers"),
    ("ceil", "ceil(n)", "the whole number below or above", "", "Numbers"),
    ("round", "round(n), round(n, places)", "the nearest whole number, or to 0 to 15 decimal places. A half rounds away from zero", "", "Numbers"),
    ("sum", "sum(list)", "the total of a list of numbers; `0` for an empty list", "", "Numbers"),
    ("length", "length(x)", "the characters of text, the items of a list, the keys of a mapping", "", "Lists and mappings"),
    ("keys", "keys(map)", "the keys, or the values, in key order", "", "Lists and mappings"),
    ("values", "values(map)", "the keys, or the values, in key order", "", "Lists and mappings"),
    ("entries", "entries(map)", "a list of `{key, value}`", "", "Lists and mappings"),
    ("merge", "merge(a, b, ...)", "one mapping; a later argument's key replaces an earlier one's, whole", "`merge({a: {x: 1}}, {a: {y: 2}})` is `{a: {y: 2}}`", "Lists and mappings"),
    ("deepMerge", "deepMerge(a, b, ...)", "the same, but mappings inside mappings are merged key by key. Lists are replaced", "`deepMerge({a: {x: 1}}, {a: {y: 2}})` is `{a: {x: 1, y: 2}}`", "Lists and mappings"),
    ("concat", "concat(a, b, ...)", "the lists one after another", "", "Lists and mappings"),
    ("flatten", "flatten(list)", "lists inside the list opened up, one level", "`flatten([[1, 2], 3])` is `[1, 2, 3]`", "Lists and mappings"),
    ("unique", "unique(list)", "without repeats; the first of each is kept", "", "Lists and mappings"),
    ("sort", "sort(list)", "in order. All numbers, or all text", "", "Lists and mappings"),
    ("reverse", "reverse(x)", "a list or text, backwards", "", "Lists and mappings"),
    ("first", "first(list)", "one item. An empty list is an error", "", "Lists and mappings"),
    ("last", "last(list)", "one item. An empty list is an error", "", "Lists and mappings"),
    ("slice", "slice(list, start), slice(list, start, end)", "the items from `start` up to but not including `end`. Negative counts from the end", "`slice([1, 2, 3, 4], 1, 3)` is `[2, 3]`", "Lists and mappings"),
    ("range", "range(n), range(start, end), range(start, end, step)", "whole numbers from `start` up to but not including `end`. At most 10000", "`range(3)` is `[0, 1, 2]`", "Lists and mappings"),
    ("zip", "zip(a, b)", "pairs, as long as the shorter list", "`zip([1, 2], ['a', 'b'])` is `[[1, 'a'], [2, 'b']]`", "Lists and mappings"),
    ("lookup", "lookup(map, key), lookup(map, key, fallback)", "the value of the key; without a fallback a missing key is an error", "", "Lists and mappings"),
    ("pick", "pick(map, keys)", "the mapping with only, or without, those keys", "", "Lists and mappings"),
    ("omit", "omit(map, keys)", "the mapping with only, or without, those keys", "", "Lists and mappings"),
    ("any", "any(list)", "whether any, or every, item is true. `any([])` is false and `all([])` is true", "", "Lists and mappings"),
    ("all", "all(list)", "whether any, or every, item is true. `any([])` is false and `all([])` is true", "", "Lists and mappings"),
    ("map", "map(list, 'name', expression)", "a list of the expression's value for each item, which the expression reads by the name given", "`map([1, 2], 'n', n * 2)` is `[2, 4]`", "Lists and mappings"),
    ("filter", "filter(list, 'name', condition)", "the items the condition is true for", "`filter(data.ports, 'p', p > 1000)`", "Lists and mappings"),
    ("where", "where(list, 'field', value)", "the items whose field equals the value", "", "Selection"),
    ("pluck", "pluck(list, 'field')", "that field of every item that has it", "", "Selection"),
    ("groupBy", "groupBy(list, 'field')", "a mapping from each value of the field, as text, to the items that have it", "", "Selection"),
    ("type", "type(x)", "`'null'`, `'bool'`, `'number'`, `'string'`, `'list'` or `'map'`", "", "Types"),
    ("toString", "toString(x)", "text, a number or a boolean as text. A list or mapping has no single text form: use `join` or `toJson`", "", "Types"),
    ("toNumber", "toNumber(x)", "a number, from a number or from text holding one. Anything else is an error", "", "Types"),
    ("toBool", "toBool(x)", "from `true`, `false`, `'true'` or `'false'`. Nothing else: `'yes'` is refused", "", "Types"),
    ("toJson", "toJson(x)", "the value as compact JSON text, keys in order", "", "Types"),
    ("fromJson", "fromJson(text)", "the value the JSON text holds", "", "Types"),
    ("base64Encode", "base64Encode(text)", "standard base64. Decoding something that is not base64, or not text once decoded, is an error", "", "Encoding"),
    ("base64Decode", "base64Decode(text)", "standard base64. Decoding something that is not base64, or not text once decoded, is an error", "", "Encoding"),
    ("urlEncode", "urlEncode(text)", "the text safe for a URL: everything but letters, digits and `-_.~` percent-encoded", "", "Encoding"),
    ("sha256", "sha256(text)", "the SHA-256 of the text, as 64 hexadecimal characters", "", "Encoding"),
    ("cidrHost", "cidrHost(network, n)", "the nth address of the network; negative counts from the end", "`cidrHost('10.0.0.0/24', 5)` is `'10.0.0.5'`", "Networks"),
    ("cidrSubnet", "cidrSubnet(network, bits, n)", "the nth subnet made by adding `bits` to the prefix", "`cidrSubnet('10.0.0.0/16', 8, 3)` is `'10.0.3.0/24'`", "Networks"),
    ("cidrNetmask", "cidrNetmask(network)", "the netmask of an IPv4 network", "`cidrNetmask('10.0.0.0/20')` is `'255.255.240.0'`", "Networks"),
    ("cidrContains", "cidrContains(network, address)", "whether the address is inside the network", "", "Networks"),
    ("semverCompare", "semverCompare(a, b)", "`-1`, `0` or `1`", "", "Versions"),
    ("semverSatisfies", "semverSatisfies(version, range)", "whether the version meets every requirement of the range. Requirements are separated by spaces and start with `>=`, `<=`, `>`, `<`, `=` or `!=`: `'>=1.6 <2'`", "", "Versions"),
    ("basename", "basename(path)", "the last part: `'nginx.conf'`", "", "Paths"),
    ("dirname", "dirname(path)", "everything before it: `'/etc/nginx'`", "", "Paths"),
    ("joinPath", "joinPath(a, b, ...)", "the parts joined with one `/` between each", "", "Paths"),
    ("env", "env('NAME'), env('NAME', fallback)", "the environment variable of the `meridian` process. Not set is an error", "", "Environment"),
    ("matchesSelector", "matchesSelector(labels, 'tier=web,env!=dev')", "whether a mapping of labels meets a selector, in the language of [`-l`](index.md#labels-and-selectors): `key=value`, `key!=value`, `key`, `!key`, separated by commas", "", "Selectors"),
];

/// One function, as an editor describes it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FunctionInfo {
    pub name: &'static str,
    /// How it is called: `substr(text, start, length)`.
    pub signature: &'static str,
    /// What it gives, as the reference puts it.
    pub gives: &'static str,
    /// An example, or empty.
    pub example: &'static str,
    /// The reference section: `Text`, `Numbers`, ...
    pub category: &'static str,
    /// The fewest and the most arguments it takes.
    pub min_args: usize,
    pub max_args: usize,
}

impl FunctionInfo {
    /// The documentation as Markdown, for a hover or a completion.
    pub fn markdown(&self) -> String {
        let mut out = format!(
            "**`{}`** · {}\n\n{}",
            self.signature, self.category, self.gives
        );
        if !self.example.is_empty() {
            out.push_str(&format!("\n\nExample: {}", self.example));
        }
        out
    }
}

/// Every function with its documentation, in the order the reference
/// lists them.
pub fn functions() -> Vec<FunctionInfo> {
    FUNCTIONS
        .iter()
        .map(|(name, min, max)| {
            let (_, signature, gives, example, category) = FUNCTION_DOCS
                .iter()
                .find(|(n, ..)| n == name)
                .copied()
                .unwrap_or((name, name, "", "", ""));
            FunctionInfo {
                name,
                signature,
                gives,
                example,
                category,
                min_args: *min,
                max_args: *max,
            }
        })
        .collect()
}

/// The function called `name`, if there is one.
pub fn function(name: &str) -> Option<FunctionInfo> {
    functions().into_iter().find(|f| f.name == name)
}

pub(crate) fn check_arity(name: &str, given: usize) -> Result<(), Error> {
    let Some((_, min, max)) = FUNCTIONS.iter().find(|(n, _, _)| *n == name) else {
        // The nearest name, for a slip of the hand or of the case.
        let lower = name.to_lowercase();
        let near = FUNCTIONS
            .iter()
            .map(|(n, _, _)| *n)
            .find(|n| n.to_lowercase() == lower);
        return Err(Error::new(
            Kind::UnknownFunction,
            match near {
                Some(near) => format!("unknown function {name:?} (did you mean {near}?)"),
                None => format!("unknown function {name:?}"),
            },
        ));
    };
    let plural = |n: usize| if n == 1 { "argument" } else { "arguments" };
    if given < *min || given > *max {
        let takes = if min == max {
            format!("{min} {}", plural(*min))
        } else if *max == usize::MAX {
            format!("at least {min} {}", plural(*min))
        } else {
            format!("{min} to {max} arguments")
        };
        return Err(Error::new(
            Kind::Arity,
            format!("{name}(...) takes {takes}, and was given {given}"),
        ));
    }
    Ok(())
}

/// Whether `container` holds `item`: an item of a list, a key of a mapping,
/// or text inside text.
pub(crate) fn holds(container: &Value, item: &Value) -> Result<bool, ()> {
    match (container, item) {
        (Value::List(items), item) => Ok(items.iter().any(|candidate| same(candidate, item))),
        (Value::Map(map), Value::String(key)) => Ok(map.contains_key(key)),
        (Value::Map(_), _) => Ok(false),
        (Value::String(text), Value::String(part)) => Ok(text.contains(part.as_str())),
        _ => Err(()),
    }
}

pub(crate) fn pattern(text: &str) -> Result<regex::Regex, String> {
    regex::RegexBuilder::new(text)
        .size_limit(1 << 20)
        .build()
        .map_err(|_| format!("{text:?} is not a valid pattern"))
}

struct Args<'a> {
    name: &'a str,
    values: &'a [Value],
}

impl<'a> Args<'a> {
    fn wrong(&self, i: usize, wanted: &str) -> String {
        format!(
            "argument {} of {}(...) must be {wanted}, and is {}",
            i + 1,
            self.name,
            self.values[i].describe()
        )
    }

    fn text(&self, i: usize) -> Result<&'a str, String> {
        self.values[i].as_str().ok_or_else(|| self.wrong(i, "text"))
    }

    fn list(&self, i: usize) -> Result<&'a [Value], String> {
        self.values[i]
            .as_list()
            .ok_or_else(|| self.wrong(i, "a list"))
    }

    fn map(&self, i: usize) -> Result<&'a BTreeMap<String, Value>, String> {
        self.values[i]
            .as_map()
            .ok_or_else(|| self.wrong(i, "a mapping"))
    }

    fn number(&self, i: usize) -> Result<Number, String> {
        Number::of(&self.values[i]).ok_or_else(|| self.wrong(i, "a number"))
    }

    fn int(&self, i: usize) -> Result<i64, String> {
        match Number::of(&self.values[i]) {
            Some(Number::Int(n)) => Ok(n),
            _ => Err(self.wrong(i, "a whole number")),
        }
    }

    /// The numbers of a variadic call, or of its single list argument.
    fn numbers(&self) -> Result<Vec<Number>, String> {
        let values: &[Value] = match self.values {
            [Value::List(items)] => items,
            other => other,
        };
        if values.is_empty() {
            return Err(format!("{}(...) was given no numbers", self.name));
        }
        values
            .iter()
            .map(|v| {
                Number::of(v).ok_or_else(|| {
                    format!(
                        "{}(...) takes numbers, and was given {}",
                        self.name,
                        v.describe()
                    )
                })
            })
            .collect()
    }
}

fn text(s: impl Into<String>) -> Value {
    Value::String(s.into())
}

/// A value as the text an interpolation would place: text as itself, a
/// number or boolean as written. A list or mapping has no one text form.
pub(crate) fn to_text(value: &Value) -> Result<String, String> {
    match value {
        Value::String(s) => Ok(s.clone()),
        Value::Bool(_) | Value::Int(_) | Value::Uint(_) | Value::Float(_) => Ok(value.canonical()),
        Value::Null => Ok("null".to_owned()),
        other => Err(format!(
            "{} has no single text form; use join(...) or toJson(...)",
            other.describe()
        )),
    }
}

fn deep_merge(into: &mut BTreeMap<String, Value>, from: &BTreeMap<String, Value>) {
    for (key, value) in from {
        match (into.get_mut(key), value) {
            (Some(Value::Map(existing)), Value::Map(incoming)) => deep_merge(existing, incoming),
            _ => {
                into.insert(key.clone(), value.clone());
            }
        }
    }
}

fn sorted(items: &[Value], name: &str) -> Result<Vec<Value>, String> {
    let mut out = items.to_vec();
    if out.iter().all(|v| Number::of(v).is_some()) {
        out.sort_by(|a, b| {
            let (a, b) = (
                Number::of(a).map(Number::float),
                Number::of(b).map(Number::float),
            );
            a.partial_cmp(&b).unwrap_or(std::cmp::Ordering::Equal)
        });
    } else if out.iter().all(|v| v.as_str().is_some()) {
        out.sort_by(|a, b| a.as_str().cmp(&b.as_str()));
    } else {
        return Err(format!(
            "{name}(...) orders a list of numbers or a list of text, not a mix"
        ));
    }
    Ok(out)
}

struct Cidr {
    address: u128,
    prefix: u32,
    /// 32 for IPv4, 128 for IPv6.
    bits: u32,
}

impl Cidr {
    fn parse(text: &str) -> Result<Cidr, String> {
        let bad = || format!("{text:?} is not a network in CIDR form, such as 10.0.0.0/16");
        let (address, prefix) = text.split_once('/').ok_or_else(bad)?;
        let prefix: u32 = prefix.parse().map_err(|_| bad())?;
        let (address, bits) = match address.parse::<IpAddr>().map_err(|_| bad())? {
            IpAddr::V4(v4) => (u128::from(u32::from(v4)), 32),
            IpAddr::V6(v6) => (u128::from(v6), 128),
        };
        if prefix > bits {
            return Err(bad());
        }
        Ok(Cidr {
            address,
            prefix,
            bits,
        })
    }

    fn host_bits(&self) -> u32 {
        self.bits - self.prefix
    }

    fn mask(&self) -> u128 {
        let all = if self.bits == 32 {
            u128::from(u32::MAX)
        } else {
            u128::MAX
        };
        match self.host_bits() {
            0 => all,
            n if n >= 128 => 0,
            n => all & !((1u128 << n) - 1),
        }
    }

    fn network(&self) -> u128 {
        self.address & self.mask()
    }

    fn render(&self, address: u128) -> String {
        if self.bits == 32 {
            Ipv4Addr::from(address as u32).to_string()
        } else {
            Ipv6Addr::from(address).to_string()
        }
    }
}

/// A version as its numeric parts and whether it is a pre-release, which
/// orders before the release of the same numbers.
fn version(text: &str) -> Result<(Vec<u64>, bool), String> {
    let bad = || format!("{text:?} is not a version, such as 1.4.2");
    let body = text.trim().trim_start_matches('v');
    let body = body.split('+').next().unwrap_or(body);
    let (numbers, pre) = match body.split_once('-') {
        Some((numbers, _)) => (numbers, true),
        None => (body, false),
    };
    if numbers.is_empty() {
        return Err(bad());
    }
    let mut parts = numbers
        .split('.')
        .map(|part| part.parse::<u64>().map_err(|_| bad()))
        .collect::<Result<Vec<u64>, String>>()?;
    if parts.len() > 3 {
        return Err(bad());
    }
    parts.resize(3, 0);
    Ok((parts, pre))
}

fn compare_versions(a: &str, b: &str) -> Result<std::cmp::Ordering, String> {
    let (a, a_pre) = version(a)?;
    let (b, b_pre) = version(b)?;
    // A pre-release comes before its release: false (release) sorts after.
    Ok(a.cmp(&b).then(b_pre.cmp(&a_pre)))
}

fn satisfies(candidate: &str, range: &str) -> Result<bool, String> {
    for requirement in range.split_whitespace() {
        let (op, wanted) = ["<=", ">=", "!=", "<", ">", "="]
            .iter()
            .find_map(|op| requirement.strip_prefix(op).map(|rest| (*op, rest)))
            .unwrap_or(("=", requirement));
        let ordering = compare_versions(candidate, wanted)?;
        let holds = match op {
            "<=" => ordering.is_le(),
            ">=" => ordering.is_ge(),
            "!=" => ordering.is_ne(),
            "<" => ordering.is_lt(),
            ">" => ordering.is_gt(),
            _ => ordering.is_eq(),
        };
        if !holds {
            return Ok(false);
        }
    }
    Ok(true)
}

/// The selector language of `-l`: `key=value`, `key!=value`, `key`, `!key`,
/// separated by commas, all of which must hold.
fn matches_selector(labels: &BTreeMap<String, Value>, selector: &str) -> Result<bool, String> {
    let label = |key: &str| labels.get(key).map(|v| to_text(v).unwrap_or_default());
    // The whole selector is checked before any of it decides, so a malformed
    // one is always an error and never a quiet "no".
    if selector
        .split(',')
        .any(|requirement| requirement.trim().is_empty())
    {
        return Err(format!(
            "the selector {selector:?} has an empty requirement"
        ));
    }
    let mut all = true;
    for requirement in selector.split(',') {
        let requirement = requirement.trim();
        let bad = || format!("{requirement:?} is not a selector requirement");
        let holds = if let Some((key, value)) = requirement.split_once("!=") {
            if key.trim().is_empty() {
                return Err(bad());
            }
            label(key.trim()).as_deref() != Some(value.trim())
        } else if let Some((key, value)) = requirement
            .split_once("==")
            .or_else(|| requirement.split_once('='))
        {
            if key.trim().is_empty() {
                return Err(bad());
            }
            label(key.trim()).as_deref() == Some(value.trim())
        } else if let Some(key) = requirement.strip_prefix('!') {
            if key.trim().is_empty() {
                return Err(bad());
            }
            !labels.contains_key(key.trim())
        } else {
            labels.contains_key(requirement)
        };
        all = all && holds;
    }
    Ok(all)
}

fn format_text(args: &Args) -> Result<String, String> {
    let template = args.text(0)?;
    let mut out = String::new();
    let mut next = 1;
    let mut chars = template.chars();
    while let Some(c) = chars.next() {
        if c != '%' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('%') => out.push('%'),
            Some(verb @ ('s' | 'd')) => {
                let Some(value) = args.values.get(next) else {
                    return Err(format!(
                        "format(...) has more % places than values: {template:?}"
                    ));
                };
                if verb == 'd' && !matches!(Number::of(value), Some(Number::Int(_))) {
                    return Err(format!(
                        "%d in format(...) takes a whole number, and was given {}",
                        value.describe()
                    ));
                }
                out.push_str(&to_text(value)?);
                next += 1;
            }
            other => {
                return Err(format!(
                    "format(...) understands %s, %d and %%, not %{}",
                    other.map(String::from).unwrap_or_default()
                ))
            }
        }
    }
    if next != args.values.len() {
        return Err(format!(
            "format(...) was given more values than {template:?} has % places"
        ));
    }
    Ok(out)
}

fn field<'a>(item: &'a Value, key: &str, name: &str) -> Result<Option<&'a Value>, String> {
    match item {
        Value::Map(map) => Ok(map.get(key)),
        other => Err(format!(
            "{name}(...) takes a list of mappings, and found {} in it",
            other.describe()
        )),
    }
}

/// Calls a function by name. A failure is about an argument: the arity and
/// the name were checked when the expression was read.
pub(crate) fn call(name: &str, values: &[Value], scope: &Scope) -> Result<Value, Error> {
    dispatch(name, values, scope).map_err(|msg| Error::new(Kind::Argument, msg))
}

fn dispatch(name: &str, values: &[Value], scope: &Scope) -> Result<Value, String> {
    let args = Args { name, values };
    Ok(match name {
        "required" => match &values[0] {
            Value::Null => return Err(args.text(1)?.to_owned()),
            value => value.clone(),
        },

        "lower" => text(args.text(0)?.to_lowercase()),
        "upper" => text(args.text(0)?.to_uppercase()),
        "title" => {
            let mut out = String::new();
            let mut start = true;
            for c in args.text(0)?.chars() {
                if start {
                    out.extend(c.to_uppercase());
                } else {
                    out.push(c);
                }
                start = !c.is_alphanumeric();
            }
            text(out)
        }
        "trim" => text(args.text(0)?.trim()),
        "trimPrefix" => {
            let s = args.text(0)?;
            text(s.strip_prefix(args.text(1)?).unwrap_or(s))
        }
        "trimSuffix" => {
            let s = args.text(0)?;
            text(s.strip_suffix(args.text(1)?).unwrap_or(s))
        }
        "replace" => text(args.text(0)?.replace(args.text(1)?, args.text(2)?)),
        "split" => {
            let (s, separator) = (args.text(0)?, args.text(1)?);
            if separator.is_empty() {
                return Err("split(...) needs a separator that is not empty".to_owned());
            }
            if s.is_empty() {
                Value::List(Vec::new())
            } else {
                Value::List(s.split(separator).map(text).collect())
            }
        }
        "join" => {
            let parts = args
                .list(0)?
                .iter()
                .map(to_text)
                .collect::<Result<Vec<String>, String>>()?;
            text(parts.join(args.text(1)?))
        }
        "format" => text(format_text(&args)?),
        "startsWith" => Value::Bool(args.text(0)?.starts_with(args.text(1)?)),
        "endsWith" => Value::Bool(args.text(0)?.ends_with(args.text(1)?)),
        "contains" => Value::Bool(
            holds(&values[0], &values[1])
                .map_err(|()| args.wrong(0, "a list, a mapping or text (with text to look for)"))?,
        ),
        "substr" => {
            let chars: Vec<char> = args.text(0)?.chars().collect();
            let start = args.int(1)?;
            let len = chars.len() as i64;
            let length = if values.len() == 3 { args.int(2)? } else { len };
            if start < 0 || length < 0 {
                return Err(
                    "substr(...) takes a start and a length that are not negative".to_owned(),
                );
            }
            let from = start.min(len) as usize;
            let to = start.saturating_add(length).min(len) as usize;
            text(chars[from..to].iter().collect::<String>())
        }
        "indent" => {
            let pad = " ".repeat(
                usize::try_from(args.int(1)?)
                    .map_err(|_| args.wrong(1, "a count that is not negative"))?
                    .min(1000),
            );
            let lines: Vec<String> = args
                .text(0)?
                .split('\n')
                .map(|line| {
                    if line.is_empty() {
                        String::new()
                    } else {
                        format!("{pad}{line}")
                    }
                })
                .collect();
            text(lines.join("\n"))
        }
        "quote" => {
            let escaped = args.text(0)?.replace('\\', "\\\\").replace('"', "\\\"");
            text(format!("\"{escaped}\""))
        }
        "regexMatch" => Value::Bool(pattern(args.text(1)?)?.is_match(args.text(0)?)),
        "regexReplace" => text(
            pattern(args.text(1)?)?
                .replace_all(args.text(0)?, args.text(2)?)
                .into_owned(),
        ),
        "regexFind" => match pattern(args.text(1)?)?.find(args.text(0)?) {
            Some(found) => text(found.as_str()),
            None => Value::Null,
        },

        "min" | "max" => {
            let numbers = args.numbers()?;
            let mut best = numbers[0];
            for n in &numbers[1..] {
                let better = if name == "min" {
                    n.float() < best.float()
                } else {
                    n.float() > best.float()
                };
                if better {
                    best = *n;
                }
            }
            best.value()
        }
        "abs" => match args.number(0)? {
            Number::Int(n) => Value::Int(
                n.checked_abs()
                    .ok_or("the result of abs(...) is too large for a whole number")?,
            ),
            Number::Float(n) => Value::Float(n.abs()),
        },
        "floor" | "ceil" => match args.number(0)? {
            Number::Int(n) => Value::Int(n),
            Number::Float(n) => {
                let rounded = if name == "floor" { n.floor() } else { n.ceil() };
                whole(rounded)
            }
        },
        "round" => {
            let digits = if values.len() == 2 { args.int(1)? } else { 0 };
            if !(0..=15).contains(&digits) {
                return Err("round(...) takes 0 to 15 decimal places".to_owned());
            }
            match args.number(0)? {
                Number::Int(n) => Value::Int(n),
                Number::Float(n) if digits == 0 => whole(n.round()),
                Number::Float(n) => {
                    let scale = 10f64.powi(digits as i32);
                    Value::Float((n * scale).round() / scale)
                }
            }
        }
        "sum" => {
            let mut total = Number::Int(0);
            for (i, item) in args.list(0)?.iter().enumerate() {
                let n = Number::of(item).ok_or_else(|| {
                    format!(
                        "sum(...) takes a list of numbers; item {i} is {}",
                        item.describe()
                    )
                })?;
                total = match (total, n) {
                    (Number::Int(a), Number::Int(b)) => Number::Int(
                        a.checked_add(b)
                            .ok_or("the result of sum(...) is too large for a whole number")?,
                    ),
                    (a, b) => Number::Float(a.float() + b.float()),
                };
            }
            total.value()
        }

        "length" => Value::Int(match &values[0] {
            Value::String(s) => s.chars().count() as i64,
            Value::List(items) => items.len() as i64,
            Value::Map(map) => map.len() as i64,
            _ => return Err(args.wrong(0, "text, a list or a mapping")),
        }),
        "keys" => Value::List(args.map(0)?.keys().map(text).collect()),
        "values" => Value::List(args.map(0)?.values().cloned().collect()),
        "entries" => Value::List(
            args.map(0)?
                .iter()
                .map(|(key, value)| {
                    Value::Map(BTreeMap::from([
                        ("key".to_owned(), text(key)),
                        ("value".to_owned(), value.clone()),
                    ]))
                })
                .collect(),
        ),
        "merge" | "deepMerge" => {
            let mut out = BTreeMap::new();
            for i in 0..values.len() {
                let map = args.map(i)?;
                if name == "merge" {
                    out.extend(map.iter().map(|(k, v)| (k.clone(), v.clone())));
                } else {
                    deep_merge(&mut out, map);
                }
            }
            Value::Map(out)
        }
        "concat" => {
            let mut out = Vec::new();
            for i in 0..values.len() {
                out.extend(args.list(i)?.iter().cloned());
            }
            Value::List(out)
        }
        "flatten" => {
            let mut out = Vec::new();
            for item in args.list(0)? {
                match item {
                    Value::List(inner) => out.extend(inner.iter().cloned()),
                    other => out.push(other.clone()),
                }
            }
            Value::List(out)
        }
        "unique" => {
            let mut out: Vec<Value> = Vec::new();
            for item in args.list(0)? {
                if !out.iter().any(|seen| same(seen, item)) {
                    out.push(item.clone());
                }
            }
            Value::List(out)
        }
        "sort" => Value::List(sorted(args.list(0)?, name)?),
        "reverse" => match &values[0] {
            Value::List(items) => Value::List(items.iter().rev().cloned().collect()),
            Value::String(s) => text(s.chars().rev().collect::<String>()),
            _ => return Err(args.wrong(0, "a list or text")),
        },
        "first" | "last" => {
            let items = args.list(0)?;
            let item = if name == "first" {
                items.first()
            } else {
                items.last()
            };
            item.cloned()
                .ok_or_else(|| format!("{name}(...) was given an empty list"))?
        }
        "slice" => {
            let items = args.list(0)?;
            let len = items.len() as i64;
            let clamp = |n: i64| (if n < 0 { len + n } else { n }).clamp(0, len) as usize;
            let from = clamp(args.int(1)?);
            let to = if values.len() == 3 {
                clamp(args.int(2)?)
            } else {
                items.len()
            };
            Value::List(items[from..to.max(from)].to_vec())
        }
        "range" => {
            let (start, end, step) = match values.len() {
                1 => (0, args.int(0)?, 1),
                2 => (args.int(0)?, args.int(1)?, 1),
                _ => (args.int(0)?, args.int(1)?, args.int(2)?),
            };
            if step == 0 {
                return Err("range(...) takes a step that is not zero".to_owned());
            }
            // In 128 bits, so that no pair of whole numbers can overflow.
            let (wide_start, wide_end, wide_step) =
                (i128::from(start), i128::from(end), i128::from(step));
            let count = if (step > 0 && end > start) || (step < 0 && end < start) {
                ((wide_end - wide_start).abs() + wide_step.abs() - 1) / wide_step.abs()
            } else {
                0
            };
            if count > i128::from(MAX_RANGE) {
                return Err(format!(
                    "range(...) would build {count} items; the most is {MAX_RANGE}"
                ));
            }
            Value::List(
                (0..count as i64)
                    .map(|i| Value::Int(start + i * step))
                    .collect(),
            )
        }
        "zip" => Value::List(
            args.list(0)?
                .iter()
                .zip(args.list(1)?)
                .map(|(a, b)| Value::List(vec![a.clone(), b.clone()]))
                .collect(),
        ),
        "lookup" => match args.map(0)?.get(args.text(1)?) {
            Some(value) => value.clone(),
            None => match values.get(2) {
                Some(fallback) => fallback.clone(),
                None => {
                    return Err(format!(
                        "lookup(...) found no key {:?}, and was given no fallback",
                        args.text(1)?
                    ))
                }
            },
        },
        "pick" | "omit" => {
            let wanted = args
                .list(1)?
                .iter()
                .map(|key| {
                    key.as_str()
                        .ok_or_else(|| args.wrong(1, "a list of keys, as text"))
                })
                .collect::<Result<Vec<&str>, String>>()?;
            Value::Map(
                args.map(0)?
                    .iter()
                    .filter(|(key, _)| wanted.contains(&key.as_str()) == (name == "pick"))
                    .map(|(k, v)| (k.clone(), v.clone()))
                    .collect(),
            )
        }
        "any" | "all" => {
            let mut flags = Vec::new();
            for item in args.list(0)? {
                match item {
                    Value::Bool(b) => flags.push(*b),
                    other => {
                        return Err(format!(
                            "{name}(...) takes a list of true and false, and found {} in it",
                            other.describe()
                        ))
                    }
                }
            }
            Value::Bool(if name == "any" {
                flags.iter().any(|b| *b)
            } else {
                flags.iter().all(|b| *b)
            })
        }

        "where" => {
            let key = args.text(1)?;
            let mut out = Vec::new();
            for item in args.list(0)? {
                if field(item, key, name)?.is_some_and(|v| same(v, &values[2])) {
                    out.push(item.clone());
                }
            }
            Value::List(out)
        }
        "pluck" => {
            let key = args.text(1)?;
            let mut out = Vec::new();
            for item in args.list(0)? {
                if let Some(value) = field(item, key, name)? {
                    out.push(value.clone());
                }
            }
            Value::List(out)
        }
        "groupBy" => {
            let key = args.text(1)?;
            let mut out: BTreeMap<String, Value> = BTreeMap::new();
            for item in args.list(0)? {
                let Some(value) = field(item, key, name)? else {
                    continue;
                };
                let group = to_text(value)?;
                match out.entry(group).or_insert_with(|| Value::List(Vec::new())) {
                    Value::List(items) => items.push(item.clone()),
                    _ => continue,
                }
            }
            Value::Map(out)
        }

        "type" => text(match &values[0] {
            Value::Null => "null",
            Value::Bool(_) => "bool",
            Value::Int(_) | Value::Uint(_) | Value::Float(_) => "number",
            Value::String(_) => "string",
            Value::List(_) => "list",
            Value::Map(_) => "map",
            Value::Secret(_) => "secret",
        }),
        "toString" => text(to_text(&values[0])?),
        "toNumber" => match &values[0] {
            v if Number::of(v).is_some() => v.clone(),
            Value::String(s) => {
                let s = s.trim();
                if let Ok(n) = s.parse::<i64>() {
                    Value::Int(n)
                } else {
                    match s.parse::<f64>() {
                        Ok(n) if n.is_finite() => Value::Float(n),
                        _ => return Err(format!("toNumber(...): {s:?} is not a number")),
                    }
                }
            }
            _ => return Err(args.wrong(0, "a number or text holding one")),
        },
        "toBool" => match &values[0] {
            Value::Bool(b) => Value::Bool(*b),
            Value::String(s) if s == "true" => Value::Bool(true),
            Value::String(s) if s == "false" => Value::Bool(false),
            Value::String(s) => {
                return Err(format!(
                    "toBool(...): {s:?} is neither \"true\" nor \"false\""
                ))
            }
            _ => return Err(args.wrong(0, "true, false, or the text of one")),
        },
        "toJson" => text(serde_free_json(&values[0])),
        "fromJson" => meridian_value::decode_value(args.text(0)?.as_bytes())
            .map_err(|why| format!("fromJson(...): {why}"))?,

        "base64Encode" => text(base64::engine::general_purpose::STANDARD.encode(args.text(0)?)),
        "base64Decode" => {
            let bytes = base64::engine::general_purpose::STANDARD
                .decode(args.text(0)?.trim())
                .map_err(|_| "base64Decode(...): the text is not base64".to_owned())?;
            text(
                String::from_utf8(bytes)
                    .map_err(|_| "base64Decode(...): the decoded bytes are not text".to_owned())?,
            )
        }
        "urlEncode" => {
            let mut out = String::new();
            for byte in args.text(0)?.bytes() {
                if byte.is_ascii_alphanumeric() || b"-_.~".contains(&byte) {
                    out.push(byte as char);
                } else {
                    out.push_str(&format!("%{byte:02X}"));
                }
            }
            text(out)
        }
        "sha256" => {
            let digest = Sha256::digest(args.text(0)?.as_bytes());
            text(
                digest
                    .iter()
                    .map(|b| format!("{b:02x}"))
                    .collect::<String>(),
            )
        }

        "cidrHost" => {
            let cidr = Cidr::parse(args.text(0)?)?;
            let n = args.int(1)?;
            let host_bits = cidr.host_bits();
            let size = if host_bits >= 127 {
                u128::MAX
            } else {
                1u128 << host_bits
            };
            // A negative number counts back from the end of the network.
            let offset = if n >= 0 {
                n as u128
            } else {
                size.wrapping_sub(n.unsigned_abs() as u128)
            };
            if offset >= size {
                return Err(format!(
                    "cidrHost(...): {} has no host number {n}",
                    args.text(0)?
                ));
            }
            text(cidr.render(cidr.network() + offset))
        }
        "cidrSubnet" => {
            let cidr = Cidr::parse(args.text(0)?)?;
            let (new_bits, number) = (args.int(1)?, args.int(2)?);
            let prefix = i64::from(cidr.prefix).saturating_add(new_bits);
            if new_bits < 0 || prefix > i64::from(cidr.bits) {
                return Err(format!(
                    "cidrSubnet(...): {} cannot be divided by {new_bits} more bits",
                    args.text(0)?
                ));
            }
            if number < 0 || (new_bits < 63 && number >= (1i64 << new_bits)) {
                return Err(format!(
                    "cidrSubnet(...): {new_bits} more bits give subnets 0 to {}, not {number}",
                    (1i64 << new_bits.min(62)) - 1
                ));
            }
            let shift = cidr.bits - prefix as u32;
            let address = if shift >= 128 {
                cidr.network()
            } else {
                cidr.network() | ((number as u128) << shift)
            };
            text(format!("{}/{prefix}", cidr.render(address)))
        }
        "cidrNetmask" => {
            let cidr = Cidr::parse(args.text(0)?)?;
            if cidr.bits != 32 {
                return Err(
                    "cidrNetmask(...) is for IPv4 networks; IPv6 has no netmask form".to_owned(),
                );
            }
            text(cidr.render(cidr.mask()))
        }
        "cidrContains" => {
            let cidr = Cidr::parse(args.text(0)?)?;
            let address = args.text(1)?;
            let (address, bits) = match address
                .parse::<IpAddr>()
                .map_err(|_| format!("cidrContains(...): {address:?} is not an address"))?
            {
                IpAddr::V4(v4) => (u128::from(u32::from(v4)), 32),
                IpAddr::V6(v6) => (u128::from(v6), 128),
            };
            Value::Bool(bits == cidr.bits && address & cidr.mask() == cidr.network())
        }

        "semverCompare" => Value::Int(match compare_versions(args.text(0)?, args.text(1)?)? {
            std::cmp::Ordering::Less => -1,
            std::cmp::Ordering::Equal => 0,
            std::cmp::Ordering::Greater => 1,
        }),
        "semverSatisfies" => Value::Bool(satisfies(args.text(0)?, args.text(1)?)?),

        "basename" => {
            let path = args.text(0)?.trim_end_matches('/');
            text(path.rsplit('/').next().unwrap_or(path))
        }
        "dirname" => {
            let whole = args.text(0)?;
            let path = whole.trim_end_matches('/');
            match path.rsplit_once('/') {
                Some(("", _)) => text("/"),
                Some((dir, _)) => text(dir),
                // The root's parent is the root, as POSIX dirname has it.
                None if path.is_empty() && whole.starts_with('/') => text("/"),
                None => text("."),
            }
        }
        "joinPath" => {
            let mut out = String::new();
            for i in 0..values.len() {
                let part = args.text(i)?;
                if part.is_empty() {
                    continue;
                }
                if out.is_empty() {
                    out.push_str(part);
                } else {
                    if !out.ends_with('/') {
                        out.push('/');
                    }
                    out.push_str(part.trim_start_matches('/'));
                }
            }
            text(out)
        }

        "env" => {
            let key = args.text(0)?;
            let found = match &scope.env {
                Some(env) => env.get(key).cloned(),
                None => std::env::var(key).ok(),
            };
            match (found, values.get(1)) {
                (Some(value), _) => text(value),
                (None, Some(fallback)) => fallback.clone(),
                (None, None) => {
                    return Err(format!(
                        "the environment variable {key} is not set (give env(...) a second \
                         argument to fall back on)"
                    ))
                }
            }
        }

        "matchesSelector" => Value::Bool(matches_selector(args.map(0)?, args.text(1)?)?),

        other => return Err(format!("unknown function {other:?}")),
    })
}

/// A float that is a whole number, as an integer when it fits one.
fn whole(n: f64) -> Value {
    if n.is_finite() && n.abs() < 9.0e15 {
        Value::Int(n as i64)
    } else {
        Value::Float(n)
    }
}

/// Compact JSON that escapes only what JSON requires, so `<` and `&` stay
/// readable in a file a person will open.
fn serde_free_json(value: &Value) -> String {
    match value {
        Value::Null => "null".to_owned(),
        Value::Secret(_) => format!("\"{}\"", meridian_value::SENSITIVE),
        Value::Bool(_) | Value::Int(_) | Value::Uint(_) | Value::Float(_) => value.canonical(),
        Value::String(s) => {
            let mut out = String::from("\"");
            for c in s.chars() {
                match c {
                    '"' => out.push_str("\\\""),
                    '\\' => out.push_str("\\\\"),
                    '\n' => out.push_str("\\n"),
                    '\r' => out.push_str("\\r"),
                    '\t' => out.push_str("\\t"),
                    c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
                    c => out.push(c),
                }
            }
            out.push('"');
            out
        }
        Value::List(items) => {
            let parts: Vec<String> = items.iter().map(serde_free_json).collect();
            format!("[{}]", parts.join(","))
        }
        Value::Map(map) => {
            let parts: Vec<String> = map
                .iter()
                .map(|(k, v)| {
                    format!(
                        "{}:{}",
                        serde_free_json(&Value::String(k.clone())),
                        serde_free_json(v)
                    )
                })
                .collect();
            format!("{{{}}}", parts.join(","))
        }
    }
}
