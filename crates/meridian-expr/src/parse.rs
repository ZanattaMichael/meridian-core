//! Text to tree. Nothing is evaluated here.
//!
//! ```text
//! expr     := coalesce ( "?" expr ":" expr )?
//! coalesce := or ( "??" or )*
//! or       := and ( "||" and )*
//! and      := equal ( "&&" equal )*
//! equal    := compare ( ( "==" | "!=" ) compare )?
//! compare  := sum ( ( "<" | "<=" | ">" | ">=" | "in" | "not" "in" | "=~" | "!~" ) sum )?
//! sum      := product ( ( "+" | "-" ) product )*
//! product  := unary ( ( "*" | "/" | "%" ) unary )*
//! unary    := ( "!" | "-" ) unary | postfix
//! postfix  := primary ( "." name | "[" expr "]" )*
//! primary  := "(" expr ")" | literal | list | mapping | name "(" args ")" | name
//! ```

use std::fmt;

use meridian_value::Value;

/// How deep an expression may nest. Far more than anything written by hand,
/// and small enough that reading one can never exhaust the stack.
const MAX_DEPTH: usize = 64;

/// How many operators, field reads and indexes one expression may hold. A
/// chain such as `a || b || c` is not nested, so the depth limit does not
/// count it, but it builds a tree as long as the chain, and reading that
/// tree back is recursive: without this limit a long enough chain would
/// exhaust the stack instead of reporting an error.
const MAX_LINKS: usize = 1000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Unary {
    Not,
    Negate,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Binary {
    Or,
    And,
    Coalesce,
    Equal,
    NotEqual,
    Less,
    LessEqual,
    Greater,
    GreaterEqual,
    In,
    NotIn,
    Matches,
    NotMatches,
    Add,
    Subtract,
    Multiply,
    Divide,
    Remainder,
}

impl Binary {
    pub(crate) fn symbol(self) -> &'static str {
        match self {
            Binary::Or => "||",
            Binary::And => "&&",
            Binary::Coalesce => "??",
            Binary::Equal => "==",
            Binary::NotEqual => "!=",
            Binary::Less => "<",
            Binary::LessEqual => "<=",
            Binary::Greater => ">",
            Binary::GreaterEqual => ">=",
            Binary::In => "in",
            Binary::NotIn => "not in",
            Binary::Matches => "=~",
            Binary::NotMatches => "!~",
            Binary::Add => "+",
            Binary::Subtract => "-",
            Binary::Multiply => "*",
            Binary::Divide => "/",
            Binary::Remainder => "%",
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Node {
    Literal(Value),
    /// A bare name: a root such as `data`, or a name bound by `map`/`filter`.
    Name(String),
    Field(Box<Node>, String),
    Index(Box<Node>, Box<Node>),
    Call(String, Vec<Node>),
    Unary(Unary, Box<Node>),
    Binary(Binary, Box<Node>, Box<Node>),
    Choice(Box<Node>, Box<Node>, Box<Node>),
    List(Vec<Node>),
    Map(Vec<(String, Node)>),
}

impl Node {
    /// Whether this is a path: a name followed only by fields and indexes.
    pub(crate) fn is_path(&self) -> bool {
        match self {
            Node::Name(_) => true,
            Node::Field(base, _) | Node::Index(base, _) => base.is_path(),
            _ => false,
        }
    }

    /// The first name of a path.
    pub(crate) fn root(&self) -> Option<&str> {
        match self {
            Node::Name(name) => Some(name),
            Node::Field(base, _) | Node::Index(base, _) => base.root(),
            _ => None,
        }
    }

    /// How a diagnostic names this expression: a path as it was written,
    /// a literal as itself, anything else as "the expression".
    pub(crate) fn describe(&self) -> String {
        match self {
            Node::Name(name) => name.clone(),
            Node::Field(base, key) if base.is_path() => format!("{}.{key}", base.describe()),
            Node::Index(base, index) if base.is_path() => match index.as_ref() {
                Node::Literal(Value::String(key)) => format!("{}['{key}']", base.describe()),
                Node::Literal(value) => format!("{}[{}]", base.describe(), value.canonical()),
                _ => format!("{}[...]", base.describe()),
            },
            Node::Literal(value) => value.canonical(),
            _ => "the expression".to_owned(),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
enum Token {
    Open,
    Close,
    OpenList,
    CloseList,
    OpenMap,
    CloseMap,
    Comma,
    Colon,
    Question,
    Dot,
    Op(&'static str),
    Name(String),
    Number(Value),
    Text(String),
}

impl fmt::Display for Token {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let symbol = match self {
            Token::Open => "(",
            Token::Close => ")",
            Token::OpenList => "[",
            Token::CloseList => "]",
            Token::OpenMap => "{",
            Token::CloseMap => "}",
            Token::Comma => ",",
            Token::Colon => ":",
            Token::Question => "?",
            Token::Dot => ".",
            Token::Op(op) => op,
            Token::Name(name) => return write!(f, "{name:?}"),
            Token::Number(value) => return f.write_str(&value.canonical()),
            Token::Text(text) => return write!(f, "the text {text:?}"),
        };
        write!(f, "{symbol:?}")
    }
}

const TWO_CHAR: [&str; 9] = ["==", "!=", "<=", ">=", "&&", "||", "??", "=~", "!~"];

fn tokens(text: &str, what: &str) -> Result<Vec<Token>, String> {
    let chars: Vec<char> = text.chars().collect();
    let mut out: Vec<Token> = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        let after_dot = out.last() == Some(&Token::Dot);
        if c.is_whitespace() {
            i += 1;
            continue;
        }
        if let Some(next) = chars.get(i + 1) {
            let pair: String = [c, *next].iter().collect();
            if let Some(op) = TWO_CHAR.iter().find(|op| **op == pair) {
                out.push(Token::Op(op));
                i += 2;
                continue;
            }
        }
        let single = match c {
            '(' => Some(Token::Open),
            ')' => Some(Token::Close),
            '[' => Some(Token::OpenList),
            ']' => Some(Token::CloseList),
            '{' => Some(Token::OpenMap),
            '}' => Some(Token::CloseMap),
            ',' => Some(Token::Comma),
            ':' => Some(Token::Colon),
            '?' => Some(Token::Question),
            '.' => Some(Token::Dot),
            '!' => Some(Token::Op("!")),
            '<' => Some(Token::Op("<")),
            '>' => Some(Token::Op(">")),
            '+' => Some(Token::Op("+")),
            '-' => Some(Token::Op("-")),
            '*' => Some(Token::Op("*")),
            '/' => Some(Token::Op("/")),
            '%' => Some(Token::Op("%")),
            _ => None,
        };
        if let Some(token) = single {
            out.push(token);
            i += 1;
            continue;
        }
        match c {
            '\'' | '"' => {
                let mut value = String::new();
                let mut j = i + 1;
                loop {
                    match chars.get(j) {
                        None => return Err("a quoted text is never closed".to_owned()),
                        Some(&q) if q == c => break,
                        Some('\\') => {
                            let Some(&escaped) = chars.get(j + 1) else {
                                return Err("a quoted text is never closed".to_owned());
                            };
                            value.push(match escaped {
                                'n' => '\n',
                                't' => '\t',
                                'r' => '\r',
                                other => other,
                            });
                            j += 2;
                        }
                        Some(&other) => {
                            value.push(other);
                            j += 1;
                        }
                    }
                }
                out.push(Token::Text(value));
                i = j + 1;
            }
            c if c.is_ascii_digit() => {
                let start = i;
                i += 1;
                while i < chars.len()
                    && (chars[i].is_ascii_digit()
                        // After a "." a number is a key, as in data.list.0,
                        // so it never takes a fraction.
                        || (!after_dot
                            && chars[i] == '.'
                            && chars.get(i + 1).is_some_and(|n| n.is_ascii_digit())))
                {
                    i += 1;
                }
                let literal: String = chars[start..i].iter().collect();
                let value = if let Ok(n) = literal.parse::<i64>() {
                    Value::Int(n)
                } else if let Ok(n) = literal.parse::<f64>() {
                    Value::Float(n)
                } else {
                    return Err(format!("{literal:?} is not a number"));
                };
                out.push(Token::Number(value));
            }
            c if c.is_ascii_alphabetic() || c == '_' => {
                let start = i;
                while i < chars.len()
                    && (chars[i].is_ascii_alphanumeric()
                        || chars[i] == '_'
                        // A key may hold a hyphen, as in data.my-key. Only
                        // after a ".": anywhere else a "-" is a minus.
                        || (after_dot
                            && chars[i] == '-'
                            && chars
                                .get(i + 1)
                                .is_some_and(|n| n.is_ascii_alphanumeric() || *n == '_')))
                {
                    i += 1;
                }
                out.push(Token::Name(chars[start..i].iter().collect()));
            }
            '=' => {
                return Err(format!(
                    "unexpected \"=\" (to compare two values in a {what}, write ==)"
                ))
            }
            other => {
                return Err(format!(
                    "unexpected {:?} (a {what} is built from names such as data.<key>, true, \
                     false, null, numbers, quoted text, lists, mappings, function calls, and \
                     the operators ! - + * / % == != < <= > >= in =~ !~ && || ?? and ? :)",
                    other.to_string()
                ))
            }
        }
    }
    Ok(out)
}

struct Parser<'a> {
    tokens: Vec<Token>,
    at: usize,
    depth: usize,
    /// Operators and postfixes read so far, against `MAX_LINKS`.
    links: usize,
    what: &'a str,
}

impl Parser<'_> {
    fn peek(&self) -> Option<&Token> {
        self.tokens.get(self.at)
    }

    fn take(&mut self, token: &Token) -> bool {
        if self.peek() == Some(token) {
            self.at += 1;
            true
        } else {
            false
        }
    }

    fn take_op(&mut self, ops: &[&'static str]) -> Option<&'static str> {
        if let Some(Token::Op(op)) = self.peek() {
            if let Some(found) = ops.iter().find(|candidate| *candidate == op) {
                self.at += 1;
                return Some(found);
            }
        }
        None
    }

    fn take_word(&mut self, word: &str) -> bool {
        if matches!(self.peek(), Some(Token::Name(name)) if name == word) {
            self.at += 1;
            true
        } else {
            false
        }
    }

    fn expect(&mut self, token: Token) -> Result<(), String> {
        if self.take(&token) {
            return Ok(());
        }
        Err(match self.peek() {
            Some(found) => format!("expected {token}, found {found}"),
            None => format!("expected {token}, but the {} ends", self.what),
        })
    }

    fn nested<T>(&mut self, f: impl FnOnce(&mut Self) -> Result<T, String>) -> Result<T, String> {
        self.depth += 1;
        if self.depth > MAX_DEPTH {
            return Err(format!(
                "the {} is nested more than {MAX_DEPTH} levels deep",
                self.what
            ));
        }
        let out = f(self);
        self.depth -= 1;
        out
    }

    /// Counts one more operator, field read or index.
    fn link(&mut self) -> Result<(), String> {
        self.links += 1;
        if self.links > MAX_LINKS {
            return Err(format!(
                "the {} has more than {MAX_LINKS} operators",
                self.what
            ));
        }
        Ok(())
    }

    fn expr(&mut self) -> Result<Node, String> {
        self.nested(|p| {
            let condition = p.coalesce()?;
            if !p.take(&Token::Question) {
                return Ok(condition);
            }
            let yes = p.expr()?;
            p.expect(Token::Colon)?;
            let no = p.expr()?;
            Ok(Node::Choice(
                Box::new(condition),
                Box::new(yes),
                Box::new(no),
            ))
        })
    }

    fn chain(
        &mut self,
        ops: &[(&'static str, Binary)],
        next: fn(&mut Self) -> Result<Node, String>,
    ) -> Result<Node, String> {
        let symbols: Vec<&'static str> = ops.iter().map(|(symbol, _)| *symbol).collect();
        let mut left = next(self)?;
        while let Some(symbol) = self.take_op(&symbols) {
            self.link()?;
            let op = ops.iter().find(|(s, _)| *s == symbol).map(|(_, op)| *op);
            let right = next(self)?;
            left = Node::Binary(op.unwrap_or(Binary::Or), Box::new(left), Box::new(right));
        }
        Ok(left)
    }

    fn coalesce(&mut self) -> Result<Node, String> {
        self.chain(&[("??", Binary::Coalesce)], Self::or)
    }

    fn or(&mut self) -> Result<Node, String> {
        self.chain(&[("||", Binary::Or)], Self::and)
    }

    fn and(&mut self) -> Result<Node, String> {
        self.chain(&[("&&", Binary::And)], Self::equal)
    }

    fn equal(&mut self) -> Result<Node, String> {
        let left = self.compare()?;
        let op = match self.take_op(&["==", "!="]) {
            Some("==") => Binary::Equal,
            Some(_) => Binary::NotEqual,
            None => return Ok(left),
        };
        Ok(Node::Binary(op, Box::new(left), Box::new(self.compare()?)))
    }

    fn compare(&mut self) -> Result<Node, String> {
        let left = self.sum()?;
        let op = if let Some(symbol) = self.take_op(&["<=", ">=", "<", ">", "=~", "!~"]) {
            match symbol {
                "<=" => Binary::LessEqual,
                ">=" => Binary::GreaterEqual,
                "<" => Binary::Less,
                ">" => Binary::Greater,
                "=~" => Binary::Matches,
                _ => Binary::NotMatches,
            }
        } else if self.take_word("in") {
            Binary::In
        } else if matches!(self.peek(), Some(Token::Name(name)) if name == "not")
            && matches!(self.tokens.get(self.at + 1), Some(Token::Name(name)) if name == "in")
        {
            self.at += 2;
            Binary::NotIn
        } else {
            return Ok(left);
        };
        Ok(Node::Binary(op, Box::new(left), Box::new(self.sum()?)))
    }

    fn sum(&mut self) -> Result<Node, String> {
        self.chain(
            &[("+", Binary::Add), ("-", Binary::Subtract)],
            Self::product,
        )
    }

    fn product(&mut self) -> Result<Node, String> {
        self.chain(
            &[
                ("*", Binary::Multiply),
                ("/", Binary::Divide),
                ("%", Binary::Remainder),
            ],
            Self::unary,
        )
    }

    fn unary(&mut self) -> Result<Node, String> {
        self.nested(|p| {
            if p.take_op(&["!"]).is_some() {
                return Ok(Node::Unary(Unary::Not, Box::new(p.unary()?)));
            }
            if p.take_op(&["-"]).is_some() {
                // A minus in front of a number is that number.
                return Ok(match p.unary()? {
                    Node::Literal(Value::Int(n)) => Node::Literal(Value::Int(-n)),
                    Node::Literal(Value::Float(n)) => Node::Literal(Value::Float(-n)),
                    other => Node::Unary(Unary::Negate, Box::new(other)),
                });
            }
            p.postfix()
        })
    }

    fn postfix(&mut self) -> Result<Node, String> {
        let mut node = self.primary()?;
        loop {
            if self.take(&Token::Dot) {
                self.link()?;
                match self.peek().cloned() {
                    Some(Token::Name(key)) => {
                        self.at += 1;
                        node = Node::Field(Box::new(node), key);
                    }
                    // A key that is all digits arrives as a number.
                    Some(Token::Number(Value::Int(n))) if n >= 0 => {
                        self.at += 1;
                        node = Node::Field(Box::new(node), n.to_string());
                    }
                    _ => return Err("a \".\" must be followed by a key".to_owned()),
                }
            } else if self.take(&Token::OpenList) {
                self.link()?;
                let index = self.expr()?;
                self.expect(Token::CloseList)?;
                node = Node::Index(Box::new(node), Box::new(index));
            } else {
                return Ok(node);
            }
        }
    }

    fn primary(&mut self) -> Result<Node, String> {
        let Some(token) = self.peek().cloned() else {
            return Err(format!("the {} ends where a value was expected", self.what));
        };
        self.at += 1;
        match token {
            Token::Open => {
                let inner = self.expr()?;
                self.expect(Token::Close)?;
                Ok(inner)
            }
            Token::OpenList => {
                let mut items = Vec::new();
                if !self.take(&Token::CloseList) {
                    loop {
                        items.push(self.expr()?);
                        if self.take(&Token::CloseList) {
                            break;
                        }
                        self.expect(Token::Comma)?;
                        // A comma may follow the last item.
                        if self.take(&Token::CloseList) {
                            break;
                        }
                    }
                }
                Ok(Node::List(items))
            }
            Token::OpenMap => {
                let mut entries: Vec<(String, Node)> = Vec::new();
                if !self.take(&Token::CloseMap) {
                    loop {
                        let key = match self.peek().cloned() {
                            Some(Token::Name(key)) | Some(Token::Text(key)) => key,
                            Some(other) => {
                                return Err(format!(
                                    "a key of a mapping is a name or quoted text, found {other}"
                                ))
                            }
                            None => {
                                return Err(format!(
                                    "expected a key or \"}}\", but the {} ends",
                                    self.what
                                ))
                            }
                        };
                        self.at += 1;
                        if entries.iter().any(|(existing, _)| *existing == key) {
                            return Err(format!("the key {key:?} is set twice in one mapping"));
                        }
                        self.expect(Token::Colon)?;
                        entries.push((key, self.expr()?));
                        if self.take(&Token::CloseMap) {
                            break;
                        }
                        self.expect(Token::Comma)?;
                        if self.take(&Token::CloseMap) {
                            break;
                        }
                    }
                }
                Ok(Node::Map(entries))
            }
            Token::Number(value) => Ok(Node::Literal(value)),
            Token::Text(text) => Ok(Node::Literal(Value::String(text))),
            Token::Name(name) => match name.as_str() {
                "true" => Ok(Node::Literal(Value::Bool(true))),
                "false" => Ok(Node::Literal(Value::Bool(false))),
                "null" => Ok(Node::Literal(Value::Null)),
                _ if self.take(&Token::Open) => {
                    let mut args = Vec::new();
                    if !self.take(&Token::Close) {
                        loop {
                            args.push(self.expr()?);
                            if self.take(&Token::Close) {
                                break;
                            }
                            self.expect(Token::Comma)?;
                        }
                    }
                    Ok(Node::Call(name, args))
                }
                _ => Ok(Node::Name(name)),
            },
            other => Err(format!("unexpected {other} where a value was expected")),
        }
    }
}

/// Reads `text` as one expression. `what` is the word diagnostics use for it:
/// "condition" for a `when`, "expression" elsewhere.
pub(crate) fn parse(text: &str, what: &str) -> Result<Node, String> {
    let mut parser = Parser {
        tokens: tokens(text, what)?,
        at: 0,
        depth: 0,
        links: 0,
        what,
    };
    let node = parser.expr()?;
    if let Some(extra) = parser.peek() {
        return Err(format!("unexpected {extra} after the end of the {what}"));
    }
    Ok(node)
}
