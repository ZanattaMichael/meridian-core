//! A forgiving, positioned reading of a YAML file.
//!
//! The compiler's own parser answers "what does this document say" and stops
//! at the first syntax error. An editor asks different questions: what is
//! under the cursor in a file that is half typed, where does this field
//! start and end, which comment belongs to which entry. This module answers
//! those. It reads block-style YAML by indentation into a tree that keeps
//! every line, and it never fails: text it does not understand becomes an
//! opaque scalar.
//!
//! It is not a second parser for the language. Nothing here decides what a
//! document means; that is always the compiler's answer. This tree only says
//! where things are.

/// A point in a file. Both are 0-based; `col` counts bytes within the line.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Pos {
    pub line: usize,
    pub col: usize,
}

impl Pos {
    pub fn new(line: usize, col: usize) -> Pos {
        Pos { line, col }
    }
}

/// A stretch of a file, from `start` up to but not including `end`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Span {
    pub start: Pos,
    pub end: Pos,
}

impl Span {
    pub fn on_line(line: usize, start: usize, end: usize) -> Span {
        Span {
            start: Pos::new(line, start),
            end: Pos::new(line, end),
        }
    }

    pub fn contains(&self, pos: Pos) -> bool {
        self.start <= pos && pos <= self.end
    }
}

/// One step of a path into a document.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Seg {
    Key(String),
    Index(usize),
}

/// Reads a dotted path as the compiler writes them in diagnostics, such as
/// `spec.resources[1].dependsOn[0].resource`.
pub fn parse_path(path: &str) -> Vec<Seg> {
    let mut segs = Vec::new();
    for part in path.split('.').filter(|p| !p.is_empty()) {
        let (name, mut rest) = match part.find('[') {
            Some(at) => (&part[..at], &part[at..]),
            None => (part, ""),
        };
        if !name.is_empty() {
            segs.push(Seg::Key(name.to_owned()));
        }
        while let Some(close) = rest.find(']') {
            if let Ok(index) = rest[1..close].parse() {
                segs.push(Seg::Index(index));
            }
            rest = &rest[close + 1..];
        }
    }
    segs
}

/// Writes a path the way [`parse_path`] reads it.
pub fn show_path(path: &[Seg]) -> String {
    let mut out = String::new();
    for seg in path {
        match seg {
            Seg::Key(key) => {
                if !out.is_empty() {
                    out.push('.');
                }
                out.push_str(key);
            }
            Seg::Index(index) => out.push_str(&format!("[{index}]")),
        }
    }
    out
}

/// How a scalar is written.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Style {
    Plain,
    Quoted,
    /// A `|` or `>` block.
    Block,
    /// A `{...}` or `[...]` flow collection, kept as text.
    Flow,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Scalar {
    /// The value as written on its first line, without quotes or a trailing
    /// comment. For a block or multi-line scalar this is only the first line.
    pub text: String,
    /// Where the value's text sits on its first line.
    pub span: Span,
    pub style: Style,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    /// The comment and blank lines between the previous entry and this one.
    pub leading: Vec<usize>,
    pub key: String,
    pub key_span: Span,
    /// The column of the `:`, when one has been typed.
    pub colon: Option<usize>,
    pub value: Option<Node>,
    /// The line the key is on.
    pub line: usize,
    /// The line after the entry's last line.
    pub end_line: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Item {
    pub leading: Vec<usize>,
    pub line: usize,
    pub dash_col: usize,
    pub value: Option<Node>,
    pub end_line: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NodeKind {
    Map(Vec<Entry>),
    Seq(Vec<Item>),
    Scalar(Scalar),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Node {
    /// The column the node's entries, items or text start at.
    pub indent: usize,
    pub line: usize,
    /// The line after the node's last line, trailing comments included.
    pub end_line: usize,
    pub kind: NodeKind,
    /// Comment and blank lines after the last entry that still belong here.
    pub trailing: Vec<usize>,
}

impl Node {
    pub fn entries(&self) -> &[Entry] {
        match &self.kind {
            NodeKind::Map(entries) => entries,
            _ => &[],
        }
    }

    pub fn items(&self) -> &[Item] {
        match &self.kind {
            NodeKind::Seq(items) => items,
            _ => &[],
        }
    }

    pub fn scalar(&self) -> Option<&Scalar> {
        match &self.kind {
            NodeKind::Scalar(scalar) => Some(scalar),
            _ => None,
        }
    }

    /// The entry called `key`, if this is a mapping that has one.
    pub fn entry(&self, key: &str) -> Option<&Entry> {
        self.entries().iter().find(|e| e.key == key)
    }

    /// The value under `key`, if this is a mapping that has one.
    pub fn child(&self, key: &str) -> Option<&Node> {
        self.entry(key).and_then(|e| e.value.as_ref())
    }

    /// The single-line text of the value under `key`.
    pub fn text(&self, key: &str) -> Option<&str> {
        self.child(key)
            .and_then(Node::scalar)
            .map(|s| s.text.as_str())
    }

    /// The node a path leads to.
    pub fn get(&self, path: &[Seg]) -> Option<&Node> {
        let Some((first, rest)) = path.split_first() else {
            return Some(self);
        };
        let next = match first {
            Seg::Key(key) => self.child(key)?,
            Seg::Index(index) => self.items().get(*index)?.value.as_ref()?,
        };
        next.get(rest)
    }
}

/// One `---`-separated document of a file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Doc {
    /// The `---` line that opens the document, when there is one.
    pub marker: Option<usize>,
    pub first_line: usize,
    /// The line after the document's last line.
    pub end_line: usize,
    /// Comment and blank lines before the first entry.
    pub leading: Vec<usize>,
    pub root: Option<Node>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct File {
    pub lines: Vec<String>,
    pub docs: Vec<Doc>,
    /// Whether the text opens like JSON, which this module does not read
    /// into a tree.
    pub json: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LineKind {
    Blank,
    Comment,
    Content,
}

/// The column of the first character that is not a space.
pub fn indent_of(line: &str) -> usize {
    line.len() - line.trim_start_matches(' ').len()
}

pub fn line_kind(line: &str) -> LineKind {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        LineKind::Blank
    } else if trimmed.starts_with('#') {
        LineKind::Comment
    } else {
        LineKind::Content
    }
}

fn is_marker(line: &str, marker: &str) -> bool {
    line == marker || (line.starts_with(marker) && line[marker.len()..].starts_with([' ', '\t']))
}

/// Whether text starting a node is a sequence item.
fn is_dash(text: &str) -> bool {
    text == "-" || text.starts_with("- ")
}

/// The length of `text` before any trailing comment, with trailing spaces
/// removed. A `#` opens a comment at the start or after whitespace, outside
/// quotes.
pub fn value_len(text: &str) -> usize {
    let bytes = text.as_bytes();
    let mut quote: Option<u8> = None;
    let mut i = 0;
    let mut end = bytes.len();
    while i < bytes.len() {
        let b = bytes[i];
        match quote {
            Some(b'"') if b == b'\\' => i += 1,
            Some(q) if b == q => quote = None,
            Some(_) => {}
            None if b == b'"' || b == b'\'' => {
                // A quote only opens a quoted scalar at the start of a token.
                if i == 0 || matches!(bytes[i - 1], b' ' | b'[' | b'{' | b',' | b':') {
                    quote = Some(b);
                }
            }
            None if b == b'#' && (i == 0 || bytes[i - 1] == b' ' || bytes[i - 1] == b'\t') => {
                end = i;
                break;
            }
            None => {}
        }
        i += 1;
    }
    text[..end.min(text.len())].trim_end().len()
}

/// Splits `text` into a key and what follows its colon. Returns the key as
/// written, its length in bytes and the colon's offset.
fn split_key(text: &str) -> Option<(String, usize, usize)> {
    let bytes = text.as_bytes();
    let first = *bytes.first()?;
    if matches!(
        first,
        b'{' | b'[' | b'|' | b'>' | b'#' | b'&' | b'*' | b'!' | b'%' | b'@'
    ) {
        return None;
    }
    if first == b'"' || first == b'\'' {
        let mut i = 1;
        while i < bytes.len() {
            if first == b'"' && bytes[i] == b'\\' {
                i += 2;
                continue;
            }
            if bytes[i] == first {
                break;
            }
            i += 1;
        }
        if i >= bytes.len() {
            return None;
        }
        let key_len = i + 1;
        let after = text[key_len..].trim_start_matches(' ');
        let colon = text.len() - after.len();
        if !after.starts_with(':') || !(after.len() == 1 || after[1..].starts_with([' ', '\t'])) {
            return None;
        }
        return Some((unquote(&text[..key_len]), key_len, colon));
    }
    for (i, b) in bytes.iter().enumerate() {
        match b {
            b':' if i + 1 == bytes.len() || bytes[i + 1] == b' ' || bytes[i + 1] == b'\t' => {
                let key = text[..i].trim_end();
                if key.is_empty() {
                    return None;
                }
                return Some((key.to_owned(), key.len(), i));
            }
            b'#' if i > 0 && bytes[i - 1] == b' ' => return None,
            _ => {}
        }
    }
    None
}

/// The text of a single-line scalar, without its quotes.
pub fn unquote(text: &str) -> String {
    let bytes = text.as_bytes();
    if bytes.len() >= 2 && bytes[0] == b'\'' && bytes[bytes.len() - 1] == b'\'' {
        return text[1..text.len() - 1].replace("''", "'");
    }
    if bytes.len() >= 2 && bytes[0] == b'"' && bytes[bytes.len() - 1] == b'"' {
        let mut out = String::new();
        let mut chars = text[1..text.len() - 1].chars();
        while let Some(c) = chars.next() {
            if c != '\\' {
                out.push(c);
                continue;
            }
            match chars.next() {
                Some('n') => out.push('\n'),
                Some('t') => out.push('\t'),
                Some(other) => out.push(other),
                None => {}
            }
        }
        return out;
    }
    text.to_owned()
}

/// Whether a quoted scalar opened at the start of `text` is closed within it.
fn quote_closed(text: &str) -> bool {
    let bytes = text.as_bytes();
    let quote = bytes[0];
    let mut i = 1;
    while i < bytes.len() {
        if quote == b'"' && bytes[i] == b'\\' {
            i += 2;
            continue;
        }
        if bytes[i] == quote {
            if quote == b'\'' && bytes.get(i + 1) == Some(&b'\'') {
                i += 2;
                continue;
            }
            return true;
        }
        i += 1;
    }
    false
}

/// The net count of open brackets in `text`, ignoring quoted text.
fn flow_depth(text: &str) -> i32 {
    let mut depth = 0;
    let mut quote: Option<char> = None;
    let mut escaped = false;
    for c in text.chars() {
        if escaped {
            escaped = false;
            continue;
        }
        match quote {
            Some('"') if c == '\\' => escaped = true,
            Some(q) if c == q => quote = None,
            Some(_) => {}
            None => match c {
                '"' | '\'' => quote = Some(c),
                '{' | '[' => depth += 1,
                '}' | ']' => depth -= 1,
                _ => {}
            },
        }
    }
    depth
}

struct Reader<'a> {
    lines: &'a [String],
    /// The line after the last one in the current document.
    end: usize,
    /// The next line nothing has claimed yet.
    next: usize,
}

impl Reader<'_> {
    fn kind(&self, line: usize) -> LineKind {
        line_kind(&self.lines[line])
    }

    fn indent(&self, line: usize) -> usize {
        indent_of(&self.lines[line])
    }

    /// The next line with content at or after `self.next`.
    fn next_content(&self) -> Option<usize> {
        (self.next..self.end).find(|&l| self.kind(l) == LineKind::Content)
    }

    /// Reads the node that starts at `col` of `line`. `parent` is the indent
    /// of what holds it: lines indented deeper than that continue the node.
    fn node(&mut self, line: usize, col: usize, parent: usize) -> Node {
        let text = &self.lines[line][col..];
        if is_dash(text) {
            self.seq(line, col)
        } else if split_key(text).is_some() {
            self.map(line, col)
        } else {
            self.scalar(line, col, parent)
        }
    }

    /// Decides what follows an entry or item within a block at `indent`.
    /// Returns the next sibling's line and the trivia before it, or `None`
    /// with the trivia that still belongs to this block.
    fn sibling(
        &mut self,
        indent: usize,
        want_dash: bool,
        lenient: bool,
    ) -> (Option<usize>, Vec<usize>) {
        let content = self.next_content();
        if let Some(line) = content {
            let at = self.indent(line);
            let dash = is_dash(&self.lines[line][at..]);
            let fits = if at == indent {
                dash == want_dash
            } else {
                // A line indented deeper than its siblings is half-typed text;
                // keeping it in the block beats losing track of the cursor.
                lenient && at > indent && dash == want_dash
            };
            if fits {
                let trivia = (self.next..line).collect();
                self.next = line;
                return (Some(line), trivia);
            }
        }
        // The block ends here. Comments indented at least as deep as the
        // block stay with it; everything from the first shallower line on is
        // left for whatever comes next.
        let stop = content.unwrap_or(self.end);
        let mut keep = self.next;
        for line in self.next..stop {
            if self.kind(line) == LineKind::Comment {
                if self.indent(line) >= indent {
                    keep = line + 1;
                } else {
                    break;
                }
            }
        }
        let trivia = (self.next..keep).collect();
        self.next = keep;
        (None, trivia)
    }

    fn map(&mut self, first: usize, indent: usize) -> Node {
        let mut entries: Vec<Entry> = Vec::new();
        let mut line = first;
        let mut col = indent;
        let mut leading = Vec::new();
        let trailing;
        loop {
            let text = &self.lines[line][col..];
            let (key, key_len, colon) = match split_key(text) {
                Some(found) => (found.0, found.1, Some(found.2)),
                // No colon yet: a key that is still being typed.
                None => {
                    let len = value_len(text);
                    (text[..len].to_owned(), len, None)
                }
            };
            self.next = line + 1;
            let mut value = None;
            if let Some(colon) = colon {
                let after = &text[colon + 1..];
                let gap = after.len() - after.trim_start_matches([' ', '\t']).len();
                let rest = &after[gap..];
                if value_len(rest) > 0 {
                    value = Some(self.scalar(line, col + colon + 1 + gap, col));
                } else if let Some(below) = self.next_content() {
                    let at = self.indent(below);
                    let dash = is_dash(&self.lines[below][at..]);
                    if at > col || (at == col && dash) {
                        let trivia: Vec<usize> = (self.next..below).collect();
                        let mut node = self.node(below, at, col);
                        node.attach_leading(trivia);
                        value = Some(node);
                    }
                }
            }
            entries.push(Entry {
                leading: std::mem::take(&mut leading),
                key,
                key_span: Span::on_line(line, col, col + key_len),
                colon: colon.map(|c| col + c),
                value,
                line,
                end_line: self.next,
            });
            match self.sibling(indent, false, true) {
                (Some(next), trivia) => {
                    leading = trivia;
                    line = next;
                    col = self.indent(next);
                }
                (None, trivia) => {
                    trailing = trivia;
                    break;
                }
            }
        }
        Node {
            indent,
            line: first,
            end_line: self.next,
            kind: NodeKind::Map(entries),
            trailing,
        }
    }

    fn seq(&mut self, first: usize, indent: usize) -> Node {
        let mut items: Vec<Item> = Vec::new();
        let mut line = first;
        let mut col = indent;
        let mut leading = Vec::new();
        let trailing;
        loop {
            let text = &self.lines[line][col..];
            let after = &text[1..];
            let gap = after.len() - after.trim_start_matches(' ').len();
            self.next = line + 1;
            let value = if value_len(&after[gap..]) > 0 {
                Some(self.node(line, col + 1 + gap, col))
            } else {
                match self.next_content() {
                    Some(below) if self.indent(below) > col => {
                        let trivia: Vec<usize> = (self.next..below).collect();
                        let mut node = self.node(below, self.indent(below), col);
                        node.attach_leading(trivia);
                        Some(node)
                    }
                    _ => None,
                }
            };
            items.push(Item {
                leading: std::mem::take(&mut leading),
                line,
                dash_col: col,
                value,
                end_line: self.next,
            });
            match self.sibling(indent, true, false) {
                (Some(next), trivia) => {
                    leading = trivia;
                    line = next;
                    col = self.indent(next);
                }
                (None, trivia) => {
                    trailing = trivia;
                    break;
                }
            }
        }
        Node {
            indent,
            line: first,
            end_line: self.next,
            kind: NodeKind::Seq(items),
            trailing,
        }
    }

    fn scalar(&mut self, line: usize, col: usize, parent: usize) -> Node {
        let text = &self.lines[line][col..];
        let first = text.as_bytes().first().copied().unwrap_or(b' ');
        self.next = self.next.max(line + 1);
        let len = value_len(text);
        let mut span = Span::on_line(line, col, col + len);
        let style;
        let mut value = text[..len].to_owned();
        match first {
            b'|' | b'>' => {
                style = Style::Block;
                let keep = text[..len].contains('+');
                let mut last = self.next;
                for l in self.next..self.end {
                    match self.kind(l) {
                        LineKind::Blank => {
                            if keep {
                                last = l + 1;
                            }
                        }
                        _ if self.indent(l) > parent => last = l + 1,
                        _ => break,
                    }
                }
                self.next = last;
            }
            b'"' | b'\'' => {
                style = Style::Quoted;
                if quote_closed(text) {
                    value = unquote(&text[..len]);
                } else {
                    // Runs on to the line that closes the quote.
                    let quote = first as char;
                    while self.next < self.end {
                        let l = self.next;
                        self.next += 1;
                        if self.lines[l].contains(quote) {
                            break;
                        }
                    }
                    span.end = Pos::new(line, col + text.len());
                }
            }
            b'{' | b'[' => {
                style = Style::Flow;
                let mut depth = flow_depth(&text[..len]);
                while depth > 0 && self.next < self.end {
                    depth += flow_depth(&self.lines[self.next]);
                    self.next += 1;
                }
            }
            _ => {
                style = Style::Plain;
                // A plain scalar runs on over deeper-indented lines.
                while self.next < self.end
                    && self.kind(self.next) == LineKind::Content
                    && self.indent(self.next) > parent
                {
                    self.next += 1;
                }
            }
        }
        Node {
            indent: col,
            line,
            end_line: self.next,
            kind: NodeKind::Scalar(Scalar {
                text: value,
                span,
                style,
            }),
            trailing: Vec::new(),
        }
    }
}

impl Node {
    /// Gives the lines before a nested node to its first entry or item.
    fn attach_leading(&mut self, mut trivia: Vec<usize>) {
        match &mut self.kind {
            NodeKind::Map(entries) => {
                if let Some(first) = entries.first_mut() {
                    trivia.append(&mut first.leading);
                    first.leading = trivia;
                }
            }
            NodeKind::Seq(items) => {
                if let Some(first) = items.first_mut() {
                    trivia.append(&mut first.leading);
                    first.leading = trivia;
                }
            }
            NodeKind::Scalar(_) => {}
        }
    }
}

/// Reads a file. It always succeeds.
pub fn read(text: &str) -> File {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let lines: Vec<String> = text
        .split('\n')
        .map(|l| l.strip_suffix('\r').unwrap_or(l).to_owned())
        .collect();
    let json = matches!(
        text.trim_start().as_bytes().first(),
        Some(b'{') | Some(b'[')
    );

    // Split into documents on `---`.
    let mut bounds: Vec<(Option<usize>, usize, usize)> = Vec::new();
    let mut start = 0;
    let mut marker = None;
    for (i, line) in lines.iter().enumerate() {
        if is_marker(line, "---") {
            let has_content = (start..i).any(|l| line_kind(&lines[l]) == LineKind::Content);
            if has_content || marker.is_some() {
                bounds.push((marker, start, i));
            } else if i > start {
                // Comments before the first marker travel with the document
                // that follows it.
                bounds.push((None, start, i));
            }
            marker = Some(i);
            start = i + 1;
        }
    }
    bounds.push((marker, start, lines.len()));
    // Leading comment-only stretches merge into the following document.
    let mut docs = Vec::new();
    let mut carried: Option<usize> = None;
    for (marker, first, end) in bounds {
        let has_content = (first..end).any(|l| line_kind(&lines[l]) == LineKind::Content);
        if marker.is_none() && !has_content && end < lines.len() {
            carried = Some(first);
            continue;
        }
        let mut reader = Reader {
            lines: &lines,
            end,
            next: first,
        };
        let mut leading = Vec::new();
        let mut root = None;
        if !json {
            if let Some(line) = reader.next_content() {
                // A `...` line ends a document and holds nothing.
                if !is_marker(&lines[line], "...") {
                    leading = (first..line).collect();
                    let indent = reader.indent(line);
                    let mut node = reader.node(line, indent, 0);
                    // Anything left over is stray text after the root; keep
                    // it inside the root so no line is lost.
                    node.trailing.extend(reader.next..end);
                    node.end_line = end;
                    root = Some(node);
                }
            }
        }
        if root.is_none() {
            leading = (first..end).collect();
        }
        docs.push(Doc {
            marker,
            first_line: carried.take().unwrap_or(first),
            end_line: end,
            leading,
            root,
        });
    }
    File { lines, docs, json }
}

impl File {
    /// The index of the document `line` is in.
    pub fn doc_at(&self, line: usize) -> Option<usize> {
        self.docs
            .iter()
            .position(|d| d.first_line <= line && line < d.end_line.max(d.first_line + 1))
            .or_else(|| self.docs.len().checked_sub(1))
    }

    /// Where a path leads in a document: the span of the key that names it,
    /// and the node under it. When the whole path cannot be followed, the
    /// deepest part that can is returned, so a diagnostic about a missing
    /// field lands on the mapping that should hold it.
    pub fn locate(&self, doc: usize, path: &[Seg]) -> Option<Located<'_>> {
        let root = self.docs.get(doc)?.root.as_ref()?;
        let mut found = Located {
            key: None,
            node: Some(root),
            complete: path.is_empty(),
            line: root.line,
        };
        let mut node = root;
        for (i, seg) in path.iter().enumerate() {
            let (key, value, line) = match seg {
                Seg::Key(key) => match node.entry(key) {
                    Some(entry) => (Some(entry.key_span), entry.value.as_ref(), entry.line),
                    None => break,
                },
                Seg::Index(index) => match node.items().get(*index) {
                    Some(item) => (
                        Some(Span::on_line(item.line, item.dash_col, item.dash_col + 1)),
                        item.value.as_ref(),
                        item.line,
                    ),
                    None => break,
                },
            };
            found = Located {
                key,
                node: value,
                complete: i + 1 == path.len(),
                line,
            };
            match value {
                Some(next) => node = next,
                None => break,
            }
        }
        Some(found)
    }

    /// The end of the content on `line`, ignoring trailing spaces.
    pub fn line_end(&self, line: usize) -> usize {
        self.lines.get(line).map_or(0, |l| l.trim_end().len())
    }
}

/// What [`File::locate`] found.
#[derive(Clone, Copy, Debug)]
pub struct Located<'a> {
    /// The key, or the dash of a list item, that the path ends at.
    pub key: Option<Span>,
    pub node: Option<&'a Node>,
    /// Whether the whole path was followed.
    pub complete: bool,
    pub line: usize,
}

impl Located<'_> {
    /// The span to underline: the value when it is a one-line scalar, the
    /// key otherwise.
    pub fn span(&self, file: &File) -> Span {
        if self.complete {
            if let Some(scalar) = self.node.and_then(Node::scalar) {
                if scalar.span.end.col > scalar.span.start.col {
                    return scalar.span;
                }
            }
        }
        self.key.unwrap_or_else(|| {
            let indent = file.lines.get(self.line).map_or(0, |l| indent_of(l));
            Span::on_line(self.line, indent, file.line_end(self.line).max(indent))
        })
    }
}

/// Where the cursor is, in terms of the document's structure.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Cursor {
    pub doc: usize,
    /// For [`Place::Key`], the mapping a key is being typed in. For
    /// [`Place::Value`], the value being typed.
    pub path: Vec<Seg>,
    pub place: Place,
    /// The text already typed, which a completion replaces.
    pub word: String,
    pub span: Span,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Place {
    Key,
    Value,
}

impl File {
    /// Works out what the cursor at `pos` is on, or about to type.
    pub fn cursor(&self, pos: Pos) -> Option<Cursor> {
        if self.json {
            return None;
        }
        let doc = self.doc_at(pos.line)?;
        let empty = Cursor {
            doc,
            path: Vec::new(),
            place: Place::Key,
            word: String::new(),
            span: Span {
                start: pos,
                end: pos,
            },
        };
        let Some(root) = self.docs[doc].root.as_ref() else {
            return Some(empty);
        };
        let line = self.lines.get(pos.line)?;
        if line_kind(line) == LineKind::Comment {
            return None;
        }
        if line_kind(line) == LineKind::Content && pos.line >= root.line {
            let mut path = Vec::new();
            if let Some(found) = self.on_line(root, pos, &mut path, doc) {
                return Some(found);
            }
        }
        if pos.line < root.line {
            return Some(empty);
        }
        // A blank line: the indentation says which mapping a new key joins.
        let mut path = Vec::new();
        let place = self.by_indent(root, pos, &mut path)?;
        Some(Cursor {
            path,
            place,
            ..empty
        })
    }

    fn on_line(&self, node: &Node, pos: Pos, path: &mut Vec<Seg>, doc: usize) -> Option<Cursor> {
        let at = |place, path: &[Seg], word: &str, span| Cursor {
            doc,
            path: path.to_vec(),
            place,
            word: word.to_owned(),
            span,
        };
        let here = Span {
            start: pos,
            end: pos,
        };
        match &node.kind {
            NodeKind::Map(entries) => {
                let entry = entries
                    .iter()
                    .find(|e| e.line <= pos.line && pos.line < e.end_line)?;
                if entry.line == pos.line {
                    let in_key = match entry.colon {
                        Some(colon) => pos.col <= colon,
                        None => true,
                    };
                    if in_key {
                        if pos.col < entry.key_span.start.col {
                            return None;
                        }
                        return Some(at(Place::Key, path, &entry.key, entry.key_span));
                    }
                    path.push(Seg::Key(entry.key.clone()));
                    return Some(match entry.value.as_ref() {
                        Some(value) if value.line == pos.line => match &value.kind {
                            NodeKind::Scalar(s) => at(Place::Value, path, &s.text, s.span),
                            _ => return self.on_line(value, pos, path, doc),
                        },
                        _ => at(Place::Value, path, "", here),
                    });
                }
                path.push(Seg::Key(entry.key.clone()));
                self.on_line(entry.value.as_ref()?, pos, path, doc)
            }
            NodeKind::Seq(items) => {
                let index = items
                    .iter()
                    .position(|i| i.line <= pos.line && pos.line < i.end_line)?;
                let item = &items[index];
                path.push(Seg::Index(index));
                match item.value.as_ref() {
                    Some(value) => self.on_line(value, pos, path, doc),
                    // A bare dash: the first key of a new item goes here.
                    None => Some(at(Place::Key, path, "", here)),
                }
            }
            NodeKind::Scalar(s) => {
                // An unfinished word alone in a list item or under a key may
                // be the start of a key; the caller decides from the schema.
                let (word, span) = if node.line == pos.line {
                    (s.text.as_str(), s.span)
                } else {
                    ("", here)
                };
                Some(at(Place::Value, path, word, span))
            }
        }
    }

    fn by_indent(&self, node: &Node, pos: Pos, path: &mut Vec<Seg>) -> Option<Place> {
        match &node.kind {
            NodeKind::Map(entries) => {
                let last = entries.iter().rfind(|e| e.line < pos.line)?;
                if pos.col <= node.indent {
                    return Some(Place::Key);
                }
                // Deeper than this mapping: the new line belongs under its
                // last entry.
                path.push(Seg::Key(last.key.clone()));
                match last.value.as_ref() {
                    None => Some(Place::Key),
                    Some(value) => self.by_indent(value, pos, path),
                }
            }
            NodeKind::Seq(items) => {
                let (index, last) = items.iter().enumerate().rfind(|(_, i)| i.line < pos.line)?;
                if pos.col <= node.indent {
                    // Level with the dashes: a new item, which is a value.
                    path.push(Seg::Index(items.len()));
                    return Some(Place::Value);
                }
                path.push(Seg::Index(index));
                match last.value.as_ref() {
                    None => Some(Place::Key),
                    Some(value) => self.by_indent(value, pos, path),
                }
            }
            NodeKind::Scalar(_) => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DOC: &str = "\
# A web server.
apiVersion: meridian/v1
kind: ResourceSet
metadata:
  name: web
spec:
  target: ansible
  resources:
    - id: nginx_pkg
      type: package
      params:
        name: nginx

    # The config.
    - id: nginx_conf
      type: file
      params:
        path: /etc/nginx.conf
        content: |
          server {
            listen 80;
          }
        mode: \"0644\"  # quoted
      dependsOn:
        - resource: nginx_pkg
";

    fn key(k: &str) -> Seg {
        Seg::Key(k.to_owned())
    }

    #[test]
    fn a_document_reads_into_a_tree_that_keeps_every_line() {
        let file = read(DOC);
        assert_eq!(file.docs.len(), 1);
        let root = file.docs[0].root.as_ref().unwrap();
        assert_eq!(file.docs[0].leading, [0]);
        assert_eq!(root.text("kind"), Some("ResourceSet"));
        let resources = root
            .get(&[key("spec"), key("resources")])
            .expect("resources");
        assert_eq!(resources.items().len(), 2);
        let conf = resources.items()[1].value.as_ref().unwrap();
        assert_eq!(resources.items()[1].leading, [12, 13]);
        assert_eq!(conf.text("id"), Some("nginx_conf"));
        let params = conf.child("params").unwrap();
        assert_eq!(params.text("mode"), Some("0644"));
        let content = params.child("content").unwrap();
        assert_eq!(content.scalar().unwrap().style, Style::Block);
        assert_eq!((content.line, content.end_line), (18, 22));
        let dep = conf.get(&[key("dependsOn"), Seg::Index(0)]).unwrap();
        assert_eq!(dep.text("resource"), Some("nginx_pkg"));
    }

    #[test]
    fn a_path_is_located_and_a_missing_one_lands_on_its_parent() {
        let file = read(DOC);
        let path = parse_path("spec.resources[1].dependsOn[0].resource");
        assert_eq!(show_path(&path), "spec.resources[1].dependsOn[0].resource");
        let found = file.locate(0, &path).unwrap();
        assert!(found.complete);
        assert_eq!(found.span(&file), Span::on_line(24, 20, 29));

        let missing = file
            .locate(0, &parse_path("spec.resources[0].state"))
            .unwrap();
        assert!(!missing.complete);
        // The dash of the first resource.
        assert_eq!(missing.span(&file), Span::on_line(8, 4, 5));
    }

    #[test]
    fn the_cursor_knows_a_key_from_a_value() {
        let file = read(DOC);
        // On `type` of the first resource.
        let c = file.cursor(Pos::new(9, 8)).unwrap();
        assert_eq!(c.place, Place::Key);
        assert_eq!(c.word, "type");
        assert_eq!(show_path(&c.path), "spec.resources[0]");
        // On its value.
        let c = file.cursor(Pos::new(9, 15)).unwrap();
        assert_eq!(c.place, Place::Value);
        assert_eq!(c.word, "package");
        assert_eq!(show_path(&c.path), "spec.resources[0].type");
        // In the dependsOn entry.
        let c = file.cursor(Pos::new(24, 22)).unwrap();
        assert_eq!(
            show_path(&c.path),
            "spec.resources[1].dependsOn[0].resource"
        );
    }

    #[test]
    fn a_blank_line_takes_its_place_from_its_indentation() {
        let text = "\
kind: ResourceSet
spec:
  target: ansible
  resources:
    - id: a
      type: package
      params:




";
        let file = read(text);
        let at = |line, col| {
            let c = file.cursor(Pos::new(line, col)).unwrap();
            (show_path(&c.path), c.place)
        };
        assert_eq!(
            at(7, 8),
            ("spec.resources[0].params".to_owned(), Place::Key)
        );
        assert_eq!(at(8, 6), ("spec.resources[0]".to_owned(), Place::Key));
        assert_eq!(at(9, 4), ("spec.resources[1]".to_owned(), Place::Value));
        assert_eq!(at(10, 2), ("spec".to_owned(), Place::Key));
        assert_eq!(at(10, 0), (String::new(), Place::Key));
    }

    #[test]
    fn half_typed_text_still_has_a_place() {
        let file = read("kind: ResourceSet\nspec:\n  resources:\n    - \n");
        let c = file.cursor(Pos::new(3, 6)).unwrap();
        assert_eq!(
            (show_path(&c.path), c.place),
            ("spec.resources[0]".to_owned(), Place::Key)
        );

        // A lone word under a key reads as that key's value. Whether it is
        // really the start of a nested key is the schema's call, not ours.
        let file = read("kind: ResourceSet\nspec:\n  tar\n");
        let c = file.cursor(Pos::new(2, 5)).unwrap();
        assert_eq!(
            (show_path(&c.path), c.place, c.word.as_str()),
            ("spec".to_owned(), Place::Value, "tar")
        );

        let file = read("spec:\n  target: \n");
        let c = file.cursor(Pos::new(1, 10)).unwrap();
        assert_eq!(
            (show_path(&c.path), c.place),
            ("spec.target".to_owned(), Place::Value)
        );

        let file = read("");
        assert_eq!(file.cursor(Pos::new(0, 0)).unwrap().place, Place::Key);
    }

    #[test]
    fn documents_split_on_markers_and_comments_stay_with_what_follows() {
        let file = read("# top\n---\na: 1\n---\n# second\nb: 2\n");
        assert_eq!(file.docs.len(), 2);
        assert_eq!(file.docs[0].first_line, 0);
        assert_eq!(file.docs[0].root.as_ref().unwrap().text("a"), Some("1"));
        assert_eq!(file.docs[1].marker, Some(3));
        assert_eq!(file.docs[1].leading, [4]);
        assert_eq!(file.doc_at(5), Some(1));
    }

    #[test]
    fn comments_and_quotes_do_not_confuse_values() {
        assert_eq!(value_len("nginx  # the web server"), 5);
        assert_eq!(value_len("\"a # b\" # c"), 7);
        assert_eq!(value_len("it's # fine"), 4);
        let file =
            read("url: http://x/y#frag\n'quoted key': v\nflow: {a: [1, 2],\n  b: 3}\nnext: 1\n");
        let root = file.docs[0].root.as_ref().unwrap();
        assert_eq!(root.text("url"), Some("http://x/y#frag"));
        assert_eq!(root.text("quoted key"), Some("v"));
        assert_eq!(root.child("flow").unwrap().end_line, 4);
        assert_eq!(root.text("next"), Some("1"));
    }
}
