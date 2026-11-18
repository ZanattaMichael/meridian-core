use std::fmt::Write as _;

use crate::scalar::{resolve_plain, Scalar};

/// A YAML value to write. A mapping keeps the order it is given, so the
/// caller decides whether keys are sorted or follow a convention.
#[derive(Clone, Debug, PartialEq)]
pub enum Yaml {
    Null,
    Bool(bool),
    Int(i64),
    Uint(u64),
    Float(f64),
    Str(String),
    /// Text written as it is, unquoted and unescaped, as a plain scalar on
    /// one line. For a template expression that a tool renders before it
    /// reads the file as YAML, such as a Jinja read of a Salt pillar key: the
    /// expression is what is written, and the tool's rendering of it is what
    /// the YAML reader sees. The text has no line break.
    Raw(String),
    Seq(Vec<Yaml>),
    Map(Vec<(String, Yaml)>),
}

impl Yaml {
    /// A string value.
    pub fn str(s: impl Into<String>) -> Yaml {
        Yaml::Str(s.into())
    }
}

/// Writes one document in block style with two-space indentation. The
/// result ends with a newline and has no `---` marker.
pub fn write(doc: &Yaml) -> String {
    let mut w = Writer {
        out: String::new(),
        step: 2,
    };
    w.node(doc, 0, Place::Root);
    if !w.out.ends_with('\n') {
        w.out.push('\n');
    }
    w.out
}

/// Where a node sits, which decides what precedes it on its line.
#[derive(Clone, Copy, PartialEq)]
enum Place {
    Root,
    /// After `key:`.
    MapValue,
    /// After `- `.
    SeqItem,
}

struct Writer {
    out: String,
    step: usize,
}

impl Writer {
    /// `indent` is the column a nested block's own lines start at.
    fn node(&mut self, node: &Yaml, indent: usize, place: Place) {
        match node {
            Yaml::Map(pairs) if pairs.is_empty() => self.inline(place, "{}"),
            Yaml::Seq(items) if items.is_empty() => self.inline(place, "[]"),
            Yaml::Map(pairs) => {
                // A mapping after `key:` starts on the next line; one after
                // `- ` or at the root continues on the current line.
                let mut first = place != Place::MapValue;
                if place == Place::MapValue {
                    self.out.push('\n');
                }
                for (key, value) in pairs {
                    if !first {
                        self.pad(indent);
                    }
                    first = false;
                    self.key(key, indent);
                    self.out.push(':');
                    self.node(value, indent + self.step, Place::MapValue);
                }
            }
            Yaml::Seq(items) => {
                let mut first = place != Place::MapValue;
                if place == Place::MapValue {
                    self.out.push('\n');
                }
                for item in items {
                    if !first {
                        self.pad(indent);
                    }
                    first = false;
                    self.out.push_str("- ");
                    // What follows the dash is laid out as if the dash and
                    // its space were indentation.
                    self.node(item, indent + self.step, Place::SeqItem);
                }
            }
            scalar => {
                let text = match scalar {
                    Yaml::Null => Text::Plain("null".to_owned()),
                    Yaml::Bool(b) => Text::Plain(b.to_string()),
                    Yaml::Int(i) => Text::Plain(i.to_string()),
                    Yaml::Uint(u) => Text::Plain(u.to_string()),
                    Yaml::Float(f) => Text::Plain(float(*f)),
                    // At the root there is no enclosing block, so a literal's
                    // lines are indented one step from the margin.
                    Yaml::Str(s) => string(s, indent.max(self.step)),
                    Yaml::Raw(s) => Text::Plain(s.clone()),
                    Yaml::Map(_) | Yaml::Seq(_) => unreachable!("handled above"),
                };
                match text {
                    Text::Plain(s) if s.is_empty() => self.out.push('\n'),
                    Text::Plain(s) => self.inline(place, &s),
                    Text::Block(s) => {
                        if place == Place::MapValue {
                            self.out.push(' ');
                        }
                        self.out.push_str(&s);
                    }
                }
            }
        }
    }

    fn inline(&mut self, place: Place, text: &str) {
        if place == Place::MapValue {
            self.out.push(' ');
        }
        self.out.push_str(text);
        self.out.push('\n');
    }

    fn pad(&mut self, indent: usize) {
        for _ in 0..indent {
            self.out.push(' ');
        }
    }

    /// Writes a mapping key. A key that fits on a line is written as is; one
    /// that spans lines or is very long is written in the explicit form,
    /// `? key` on its own lines and the `:` on the next.
    fn key(&mut self, key: &str, indent: usize) {
        let simple = key.len() <= 128 && !key.contains(is_break);
        if simple {
            match string(key, indent) {
                Text::Plain(s) | Text::Block(s) => self.out.push_str(&s),
            }
            return;
        }
        self.out.push_str("? ");
        match string(key, indent + self.step) {
            Text::Plain(s) => {
                self.out.push_str(&s);
                self.out.push('\n');
            }
            Text::Block(s) => self.out.push_str(&s),
        }
        self.pad(indent);
    }
}

/// A scalar's written form.
enum Text {
    /// On one line.
    Plain(String),
    /// A literal block: its header, then its indented lines, each ended.
    Block(String),
}

/// Writes a float the way Go's `strconv.FormatFloat(f, 'g', -1, 64)` does,
/// with YAML's names for the values that are not numbers.
fn float(f: f64) -> String {
    if f.is_nan() {
        return ".nan".to_owned();
    }
    if f.is_infinite() {
        return if f > 0.0 { ".inf" } else { "-.inf" }.to_owned();
    }
    if f == 0.0 {
        return if f.is_sign_negative() { "-0" } else { "0" }.to_owned();
    }
    // Rust's `{:e}` gives the shortest digits that round-trip, as Go does.
    let sci = format!("{f:e}");
    let (mantissa, exponent) = sci.split_once('e').expect("{:e} always has an exponent");
    let exponent: i32 = exponent.parse().expect("the exponent is a number");
    if !(-4..6).contains(&exponent) {
        let sign = if exponent < 0 { '-' } else { '+' };
        return format!("{mantissa}e{sign}{:02}", exponent.abs());
    }
    let negative = mantissa.starts_with('-');
    let digits: String = mantissa.chars().filter(char::is_ascii_digit).collect();
    let mut out = String::new();
    if negative {
        out.push('-');
    }
    if exponent < 0 {
        out.push_str("0.");
        for _ in 0..(-exponent - 1) {
            out.push('0');
        }
        out.push_str(&digits);
    } else {
        let whole = exponent as usize + 1;
        if digits.len() <= whole {
            out.push_str(&digits);
            for _ in digits.len()..whole {
                out.push('0');
            }
        } else {
            out.push_str(&digits[..whole]);
            out.push('.');
            out.push_str(&digits[whole..]);
        }
    }
    out
}

/// What the characters of a string allow, after the reference emitter's
/// scalar analysis.
struct Analysis {
    block_plain_allowed: bool,
    single_quoted_allowed: bool,
    block_allowed: bool,
}

fn is_break(c: char) -> bool {
    matches!(c, '\n' | '\r' | '\u{85}' | '\u{2028}' | '\u{2029}')
}

/// Whether a character may appear unescaped.
fn is_printable(c: char) -> bool {
    matches!(c,
        '\n'
        | '\u{20}'..='\u{7e}'
        | '\u{a0}'..='\u{d7ff}'
        | '\u{e000}'..='\u{fefe}'
        | '\u{ff00}'..='\u{fffd}')
}

fn analyze(value: &str) -> Analysis {
    if value.is_empty() {
        return Analysis {
            block_plain_allowed: true,
            single_quoted_allowed: true,
            block_allowed: false,
        };
    }
    let chars: Vec<char> = value.chars().collect();
    let mut block_indicators = value.starts_with("---") || value.starts_with("...");
    let mut line_breaks = false;
    let mut special_characters = false;
    let mut tab_characters = false;
    let mut leading_space = false;
    let mut leading_break = false;
    let mut trailing_space = false;
    let mut trailing_break = false;
    let mut break_space = false;
    let mut space_break = false;
    let mut preceded_by_whitespace = true;
    let mut previous_space = false;
    let mut previous_break = false;

    for (i, &c) in chars.iter().enumerate() {
        let last = i + 1 == chars.len();
        let followed_by_whitespace = last || matches!(chars[i + 1], ' ' | '\t');
        if i == 0 {
            match c {
                '#' | ',' | '[' | ']' | '{' | '}' | '&' | '*' | '!' | '|' | '>' | '\'' | '"'
                | '%' | '@' | '`' => block_indicators = true,
                '?' | ':' | '-' if followed_by_whitespace => block_indicators = true,
                _ => {}
            }
        } else {
            match c {
                ':' if followed_by_whitespace => block_indicators = true,
                '#' if preceded_by_whitespace => block_indicators = true,
                _ => {}
            }
        }

        if c == '\t' {
            tab_characters = true;
        } else if !is_printable(c) {
            special_characters = true;
        }
        if c == ' ' {
            leading_space |= i == 0;
            trailing_space |= last;
            break_space |= previous_break;
            previous_space = true;
            previous_break = false;
        } else if is_break(c) {
            line_breaks = true;
            leading_break |= i == 0;
            trailing_break |= last;
            space_break |= previous_space;
            previous_space = false;
            previous_break = true;
        } else {
            previous_space = false;
            previous_break = false;
        }
        // Blank, break, or the end: what the reference calls "blankz".
        preceded_by_whitespace = matches!(c, ' ' | '\t') || is_break(c);
    }

    let mut a = Analysis {
        block_plain_allowed: true,
        single_quoted_allowed: true,
        block_allowed: true,
    };
    if leading_space || leading_break || trailing_space || trailing_break {
        a.block_plain_allowed = false;
    }
    if trailing_space {
        a.block_allowed = false;
    }
    if break_space {
        a.block_plain_allowed = false;
        a.single_quoted_allowed = false;
    }
    if space_break || tab_characters || special_characters {
        a.block_plain_allowed = false;
        a.single_quoted_allowed = false;
    }
    if space_break || special_characters {
        a.block_allowed = false;
    }
    if line_breaks || block_indicators {
        a.block_plain_allowed = false;
    }
    a
}

#[derive(Clone, Copy, PartialEq)]
enum Style {
    Plain,
    Single,
    Double,
    Literal,
}

/// Chooses how a string is written and writes it. `indent` is the column the
/// lines of a literal block start at.
fn string(value: &str, indent: usize) -> Text {
    // A line break other than a plain newline cannot be written literally
    // and read back unchanged, so such a string is always escaped.
    if value.contains(|c| is_break(c) && c != '\n') {
        return Text::Plain(double_quoted(value));
    }
    // Written plainly, would this be read back as something other than the
    // string it is?
    let can_use_plain = matches!(resolve_plain(value), Scalar::Str)
        && value != "<<"
        && !is_timestamp(value)
        && !is_base60_float(value)
        && !is_old_bool(value);
    let mut style = if value.contains('\n') {
        Style::Literal
    } else if can_use_plain {
        Style::Plain
    } else {
        Style::Double
    };

    let a = analyze(value);
    if style == Style::Plain && !a.block_plain_allowed {
        style = Style::Single;
    }
    if style == Style::Single && !a.single_quoted_allowed {
        style = Style::Double;
    }
    if style == Style::Literal && !a.block_allowed {
        style = Style::Double;
    }

    match style {
        Style::Plain => Text::Plain(value.to_owned()),
        Style::Single => Text::Plain(format!("'{}'", value.replace('\'', "''"))),
        Style::Double => Text::Plain(double_quoted(value)),
        Style::Literal => Text::Block(literal(value, indent)),
    }
}

fn double_quoted(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    out.push('"');
    for c in value.chars() {
        let needs_escape =
            !is_printable(c) || c == '\u{feff}' || is_break(c) || c == '"' || c == '\\';
        if !needs_escape {
            out.push(c);
            continue;
        }
        out.push('\\');
        match c {
            '\0' => out.push('0'),
            '\u{7}' => out.push('a'),
            '\u{8}' => out.push('b'),
            '\t' => out.push('t'),
            '\n' => out.push('n'),
            '\u{b}' => out.push('v'),
            '\u{c}' => out.push('f'),
            '\r' => out.push('r'),
            '\u{1b}' => out.push('e'),
            '"' => out.push('"'),
            '\\' => out.push('\\'),
            '\u{85}' => out.push('N'),
            '\u{a0}' => out.push('_'),
            '\u{2028}' => out.push('L'),
            '\u{2029}' => out.push('P'),
            c => {
                let v = c as u32;
                let _ = if v <= 0xff {
                    write!(out, "x{v:02X}")
                } else if v <= 0xffff {
                    write!(out, "u{v:04X}")
                } else {
                    write!(out, "U{v:08X}")
                };
            }
        }
    }
    out.push('"');
    out
}

fn literal(value: &str, indent: usize) -> String {
    let mut out = String::from("|");
    // A block that starts with a space or a blank line needs to be told how
    // far it is indented, since its first line cannot show it.
    if value.starts_with(' ') || value.starts_with(is_break) {
        out.push('2');
    }
    let mut ends = value.chars().rev();
    match (ends.next(), ends.next()) {
        (Some(last), _) if !is_break(last) => out.push('-'),
        (Some(_), None) => out.push('+'),
        (Some(_), Some(before)) if is_break(before) => out.push('+'),
        _ => {}
    }
    out.push('\n');
    let mut at_line_start = true;
    for c in value.chars() {
        if is_break(c) {
            // A line break is written as a line break whatever kind it was.
            out.push('\n');
            at_line_start = true;
        } else {
            if at_line_start {
                for _ in 0..indent {
                    out.push(' ');
                }
                at_line_start = false;
            }
            out.push(c);
        }
    }
    if !out.ends_with('\n') {
        out.push('\n');
    }
    out
}

/// Booleans as YAML 1.1 spelled them. They are strings to Meridian, and are
/// quoted so an older parser reads them as strings too.
fn is_old_bool(s: &str) -> bool {
    matches!(
        s,
        "y" | "Y"
            | "yes"
            | "Yes"
            | "YES"
            | "on"
            | "On"
            | "ON"
            | "n"
            | "N"
            | "no"
            | "No"
            | "NO"
            | "off"
            | "Off"
            | "OFF"
    )
}

/// `^[-+]?[0-9][0-9_]*(?::[0-5]?[0-9])+(?:\.[0-9_]*)?$`: a sexagesimal number
/// in YAML 1.1, such as `1:30`, quoted for the same reason.
fn is_base60_float(s: &str) -> bool {
    let b = s.as_bytes();
    let mut i = 0;
    if matches!(b.first(), Some(b'-' | b'+')) {
        i += 1;
    }
    if !b.get(i).is_some_and(u8::is_ascii_digit) {
        return false;
    }
    while b.get(i).is_some_and(|c| c.is_ascii_digit() || *c == b'_') {
        i += 1;
    }
    let mut groups = 0;
    while b.get(i) == Some(&b':') {
        i += 1;
        let start = i;
        while i - start < 2 && b.get(i).is_some_and(u8::is_ascii_digit) {
            i += 1;
        }
        match &b[start..i] {
            [_] => {}
            [tens, _] if *tens <= b'5' => {}
            _ => return false,
        }
        groups += 1;
    }
    if groups == 0 {
        return false;
    }
    if b.get(i) == Some(&b'.') {
        i += 1;
        while b.get(i).is_some_and(|c| c.is_ascii_digit() || *c == b'_') {
            i += 1;
        }
    }
    i == b.len()
}

/// Whether a plain scalar would be read back as a timestamp: a date, or a
/// date and time, in the forms the reference library accepts.
fn is_timestamp(s: &str) -> bool {
    let b = s.as_bytes();
    // Every accepted form starts with a four-digit year and a dash.
    if b.len() < 5 || !b[..4].iter().all(u8::is_ascii_digit) || b[4] != b'-' {
        return false;
    }
    let mut p = Cursor { b, i: 5 };
    let year: u32 = s[..4].parse().unwrap_or(0);
    let Some(month) = p.number(2) else {
        return false;
    };
    if !p.eat(b'-') {
        return false;
    }
    let Some(day) = p.number(2) else { return false };
    if !(1..=12).contains(&month) || day < 1 || day > days_in(year, month) {
        return false;
    }
    if p.done() {
        return true;
    }
    // A time follows, after `T`, `t` or a space. Only the first two may
    // carry a zone, and they must.
    let Some(separator) = p.next() else {
        return false;
    };
    if !matches!(separator, b'T' | b't' | b' ') {
        return false;
    }
    let (Some(hour), true, Some(minute), true, Some(second)) = (
        p.number(2),
        p.eat(b':'),
        p.number(2),
        p.eat(b':'),
        p.number(2),
    ) else {
        return false;
    };
    if hour > 23 || minute > 59 || second > 59 {
        return false;
    }
    if p.peek().is_some_and(|c| c == b'.' || c == b',') {
        let after = p.i + 1;
        if b.get(after).is_some_and(u8::is_ascii_digit) {
            p.i = after;
            while p.peek().is_some_and(|c| c.is_ascii_digit()) {
                p.i += 1;
            }
        }
    }
    if separator == b' ' {
        return p.done();
    }
    if p.eat(b'Z') {
        return p.done();
    }
    let Some(sign) = p.next() else { return false };
    if !matches!(sign, b'+' | b'-') {
        return false;
    }
    let (Some(zone_hour), true, Some(zone_minute)) = (p.exactly(2), p.eat(b':'), p.exactly(2))
    else {
        return false;
    };
    zone_hour <= 24 && zone_minute <= 60 && p.done()
}

fn days_in(year: u32, month: u32) -> u32 {
    match month {
        4 | 6 | 9 | 11 => 30,
        2 if year.is_multiple_of(4) && (!year.is_multiple_of(100) || year.is_multiple_of(400)) => {
            29
        }
        2 => 28,
        _ => 31,
    }
}

struct Cursor<'a> {
    b: &'a [u8],
    i: usize,
}

impl Cursor<'_> {
    fn peek(&self) -> Option<u8> {
        self.b.get(self.i).copied()
    }

    fn next(&mut self) -> Option<u8> {
        let c = self.peek()?;
        self.i += 1;
        Some(c)
    }

    fn eat(&mut self, c: u8) -> bool {
        if self.peek() == Some(c) {
            self.i += 1;
            true
        } else {
            false
        }
    }

    fn done(&self) -> bool {
        self.i == self.b.len()
    }

    /// One to `max` digits.
    fn number(&mut self, max: usize) -> Option<u32> {
        let start = self.i;
        while self.i - start < max && self.peek().is_some_and(|c| c.is_ascii_digit()) {
            self.i += 1;
        }
        std::str::from_utf8(&self.b[start..self.i])
            .ok()?
            .parse()
            .ok()
    }

    /// Exactly `n` digits.
    fn exactly(&mut self, n: usize) -> Option<u32> {
        let start = self.i;
        let value = self.number(n)?;
        (self.i - start == n).then_some(value)
    }
}
