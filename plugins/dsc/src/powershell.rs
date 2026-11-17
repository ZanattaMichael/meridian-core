//! Reading a PowerShell `Configuration` block.
//!
//! A configuration script is a program, and this is not a PowerShell
//! interpreter. It reads the part of the language a configuration is
//! declared in: the `Configuration` block, `Import-DscResource`, `Node`, and
//! resource blocks whose properties are literals (strings, numbers,
//! booleans, arrays and hashtables). Everything else is handed back as the
//! text it was written as, with the line it is on, so the importer can
//! report it instead of guessing what it would have evaluated to.

use std::collections::BTreeMap;

use meridian_sdk::Value;

/// One `Configuration` block.
#[derive(Debug, Default, PartialEq)]
pub(crate) struct Configuration {
    pub name: String,
    pub line: usize,
    /// `Import-DscResource`: the modules, and the resources each was asked
    /// for by name, if it was asked for any.
    pub imports: Vec<Import>,
    /// The `Node` blocks. Resources written outside one are in a node whose
    /// `host` is `None` and whose `line` is the configuration's.
    pub nodes: Vec<NodeBlock>,
    /// What was not read: the line and the statement's text.
    pub skipped: Vec<(usize, String)>,
    pub has_params: bool,
}

#[derive(Debug, Default, PartialEq)]
pub(crate) struct Import {
    pub modules: Vec<String>,
    pub names: Vec<String>,
}

#[derive(Debug, Default, PartialEq)]
pub(crate) struct NodeBlock {
    pub line: usize,
    /// The one machine named, when the block names one with a literal.
    pub host: Option<String>,
    /// What the block was written for, as text.
    pub selector: String,
    pub resources: Vec<Block>,
}

/// A property's value, or, for one that is not a literal, the text it was
/// written as.
pub(crate) type Property = Result<Value, String>;

/// One resource block.
#[derive(Debug, Default, PartialEq)]
pub(crate) struct Block {
    pub line: usize,
    pub resource: String,
    pub name: String,
    /// In the order written.
    pub properties: Vec<(String, Property)>,
}

/// A script: its configurations, and the lines outside any of them.
#[derive(Debug, Default, PartialEq)]
pub(crate) struct Script {
    pub configurations: Vec<Configuration>,
    pub outside: Vec<usize>,
}

pub(crate) fn parse(text: &str) -> Result<Script, (usize, String)> {
    let mut p = Parser {
        chars: text.chars().collect(),
        pos: 0,
        line: 1,
    };
    let mut script = Script::default();
    loop {
        p.skip(true);
        if p.at_end() {
            break;
        }
        let line = p.line;
        let start = p.pos;
        let word = p.word();
        if word.eq_ignore_ascii_case("configuration") {
            p.skip(false);
            let name = p.word();
            p.skip(true);
            if name.is_empty() || !p.eat('{') {
                return Err((line, "a Configuration needs a name and a block".to_owned()));
            }
            let mut configuration = Configuration {
                name,
                line,
                ..Configuration::default()
            };
            let mut loose = NodeBlock {
                line,
                ..NodeBlock::default()
            };
            p.body(&mut configuration, &mut loose, true)?;
            if !loose.resources.is_empty() {
                configuration.nodes.insert(0, loose);
            }
            script.configurations.push(configuration);
        } else {
            p.pos = start;
            p.line = line;
            p.statement();
            script.outside.push(line);
        }
    }
    Ok(script)
}

struct Parser {
    chars: Vec<char>,
    pos: usize,
    line: usize,
}

impl Parser {
    fn at_end(&self) -> bool {
        self.pos >= self.chars.len()
    }

    fn peek(&self) -> Option<char> {
        self.chars.get(self.pos).copied()
    }

    fn peek_at(&self, offset: usize) -> Option<char> {
        self.chars.get(self.pos + offset).copied()
    }

    fn bump(&mut self) -> Option<char> {
        let c = self.peek()?;
        self.pos += 1;
        if c == '\n' {
            self.line += 1;
        }
        Some(c)
    }

    fn eat(&mut self, c: char) -> bool {
        if self.peek() == Some(c) {
            self.bump();
            true
        } else {
            false
        }
    }

    /// Skips blanks, comments and line continuations, and line ends as well
    /// when `lines` is set.
    fn skip(&mut self, lines: bool) {
        loop {
            match self.peek() {
                Some(' ' | '\t' | '\r') => {
                    self.bump();
                }
                Some('\n' | ';') if lines => {
                    self.bump();
                }
                Some('`') if matches!(self.peek_at(1), Some('\n' | '\r')) => {
                    self.bump();
                    while self.peek() == Some('\r') {
                        self.bump();
                    }
                    self.eat('\n');
                }
                Some('#') => {
                    while !matches!(self.peek(), None | Some('\n')) {
                        self.bump();
                    }
                }
                Some('<') if self.peek_at(1) == Some('#') => {
                    while !self.at_end()
                        && !(self.peek() == Some('#') && self.peek_at(1) == Some('>'))
                    {
                        self.bump();
                    }
                    self.bump();
                    self.bump();
                }
                _ => return,
            }
        }
    }

    /// A bare word: a keyword, a resource's name, a parameter.
    fn word(&mut self) -> String {
        let mut out = String::new();
        while let Some(c) = self.peek() {
            if c.is_alphanumeric() || "_-.:\\".contains(c) {
                out.push(c);
                self.bump();
            } else {
                break;
            }
        }
        out
    }

    fn at_statement_end(&self) -> bool {
        matches!(self.peek(), None | Some('\n' | ';' | '}'))
    }

    /// Consumes one statement, whatever it is, and returns its text. Brackets
    /// and strings are followed, so a block is part of its statement.
    fn statement(&mut self) -> String {
        let start = self.pos;
        let mut depth = 0usize;
        while let Some(c) = self.peek() {
            match c {
                '\'' | '"' => {
                    let _ = self.string();
                    continue;
                }
                '@' if matches!(self.peek_at(1), Some('\'' | '"'))
                    && matches!(self.peek_at(2), Some('\n' | '\r')) =>
                {
                    let _ = self.here_string();
                    continue;
                }
                '#' => {
                    self.skip(false);
                    continue;
                }
                '`' => {
                    self.bump();
                }
                '{' | '(' | '[' => depth += 1,
                '}' | ')' | ']' => {
                    if depth == 0 {
                        break;
                    }
                    depth -= 1;
                    if depth == 0 && c == '}' {
                        // A block ends the statement it belongs to.
                        self.bump();
                        break;
                    }
                }
                '\n' | ';' if depth == 0 => break,
                _ => {}
            }
            self.bump();
        }
        let text: String = self.chars[start..self.pos].iter().collect();
        text.split_whitespace().collect::<Vec<_>>().join(" ")
    }

    /// The statements of a configuration or of a node, up to its `}`.
    fn body(
        &mut self,
        configuration: &mut Configuration,
        node: &mut NodeBlock,
        top: bool,
    ) -> Result<(), (usize, String)> {
        loop {
            self.skip(true);
            if self.at_end() {
                return Err((self.line, "the block is never closed".to_owned()));
            }
            if self.eat('}') {
                return Ok(());
            }
            let (line, start) = (self.line, self.pos);
            let word = self.word();
            let lower = word.to_ascii_lowercase();
            let skip = |p: &mut Parser, configuration: &mut Configuration| {
                p.pos = start;
                p.line = line;
                let text = p.statement();
                if p.pos == start {
                    // Nothing a statement can start with; step over it.
                    p.bump();
                }
                configuration.skipped.push((line, text));
            };
            match lower.as_str() {
                "param" => {
                    self.pos = start;
                    self.line = line;
                    self.statement();
                    configuration.has_params = true;
                }
                "import-dscresource" => {
                    let text = self.statement();
                    configuration.imports.push(import(&text));
                }
                "node" if top => {
                    self.skip(false);
                    let selector_start = self.pos;
                    let host = match self.peek() {
                        Some('\'' | '"') => self.string().ok(),
                        Some(c) if c.is_alphanumeric() => Some(self.word()),
                        _ => None,
                    };
                    self.skip(true);
                    let host = if self.peek() == Some('{') {
                        host
                    } else {
                        // Not one literal: a variable, a list, an expression.
                        let mut depth = 0usize;
                        while let Some(c) = self.peek() {
                            match c {
                                '(' => depth += 1,
                                ')' => depth = depth.saturating_sub(1),
                                '{' if depth == 0 => break,
                                _ => {}
                            }
                            self.bump();
                        }
                        None
                    };
                    let selector: String = self.chars[selector_start..self.pos].iter().collect();
                    if !self.eat('{') {
                        return Err((line, "a Node needs a block".to_owned()));
                    }
                    let mut block = NodeBlock {
                        line,
                        host,
                        selector: selector.trim().to_owned(),
                        resources: Vec::new(),
                    };
                    self.body(configuration, &mut block, false)?;
                    configuration.nodes.push(block);
                }
                "" | "if" | "else" | "elseif" | "foreach" | "for" | "while" | "switch" | "do"
                | "try" | "catch" | "finally" | "function" | "return" => {
                    skip(self, configuration);
                }
                _ => {
                    // A resource block: its type, its name, its properties.
                    self.skip(false);
                    let name = match self.peek() {
                        Some('\'' | '"') => self.string().ok(),
                        Some(c) if c.is_alphanumeric() || c == '_' => Some(self.word()),
                        _ => None,
                    };
                    self.skip(true);
                    match name {
                        Some(name) if self.eat('{') => {
                            let properties = self.properties()?;
                            node.resources.push(Block {
                                line,
                                resource: word,
                                name,
                                properties,
                            });
                        }
                        _ => skip(self, configuration),
                    }
                }
            }
        }
    }

    /// `Name = value` pairs up to the closing `}`.
    fn properties(&mut self) -> Result<Vec<(String, Property)>, (usize, String)> {
        let mut out = Vec::new();
        loop {
            self.skip(true);
            if self.at_end() {
                return Err((self.line, "the resource block is never closed".to_owned()));
            }
            if self.eat('}') {
                return Ok(out);
            }
            let line = self.line;
            let key = self.word();
            self.skip(false);
            if key.is_empty() || !self.eat('=') {
                return Err((
                    line,
                    "a resource block holds `Property = value` lines, and this is not one"
                        .to_owned(),
                ));
            }
            self.skip(false);
            let (start, start_line) = (self.pos, self.line);
            let value = self.value().and_then(|value| {
                self.skip(false);
                if self.at_statement_end() {
                    Ok(value)
                } else {
                    Err(())
                }
            });
            let value = value.map_err(|()| {
                self.pos = start;
                self.line = start_line;
                self.statement()
            });
            out.push((key, value));
        }
    }

    /// A literal, or a comma-separated list of them.
    fn value(&mut self) -> Result<Value, ()> {
        let first = self.one()?;
        self.skip(false);
        if self.peek() != Some(',') {
            return Ok(first);
        }
        let mut items = vec![first];
        while self.eat(',') {
            self.skip(true);
            items.push(self.one()?);
            self.skip(false);
        }
        Ok(Value::List(items))
    }

    fn one(&mut self) -> Result<Value, ()> {
        match self.peek().ok_or(())? {
            '\'' | '"' => self.string().map(Value::String),
            '@' => match self.peek_at(1) {
                Some('\'' | '"') => self.here_string().map(Value::String),
                Some('(') => {
                    self.bump();
                    self.bump();
                    let mut items = Vec::new();
                    loop {
                        self.skip(true);
                        if self.eat(')') {
                            return Ok(Value::List(items));
                        }
                        items.push(self.one()?);
                        self.skip(true);
                        self.eat(',');
                    }
                }
                Some('{') => {
                    self.bump();
                    self.bump();
                    let mut map = BTreeMap::new();
                    loop {
                        self.skip(true);
                        if self.eat('}') {
                            return Ok(Value::Map(map));
                        }
                        let key = match self.peek() {
                            Some('\'' | '"') => self.string()?,
                            _ => self.word(),
                        };
                        self.skip(false);
                        if key.is_empty() || !self.eat('=') {
                            return Err(());
                        }
                        self.skip(false);
                        map.insert(key, self.value()?);
                    }
                }
                _ => Err(()),
            },
            '$' => {
                self.bump();
                let word = self.word().to_ascii_lowercase();
                match word.as_str() {
                    "true" => Ok(Value::Bool(true)),
                    "false" => Ok(Value::Bool(false)),
                    "null" => Ok(Value::Null),
                    _ => Err(()),
                }
            }
            c if c.is_ascii_digit() || c == '-' => {
                let word = self.word();
                let lower = word.to_ascii_lowercase();
                let (digits, scale) = ["kb", "mb", "gb", "tb"]
                    .iter()
                    .zip([1i64 << 10, 1 << 20, 1 << 30, 1 << 40])
                    .find_map(|(suffix, scale)| Some((lower.strip_suffix(suffix)?, scale)))
                    .unwrap_or((lower.as_str(), 1));
                if let Some(hex) = digits.strip_prefix("0x") {
                    return i64::from_str_radix(hex, 16).map(Value::Int).map_err(|_| ());
                }
                if let Ok(i) = digits.parse::<i64>() {
                    return i.checked_mul(scale).map(Value::Int).ok_or(());
                }
                match digits.parse::<f64>() {
                    Ok(f) if scale == 1 && f.is_finite() => Ok(Value::Float(f)),
                    _ => Err(()),
                }
            }
            _ => Err(()),
        }
    }

    /// A quoted string. A double-quoted one that expands a variable or an
    /// expression is not a literal.
    fn string(&mut self) -> Result<String, ()> {
        let quote = self.bump().ok_or(())?;
        let mut out = String::new();
        let mut literal = true;
        loop {
            let c = self.bump().ok_or(())?;
            if c == quote {
                // A doubled quote is one quote.
                if self.peek() == Some(quote) {
                    self.bump();
                    out.push(quote);
                    continue;
                }
                break;
            }
            if quote == '"' {
                if c == '`' {
                    out.push(escape(self.bump().ok_or(())?));
                    continue;
                }
                if c == '$'
                    && self
                        .peek()
                        .is_some_and(|n| n.is_alphabetic() || "({_".contains(n))
                {
                    literal = false;
                }
            }
            out.push(c);
        }
        if literal {
            Ok(out)
        } else {
            Err(())
        }
    }

    /// `@'` or `@"`, the lines that follow, and the closing mark at the start
    /// of a line.
    fn here_string(&mut self) -> Result<String, ()> {
        self.bump();
        let quote = self.bump().ok_or(())?;
        while self.peek() == Some('\r') {
            self.bump();
        }
        if !self.eat('\n') {
            return Err(());
        }
        let mut lines: Vec<String> = Vec::new();
        loop {
            if self.peek() == Some(quote) && self.peek_at(1) == Some('@') {
                self.bump();
                self.bump();
                break;
            }
            let mut line = String::new();
            loop {
                match self.bump().ok_or(())? {
                    '\n' => break,
                    '\r' => {}
                    c => line.push(c),
                }
            }
            lines.push(line);
        }
        let text = lines.join("\n");
        if quote == '"' && text.contains('$') {
            return Err(());
        }
        Ok(text)
    }
}

fn escape(c: char) -> char {
    match c {
        'n' => '\n',
        't' => '\t',
        'r' => '\r',
        '0' => '\0',
        other => other,
    }
}

/// The arguments of one `Import-DscResource`.
fn import(statement: &str) -> Import {
    let mut out = Import::default();
    if statement.contains("@{") {
        // The form that pins a version: only the module's name is wanted.
        let lower = statement.to_ascii_lowercase();
        let mut from = 0;
        while let Some(at) = lower[from..].find("modulename") {
            let rest = &statement[from + at + "modulename".len()..];
            let rest = rest.trim_start().trim_start_matches('=').trim_start();
            let name: String = rest
                .trim_start_matches(['\'', '"'])
                .chars()
                .take_while(|c| c.is_alphanumeric() || "._-".contains(*c))
                .collect();
            if !name.is_empty() {
                out.modules.push(name);
            }
            from += at + "modulename".len();
        }
        return out;
    }
    let mut into: Option<bool> = Some(true);
    let cleaned = statement.replace(',', " ");
    for word in cleaned.split_whitespace().skip(1) {
        if let Some(parameter) = word.strip_prefix('-') {
            let parameter = parameter.to_ascii_lowercase();
            into = if "modulename".starts_with(&parameter) && parameter.starts_with('m') {
                Some(true)
            } else if "name".starts_with(&parameter) {
                Some(false)
            } else {
                // -ModuleVersion and the like: their values are not names.
                None
            };
            continue;
        }
        let name = word.trim_matches(['\'', '"']).to_owned();
        match into {
            Some(true) => out.modules.push(name),
            Some(false) => out.names.push(name),
            None => {}
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_configuration_is_read_block_by_block() {
        let script = parse(
            r#"
# A comment.
Configuration Web {
    param([string] $Role)
    Import-DscResource -ModuleName PSDscResources, 'WebAdministrationDsc'
    Import-DscResource -Module xOther -Name xThing -ModuleVersion 1.2

    Node 'web01' {
        WindowsFeature IIS {
            Ensure = 'Present'; Name = "Web-Server"
        }
        File "Root" {
            DestinationPath = 'C:\it''s'   # with a comment
            Size      = 2kb
            Force     = $true
            Tags      = 'a', 'b'
            More      = @( 1
                           2 )
            Table     = @{ One = 1; Two = 'two' }
            Path      = "$env:TEMP\x"
            Sum       = 1 + 2
            DependsOn = '[WindowsFeature]IIS'
        }
        if ($Role -eq 'web') {
            Log Note { Message = 'x' }
        }
    }
}
Web -OutputPath .\out
"#,
        )
        .unwrap();
        assert_eq!(script.outside, [29]);
        let configuration = &script.configurations[0];
        assert_eq!(configuration.name, "Web");
        assert!(configuration.has_params);
        assert_eq!(
            configuration.imports[0].modules,
            ["PSDscResources", "WebAdministrationDsc"]
        );
        assert_eq!(configuration.imports[1].modules, ["xOther"]);
        assert_eq!(configuration.imports[1].names, ["xThing"]);
        assert_eq!(configuration.skipped.len(), 1);
        assert_eq!(configuration.skipped[0].0, 24);

        let node = &configuration.nodes[0];
        assert_eq!(node.host.as_deref(), Some("web01"));
        assert_eq!(node.resources.len(), 2);
        let root = &node.resources[1];
        assert_eq!(
            (root.resource.as_str(), root.name.as_str()),
            ("File", "Root")
        );
        let shown: Vec<String> = root
            .properties
            .iter()
            .map(|(key, value)| match value {
                Ok(value) => format!("{key}={}", value.canonical()),
                Err(raw) => format!("{key}!{raw}"),
            })
            .collect();
        assert_eq!(
            shown,
            [
                r#"DestinationPath="C:\\it's""#,
                "Size=2048",
                "Force=true",
                r#"Tags=["a","b"]"#,
                "More=[1,2]",
                r#"Table={"One":1,"Two":"two"}"#,
                r#"Path!"$env:TEMP\x""#,
                "Sum!1 + 2",
                r#"DependsOn="[WindowsFeature]IIS""#,
            ]
        );
    }

    #[test]
    fn here_strings_and_nodes_that_are_not_one_name() {
        let script = parse(
            "Configuration C {\n  Node $AllNodes.NodeName {\n    File F {\n      Contents = @'\nline one\n  $two\n'@\n    }\n  }\n  Log L { Message = 'outside a node' }\n}\n",
        )
        .unwrap();
        let configuration = &script.configurations[0];
        assert_eq!(configuration.nodes.len(), 2);
        assert_eq!(configuration.nodes[0].host, None);
        assert_eq!(configuration.nodes[0].resources[0].name, "L");
        assert_eq!(configuration.nodes[1].selector, "$AllNodes.NodeName");
        assert_eq!(
            configuration.nodes[1].resources[0].properties[0].1,
            Ok(Value::String("line one\n  $two".to_owned()))
        );
    }

    #[test]
    fn a_block_left_open_is_an_error_with_its_line() {
        let err = parse("Configuration C {\n  File F {\n    A = 1\n").unwrap_err();
        assert_eq!(err.0, 4);
    }
}
