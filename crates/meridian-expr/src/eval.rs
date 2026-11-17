//! Tree to value.

use std::collections::{BTreeMap, BTreeSet};

use meridian_secret::{Piece, SecretTable, Transform};
use meridian_value::{SecretRef, Value};

use crate::funcs;
use crate::parse::{Binary, Node, Unary};
use crate::{Names, Scope};

/// Why an expression has no value. "Not set" is kept apart from every other
/// failure because it is the one a `defined()`, a `??` or a `default()` may
/// answer; anything else is a mistake and always surfaces.
pub(crate) enum Failure {
    NotSet { path: String, hint: String },
    Other(String),
}

impl Failure {
    pub(crate) fn message(self) -> String {
        match self {
            Failure::NotSet { path, hint } if hint.is_empty() => {
                format!("{path} is not set (test for it with defined({path}))")
            }
            Failure::NotSet { path, hint } => {
                format!("{path} is not set ({})", hint.replace("{path}", &path))
            }
            Failure::Other(message) => message,
        }
    }
}

impl From<String> for Failure {
    fn from(message: String) -> Self {
        Failure::Other(message)
    }
}

type Outcome<T> = Result<T, Failure>;

/// The functions that decide for themselves which arguments to evaluate.
const LAZY: [&str; 5] = ["defined", "coalesce", "default", "map", "filter"];

pub(crate) fn check(node: &Node, names: &Names, bound: &mut Vec<String>) -> Result<(), String> {
    match node {
        Node::Literal(_) => Ok(()),
        Node::Name(name) => {
            if bound.contains(name) || names.roots.contains(name) {
                // A root on its own is its whole mapping. `data` alone is
                // almost always a path that was never finished, so it keeps
                // the message it has always had.
                if name == "data" && !bound.contains(name) {
                    return Err("data must be followed by a key, as in data.environment".to_owned());
                }
                return Ok(());
            }
            if let Some(why) = names.refused.get(name) {
                return Err(why.clone());
            }
            let hint = if names.hint.is_empty() {
                "text is quoted".to_owned()
            } else {
                names.hint.clone()
            };
            Err(format!("unknown name {name:?} ({hint})"))
        }
        Node::Field(base, _) => match base.as_ref() {
            // `data.key`: the root itself needs no further check.
            Node::Name(name) if names.roots.contains(name) && !bound.contains(name) => Ok(()),
            other => check(other, names, bound),
        },
        Node::Index(base, index) => {
            match base.as_ref() {
                Node::Name(name) if names.roots.contains(name) && !bound.contains(name) => {}
                other => check(other, names, bound)?,
            }
            check(index, names, bound)
        }
        Node::Unary(_, inner) => check(inner, names, bound),
        Node::Binary(_, left, right) => {
            check(left, names, bound)?;
            check(right, names, bound)
        }
        Node::Choice(condition, yes, no) => {
            check(condition, names, bound)?;
            check(yes, names, bound)?;
            check(no, names, bound)
        }
        Node::List(items) => items.iter().try_for_each(|item| check(item, names, bound)),
        Node::Map(entries) => entries
            .iter()
            .try_for_each(|(_, value)| check(value, names, bound)),
        Node::Call(name, args) => {
            funcs::check_arity(name, args.len())?;
            match name.as_str() {
                "defined" => {
                    if !args[0].is_path() {
                        return Err("defined(...) takes a data.<key> path".to_owned());
                    }
                    check(&args[0], names, bound)
                }
                "map" | "filter" => {
                    check(&args[0], names, bound)?;
                    let Node::Literal(Value::String(item)) = &args[1] else {
                        return Err(format!(
                            "the second argument of {name}(...) is the name each item is \
                             given, as quoted text: {name}(list, 'item', expression)"
                        ));
                    };
                    bound.push(item.clone());
                    let outcome = check(&args[2], names, bound);
                    bound.pop();
                    outcome
                }
                _ => args.iter().try_for_each(|arg| check(arg, names, bound)),
            }
        }
    }
}

pub(crate) fn roots(node: &Node, bound: &mut Vec<String>, out: &mut BTreeSet<String>) {
    match node {
        Node::Literal(_) => {}
        Node::Name(name) => {
            if !bound.contains(name) {
                out.insert(name.clone());
            }
        }
        Node::Field(base, _) => roots(base, bound, out),
        Node::Index(base, index) => {
            roots(base, bound, out);
            roots(index, bound, out);
        }
        Node::Unary(_, inner) => roots(inner, bound, out),
        Node::Binary(_, left, right) => {
            roots(left, bound, out);
            roots(right, bound, out);
        }
        Node::Choice(a, b, c) => {
            roots(a, bound, out);
            roots(b, bound, out);
            roots(c, bound, out);
        }
        Node::List(items) => items.iter().for_each(|item| roots(item, bound, out)),
        Node::Map(entries) => entries.iter().for_each(|(_, v)| roots(v, bound, out)),
        Node::Call(name, args) => {
            if matches!(name.as_str(), "map" | "filter") && args.len() == 3 {
                roots(&args[0], bound, out);
                if let Node::Literal(Value::String(item)) = &args[1] {
                    bound.push(item.clone());
                    roots(&args[2], bound, out);
                    bound.pop();
                }
            } else {
                args.iter().for_each(|arg| roots(arg, bound, out));
            }
        }
    }
}

/// Every root an expression reads, with the first key under it when that is
/// written out: `var.name` and `var['name']` are `("var", Some("name"))`, and
/// `var` alone or `var[x]` is `("var", None)`.
pub(crate) fn reads(
    node: &Node,
    bound: &mut Vec<String>,
    out: &mut BTreeSet<(String, Option<String>)>,
) {
    let free = |name: &String, bound: &Vec<String>| !bound.contains(name);
    match node {
        Node::Literal(_) => {}
        Node::Name(name) => {
            if free(name, bound) {
                out.insert((name.clone(), None));
            }
        }
        Node::Field(base, key) => match base.as_ref() {
            Node::Name(name) => {
                if free(name, bound) {
                    out.insert((name.clone(), Some(key.clone())));
                }
            }
            other => reads(other, bound, out),
        },
        Node::Index(base, index) => {
            match (base.as_ref(), index.as_ref()) {
                (Node::Name(name), Node::Literal(Value::String(key))) => {
                    if free(name, bound) {
                        out.insert((name.clone(), Some(key.clone())));
                    }
                }
                (other, _) => reads(other, bound, out),
            }
            reads(index, bound, out);
        }
        Node::Unary(_, inner) => reads(inner, bound, out),
        Node::Binary(_, left, right) => {
            reads(left, bound, out);
            reads(right, bound, out);
        }
        Node::Choice(a, b, c) => {
            reads(a, bound, out);
            reads(b, bound, out);
            reads(c, bound, out);
        }
        Node::List(items) => items.iter().for_each(|item| reads(item, bound, out)),
        Node::Map(entries) => entries.iter().for_each(|(_, v)| reads(v, bound, out)),
        Node::Call(name, args) => {
            if matches!(name.as_str(), "map" | "filter") && args.len() == 3 {
                reads(&args[0], bound, out);
                if let Node::Literal(Value::String(item)) = &args[1] {
                    bound.push(item.clone());
                    reads(&args[2], bound, out);
                    bound.pop();
                }
            } else {
                args.iter().for_each(|arg| reads(arg, bound, out));
            }
        }
    }
}

pub(crate) struct Evaluator<'a> {
    scope: &'a Scope,
    /// Names bound by `map` and `filter`, innermost last.
    bound: Vec<(String, Value)>,
}

/// A number, kept as an integer for as long as it is one.
#[derive(Clone, Copy)]
pub(crate) enum Number {
    Int(i64),
    Float(f64),
}

impl Number {
    pub(crate) fn of(value: &Value) -> Option<Number> {
        match value {
            Value::Int(n) => Some(Number::Int(*n)),
            Value::Uint(n) => Some(match i64::try_from(*n) {
                Ok(n) => Number::Int(n),
                Err(_) => Number::Float(*n as f64),
            }),
            Value::Float(n) => Some(Number::Float(*n)),
            _ => None,
        }
    }

    pub(crate) fn float(self) -> f64 {
        match self {
            Number::Int(n) => n as f64,
            Number::Float(n) => n,
        }
    }

    pub(crate) fn value(self) -> Value {
        match self {
            Number::Int(n) => Value::Int(n),
            Number::Float(n) => Value::Float(n),
        }
    }
}

/// Whether two values are the same. Numbers compare by value whatever they
/// were written as, at any depth; everything else compares as itself.
pub(crate) fn same(a: &Value, b: &Value) -> bool {
    match (Number::of(a), Number::of(b)) {
        (Some(x), Some(y)) => return x.float() == y.float(),
        (Some(_), None) | (None, Some(_)) => return false,
        (None, None) => {}
    }
    match (a, b) {
        (Value::List(x), Value::List(y)) => {
            x.len() == y.len() && x.iter().zip(y).all(|(p, q)| same(p, q))
        }
        (Value::Map(x), Value::Map(y)) => {
            x.len() == y.len()
                && x.iter()
                    .all(|(key, p)| y.get(key).is_some_and(|q| same(p, q)))
        }
        _ => a == b,
    }
}

/// Whether a secret is anywhere in the value.
pub(crate) fn holds_secret(value: &Value) -> bool {
    match value {
        Value::Secret(_) => true,
        Value::List(items) => items.iter().any(holds_secret),
        Value::Map(entries) => entries.values().any(holds_secret),
        _ => false,
    }
}

enum PieceOwned {
    Text(String),
    Secret(SecretRef),
}

impl PieceOwned {
    fn piece(&self) -> Piece<'_> {
        match self {
            PieceOwned::Text(text) => Piece::Text(text),
            PieceOwned::Secret(secret) => Piece::Secret(*secret),
        }
    }
}

/// `value`, or `None` when it is not set or is null: what `??`, `default()`
/// and `coalesce()` fall through on.
fn present_of(value: Outcome<Value>) -> Outcome<Option<Value>> {
    match value {
        Ok(Value::Null) | Err(Failure::NotSet { .. }) => Ok(None),
        Ok(value) => Ok(Some(value)),
        Err(other) => Err(other),
    }
}

fn overflow(op: &str) -> Failure {
    Failure::Other(format!(
        "the result of {op} is too large for a whole number"
    ))
}

impl<'a> Evaluator<'a> {
    pub(crate) fn new(scope: &'a Scope) -> Self {
        Evaluator {
            scope,
            bound: Vec::new(),
        }
    }

    pub(crate) fn truth(&mut self, node: &Node) -> Outcome<bool> {
        let value = self.value(node);
        self.truth_of(value, node)
    }

    /// Whether `value`, which is what `node` came to, is true.
    fn truth_of(&self, value: Outcome<Value>, node: &Node) -> Outcome<bool> {
        match value? {
            Value::Bool(b) => Ok(b),
            other => Err(Failure::Other(format!(
                "{} is {}, not true or false; compare it with == or !=",
                node.describe(),
                other.describe()
            ))),
        }
    }

    fn not_set(&self, node: &Node) -> Failure {
        let hint = node
            .root()
            .and_then(|root| self.scope.unset_hints.get(root))
            .cloned()
            .unwrap_or_default();
        Failure::NotSet {
            path: node.describe(),
            hint,
        }
    }

    pub(crate) fn value(&mut self, node: &Node) -> Outcome<Value> {
        Ok(match node {
            Node::Literal(value) => value.clone(),
            Node::Name(name) => {
                if let Some((_, value)) = self.bound.iter().rev().find(|(n, _)| n == name) {
                    return Ok(value.clone());
                }
                match self.scope.roots.get(name) {
                    Some(value) => value.clone(),
                    None => return Err(self.not_set(node)),
                }
            }
            Node::Field(..) | Node::Index(..) => self.path(node)?,
            Node::Unary(Unary::Not, inner) => Value::Bool(!self.truth(inner)?),
            Node::Unary(Unary::Negate, inner) => match Number::of(&self.value(inner)?) {
                Some(Number::Int(n)) => Value::Int(n.checked_neg().ok_or_else(|| overflow("-"))?),
                Some(Number::Float(n)) => Value::Float(-n),
                None => {
                    return Err(Failure::Other(format!(
                        "{} is not a number, so it has no negative",
                        inner.describe()
                    )))
                }
            },
            Node::Choice(condition, yes, no) => {
                if self.truth(condition)? {
                    self.value(yes)?
                } else {
                    self.value(no)?
                }
            }
            Node::List(items) => Value::List(
                items
                    .iter()
                    .map(|item| self.value(item))
                    .collect::<Outcome<Vec<Value>>>()?,
            ),
            Node::Map(entries) => {
                let mut map = BTreeMap::new();
                for (key, value) in entries {
                    map.insert(key.clone(), self.value(value)?);
                }
                Value::Map(map)
            }
            Node::Binary(..) => self.chain(node)?,
            Node::Call(name, args) => self.call(name, args)?,
        })
    }

    /// Evaluates a path of field reads and indexes, `a.b[0].c`. Each step
    /// holds the one before it, so a long path is a deep tree; it is walked
    /// here with a list instead of a call per step, so its length costs no
    /// stack.
    fn path(&mut self, node: &Node) -> Outcome<Value> {
        let mut steps = Vec::new();
        let mut innermost = node;
        while let Node::Field(base, _) | Node::Index(base, _) = innermost {
            steps.push(innermost);
            innermost = base;
        }
        let mut holder = self.value(innermost)?;
        for step in steps.into_iter().rev() {
            holder = match step {
                Node::Field(base, key) => self.field(holder, step, base, key)?,
                Node::Index(base, index) => self.index(holder, step, base, index)?,
                _ => unreachable!("only fields and indexes are steps of a path"),
            };
        }
        Ok(holder)
    }

    /// `holder.key`, where `holder` is what `base` came to.
    fn field(&mut self, holder: Value, node: &Node, base: &Node, key: &str) -> Outcome<Value> {
        Ok(match holder {
            Value::Map(mut map) => match map.remove(key) {
                Some(value) => value,
                None => return Err(self.not_set(node)),
            },
            // `list.0` is `list[0]`.
            Value::List(mut items) if key.parse::<usize>().is_ok() => match key.parse::<usize>() {
                Ok(at) if at < items.len() => items.swap_remove(at),
                _ => return Err(self.not_set(node)),
            },
            other => {
                return Err(Failure::Other(format!(
                    "{} is {}, so it has no key {key:?}",
                    base.describe(),
                    other.describe()
                )))
            }
        })
    }

    /// `holder[index]`, where `holder` is what `base` came to.
    fn index(&mut self, holder: Value, node: &Node, base: &Node, index: &Node) -> Outcome<Value> {
        let index_value = self.value(index)?;
        Ok(match (holder, &index_value) {
            (Value::Map(mut map), Value::String(key)) => match map.remove(key) {
                Some(value) => value,
                None => return Err(self.not_set(node)),
            },
            (Value::List(mut items), index) if Number::of(index).is_some() => {
                let Some(Number::Int(n)) = Number::of(index) else {
                    return Err(Failure::Other(format!(
                        "an index into a list is a whole number, not {}",
                        index.canonical()
                    )));
                };
                // A negative index counts from the end.
                let len = items.len() as i64;
                let at = if n < 0 { len + n } else { n };
                if at < 0 || at >= len {
                    return Err(self.not_set(node));
                }
                items.swap_remove(at as usize)
            }
            (Value::Map(_), other) => {
                return Err(Failure::Other(format!(
                    "{} is a mapping, so it is indexed with text, not {}",
                    base.describe(),
                    other.describe()
                )))
            }
            (Value::List(_), other) => {
                return Err(Failure::Other(format!(
                    "{} is a list, so it is indexed with a number, not {}",
                    base.describe(),
                    other.describe()
                )))
            }
            (other, _) => {
                return Err(Failure::Other(format!(
                    "{} is {}, so it cannot be indexed",
                    base.describe(),
                    other.describe()
                )))
            }
        })
    }

    /// The value of `node`, or `None` when it is not set or is null: what
    /// `??`, `default()` and `coalesce()` fall through on.
    fn present(&mut self, node: &Node) -> Outcome<Option<Value>> {
        present_of(self.value(node))
    }

    /// Evaluates a run of binary operators. Operators of one precedence
    /// associate to the left, so `a + b + c` is a tree whose left side goes
    /// as deep as the chain is long; it is walked here with a list instead
    /// of a call per level, so a long chain costs no stack.
    fn chain(&mut self, node: &Node) -> Outcome<Value> {
        let mut spine = Vec::new();
        let mut leftmost = node;
        while let Node::Binary(op, left, right) = leftmost {
            spine.push((*op, &**left, &**right));
            leftmost = left;
        }
        let mut acc = self.value(leftmost);
        for (op, left, right) in spine.into_iter().rev() {
            // The right side is only decided when the left leaves the
            // answer open, so `defined(data.x) && data.x == 1` can guard
            // itself.
            acc = match op {
                Binary::And => {
                    let a = self.truth_of(acc, left)?;
                    Ok(Value::Bool(a && self.truth(right)?))
                }
                Binary::Or => {
                    let a = self.truth_of(acc, left)?;
                    Ok(Value::Bool(a || self.truth(right)?))
                }
                Binary::Coalesce => match present_of(acc)? {
                    Some(value) => Ok(value),
                    None => self.value(right),
                },
                _ => {
                    let a = acc?;
                    let b = self.value(right)?;
                    self.binary(op, a, b, left, right)
                }
            };
        }
        acc
    }

    /// Applies `op` to `a` and `b`, what `left` and `right` came to.
    fn binary(
        &mut self,
        op: Binary,
        a: Value,
        b: Value,
        left: &Node,
        right: &Node,
    ) -> Outcome<Value> {
        let mismatch = |verb: &str| {
            Failure::Other(format!(
                "cannot {verb} {} and {} with {}; convert one first, with toString or toNumber",
                a.describe(),
                b.describe(),
                op.symbol()
            ))
        };
        if matches!(a, Value::Secret(_)) || matches!(b, Value::Secret(_)) {
            return self.binary_with_secret(op, &a, &b);
        }
        Ok(match op {
            Binary::Equal => Value::Bool(same(&a, &b)),
            Binary::NotEqual => Value::Bool(!same(&a, &b)),
            Binary::Less | Binary::LessEqual | Binary::Greater | Binary::GreaterEqual => {
                let ordering = match (Number::of(&a), Number::of(&b), &a, &b) {
                    (Some(x), Some(y), _, _) => x.float().partial_cmp(&y.float()),
                    (_, _, Value::String(x), Value::String(y)) => Some(x.cmp(y)),
                    _ => return Err(mismatch("compare")),
                };
                let Some(ordering) = ordering else {
                    return Err(Failure::Other(
                        "a value that is not a number cannot be ordered".to_owned(),
                    ));
                };
                Value::Bool(match op {
                    Binary::Less => ordering.is_lt(),
                    Binary::LessEqual => ordering.is_le(),
                    Binary::Greater => ordering.is_gt(),
                    _ => ordering.is_ge(),
                })
            }
            Binary::In | Binary::NotIn => {
                let found = funcs::holds(&b, &a).map_err(|()| {
                    Failure::Other(if matches!(b, Value::String(_)) {
                        format!(
                            "{} looks for text inside text; {} is {}",
                            op.symbol(),
                            left.describe(),
                            a.describe()
                        )
                    } else {
                        format!(
                            "{} tests for an item of a list, a key of a mapping or text \
                             inside text; {} is {}",
                            op.symbol(),
                            right.describe(),
                            b.describe()
                        )
                    })
                })?;
                Value::Bool(found == (op == Binary::In))
            }
            Binary::Matches | Binary::NotMatches => {
                let (Value::String(text), Value::String(pattern)) = (&a, &b) else {
                    return Err(Failure::Other(format!(
                        "{} matches text against a pattern; it was given {} and {}",
                        op.symbol(),
                        a.describe(),
                        b.describe()
                    )));
                };
                let found = funcs::pattern(pattern)?.is_match(text);
                Value::Bool(found == (op == Binary::Matches))
            }
            Binary::Add => match (Number::of(&a), Number::of(&b), &a, &b) {
                (Some(Number::Int(x)), Some(Number::Int(y)), _, _) => {
                    Value::Int(x.checked_add(y).ok_or_else(|| overflow("+"))?)
                }
                (Some(x), Some(y), _, _) => Value::Float(x.float() + y.float()),
                (_, _, Value::String(x), Value::String(y)) => Value::String(format!("{x}{y}")),
                (_, _, Value::List(x), Value::List(y)) => {
                    Value::List(x.iter().chain(y).cloned().collect())
                }
                _ => return Err(mismatch("add")),
            },
            Binary::Subtract | Binary::Multiply => match (Number::of(&a), Number::of(&b)) {
                (Some(Number::Int(x)), Some(Number::Int(y))) => Value::Int(
                    if op == Binary::Subtract {
                        x.checked_sub(y)
                    } else {
                        x.checked_mul(y)
                    }
                    .ok_or_else(|| overflow(op.symbol()))?,
                ),
                (Some(x), Some(y)) => Value::Float(if op == Binary::Subtract {
                    x.float() - y.float()
                } else {
                    x.float() * y.float()
                }),
                _ => return Err(mismatch("calculate")),
            },
            Binary::Divide | Binary::Remainder => {
                let (Some(x), Some(y)) = (Number::of(&a), Number::of(&b)) else {
                    return Err(mismatch("calculate"));
                };
                if y.float() == 0.0 {
                    return Err(Failure::Other(format!(
                        "{} is divided by zero",
                        left.describe()
                    )));
                }
                match (x, y, op) {
                    // The one remainder that overflows, the smallest
                    // number by -1, is 0.
                    (Number::Int(x), Number::Int(y), Binary::Remainder) => {
                        Value::Int(x.checked_rem(y).unwrap_or(0))
                    }
                    // Whole numbers that divide exactly stay whole.
                    (Number::Int(x), Number::Int(y), _) if x % y == 0 => {
                        Value::Int(x.checked_div(y).ok_or_else(|| overflow("/"))?)
                    }
                    // A remainder of nothing is 0, never -0.
                    (x, y, Binary::Remainder) => Value::Float(x.float() % y.float() + 0.0),
                    (x, y, _) => Value::Float(x.float() / y.float()),
                }
            }
            Binary::And | Binary::Or | Binary::Coalesce => unreachable_op(),
        })
    }

    fn secrets(&self) -> Outcome<&SecretTable> {
        self.scope.secrets.as_deref().ok_or_else(|| {
            Failure::Other("a secret cannot be read here: this place holds no secrets".to_owned())
        })
    }

    /// An operator with a secret on either side. A secret can be compared,
    /// which gives a plain yes or no, and joined to text or another secret,
    /// which gives a secret. Nothing else is done to one.
    fn binary_with_secret(&self, op: Binary, a: &Value, b: &Value) -> Outcome<Value> {
        let table = self.secrets()?;
        let failed = |e: meridian_secret::TableError| Failure::Other(e.to_string());
        match op {
            Binary::Equal | Binary::NotEqual => {
                let equal = match (a, b) {
                    (Value::Secret(x), Value::Secret(y)) => table.same(*x, *y).map_err(failed)?,
                    (Value::Secret(x), Value::String(text))
                    | (Value::String(text), Value::Secret(x)) => {
                        table.same_as_text(*x, text).map_err(failed)?
                    }
                    // A secret is text; it is never a number, a list or null.
                    _ => false,
                };
                Ok(Value::Bool(equal == (op == Binary::Equal)))
            }
            Binary::Add => {
                let piece = |value: &'_ Value| match value {
                    Value::Secret(secret) => Some(PieceOwned::Secret(*secret)),
                    Value::String(text) => Some(PieceOwned::Text(text.clone())),
                    _ => None,
                };
                let (Some(x), Some(y)) = (piece(a), piece(b)) else {
                    return Err(Failure::Other(format!(
                        "cannot add {} and {} with +; a secret is joined to text or to another \
                         secret",
                        a.describe(),
                        b.describe()
                    )));
                };
                let joined = table.join(&[x.piece(), y.piece()]).map_err(failed)?;
                Ok(Value::Secret(joined))
            }
            _ => Err(Failure::Other(format!(
                "{} cannot be used on a secret; a secret can be compared with == or !=, joined \
                 to text with +, and given to {}",
                op.symbol(),
                Transform::FUNCTIONS.join(", ")
            ))),
        }
    }

    /// A function given a secret. Only the few that work on protected
    /// memory take one; the rest would have to copy it out.
    fn call_with_secret(&self, name: &str, values: &[Value]) -> Outcome<Value> {
        let table = self.secrets()?;
        match (Transform::of_function(name), values) {
            (Some(transform), [Value::Secret(secret)]) => table
                .transform(*secret, transform)
                .map(Value::Secret)
                .map_err(|e| Failure::Other(e.to_string())),
            // The value is handed back as it is, or the run stops.
            _ if name == "required" => Ok(funcs::call(name, values, self.scope)?),
            _ => Err(Failure::Other(format!(
                "{name}(...) cannot be given a secret; the functions that can are {}, and a \
                 secret can be joined to text with + or placed inside text",
                Transform::FUNCTIONS.join(", ")
            ))),
        }
    }

    fn call(&mut self, name: &str, args: &[Node]) -> Outcome<Value> {
        funcs::check_arity(name, args.len())?;
        if !LAZY.contains(&name) {
            let values = args
                .iter()
                .map(|arg| self.value(arg))
                .collect::<Outcome<Vec<Value>>>()?;
            if values.iter().any(holds_secret) {
                return self.call_with_secret(name, &values);
            }
            return Ok(funcs::call(name, &values, self.scope)?);
        }
        match name {
            "defined" => match self.value(&args[0]) {
                Ok(_) => Ok(Value::Bool(true)),
                Err(Failure::NotSet { .. }) => Ok(Value::Bool(false)),
                Err(other) => Err(other),
            },
            "coalesce" => {
                for arg in args {
                    if let Some(value) = self.present(arg)? {
                        return Ok(value);
                    }
                }
                Ok(Value::Null)
            }
            "default" => match self.present(&args[0])? {
                Some(value) => Ok(value),
                None => self.value(&args[1]),
            },
            // map and filter
            _ => {
                let Value::List(items) = self.value(&args[0])? else {
                    return Err(Failure::Other(format!(
                        "{name}(...) takes a list as its first argument; {} is not one",
                        args[0].describe()
                    )));
                };
                let Node::Literal(Value::String(item_name)) = &args[1] else {
                    return Err(Failure::Other(format!(
                        "the second argument of {name}(...) is the name each item is given, \
                         as quoted text"
                    )));
                };
                let mut out = Vec::with_capacity(items.len());
                for item in items {
                    self.bound.push((item_name.clone(), item));
                    let outcome = if name == "map" {
                        self.value(&args[2]).map(Some)
                    } else {
                        self.truth(&args[2]).map(|keep| keep.then_some(Value::Null))
                    };
                    let item = self.bound.pop().map_or(Value::Null, |(_, item)| item);
                    match outcome? {
                        Some(value) if name == "map" => out.push(value),
                        Some(_) => out.push(item),
                        None => {}
                    }
                }
                Ok(Value::List(out))
            }
        }
    }
}

/// The three operators handled before their operands are evaluated never
/// reach the table below them.
fn unreachable_op() -> Value {
    Value::Null
}
