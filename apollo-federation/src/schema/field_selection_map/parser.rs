//! Recursive-descent parser for the `FieldSelectionMap` grammar (see the module docs).

use apollo_compiler::Name;
use apollo_compiler::Node;
use apollo_compiler::ast;

use super::FieldSelectionMapParseError;
use super::Path;
use super::PathSegment;
use super::SelectedListValue;
use super::SelectedObjectField;
use super::SelectedObjectValue;
use super::SelectedValue;
use super::SelectedValueEntry;

/// How deeply selected values, lists and argument values may nest. The parser, and every later
/// pass over the parsed map, recurses once per level; the map comes from a plain string argument,
/// so apollo-compiler's own recursion limit does not cover it.
const MAX_DEPTH: usize = 64;

/// Parse a `FieldSelectionMap` string.
pub(crate) fn parse(input: &str) -> Result<SelectedValue, FieldSelectionMapParseError> {
    let mut parser = Parser {
        input,
        pos: 0,
        depth: 0,
    };
    parser.skip_ignored();
    let value = parser.selected_value()?;
    parser.skip_ignored();
    if parser.pos < input.len() {
        return Err(parser.error(format!(
            "unexpected `{}`",
            parser.rest().chars().next().unwrap_or_default()
        )));
    }
    Ok(value)
}

struct Parser<'a> {
    input: &'a str,
    pos: usize,
    /// Current nesting of selected values, lists and argument values.
    depth: usize,
}

impl Parser<'_> {
    fn rest(&self) -> &str {
        &self.input[self.pos..]
    }

    fn peek(&self) -> Option<char> {
        self.rest().chars().next()
    }

    fn error(&self, message: impl Into<String>) -> FieldSelectionMapParseError {
        FieldSelectionMapParseError {
            message: message.into(),
            offset: self.pos,
        }
    }

    /// Skip whitespace, commas and comments, which are insignificant as in GraphQL.
    fn skip_ignored(&mut self) {
        loop {
            match self.peek() {
                // GraphQL's insignificant tokens: whitespace (tab, space), line terminators,
                // commas and the byte order mark.
                Some(c @ ('\t' | ' ' | '\n' | '\r' | ',' | '\u{feff}')) => {
                    self.pos += c.len_utf8();
                }
                Some('#') => {
                    while let Some(c) = self.peek() {
                        self.pos += c.len_utf8();
                        if c == '\n' || c == '\r' {
                            break;
                        }
                    }
                }
                _ => return,
            }
        }
    }

    /// Consume `c` (after skipping ignored tokens) if it is next.
    fn eat(&mut self, c: char) -> bool {
        self.skip_ignored();
        if self.peek() == Some(c) {
            self.pos += c.len_utf8();
            true
        } else {
            false
        }
    }

    fn expect(&mut self, c: char) -> Result<(), FieldSelectionMapParseError> {
        if self.eat(c) {
            Ok(())
        } else {
            Err(self.error(match self.peek() {
                Some(found) => format!("expected `{c}`, found `{found}`"),
                None => format!("expected `{c}`, found end of input"),
            }))
        }
    }

    /// Run `parse` one nesting level deeper, failing past [`MAX_DEPTH`].
    fn nested<T>(
        &mut self,
        parse: impl FnOnce(&mut Self) -> Result<T, FieldSelectionMapParseError>,
    ) -> Result<T, FieldSelectionMapParseError> {
        if self.depth >= MAX_DEPTH {
            return Err(self.error(format!("nested more than {MAX_DEPTH} levels deep")));
        }
        self.depth += 1;
        let result = parse(self);
        self.depth -= 1;
        result
    }

    fn at(&mut self, c: char) -> bool {
        self.skip_ignored();
        self.peek() == Some(c)
    }

    fn name(&mut self) -> Result<Name, FieldSelectionMapParseError> {
        self.skip_ignored();
        let start = self.pos;
        let mut chars = self.rest().char_indices();
        match chars.next() {
            Some((_, c)) if c == '_' || c.is_ascii_alphabetic() => {}
            Some((_, c)) => return Err(self.error(format!("expected a name, found `{c}`"))),
            None => return Err(self.error("expected a name, found end of input")),
        }
        let len = chars
            .find(|(_, c)| !(*c == '_' || c.is_ascii_alphanumeric()))
            .map_or(self.rest().len(), |(i, _)| i);
        self.pos += len;
        Name::new(&self.input[start..self.pos]).map_err(|e| FieldSelectionMapParseError {
            message: e.to_string(),
            offset: start,
        })
    }

    fn at_name_start(&mut self) -> bool {
        self.skip_ignored();
        self.peek()
            .is_some_and(|c| c == '_' || c.is_ascii_alphabetic())
    }

    fn selected_value(&mut self) -> Result<SelectedValue, FieldSelectionMapParseError> {
        self.nested(Self::selected_value_inner)
    }

    fn selected_value_inner(&mut self) -> Result<SelectedValue, FieldSelectionMapParseError> {
        // A leading `|` is allowed.
        self.eat('|');
        let mut alternatives = vec![self.selected_value_entry()?];
        while self.eat('|') {
            alternatives.push(self.selected_value_entry()?);
        }
        Ok(SelectedValue { alternatives })
    }

    fn selected_value_entry(&mut self) -> Result<SelectedValueEntry, FieldSelectionMapParseError> {
        if self.at('{') {
            return Ok(SelectedValueEntry::Object(self.selected_object_value()?));
        }
        let (path, dot_follows) = self.path()?;
        if dot_follows {
            // `Path . SelectedObjectValue`: the path already consumed the dot.
            return Ok(SelectedValueEntry::PathObject(
                path,
                self.selected_object_value()?,
            ));
        }
        if self.at('[') {
            return Ok(SelectedValueEntry::PathList(
                path,
                self.selected_list_value()?,
            ));
        }
        Ok(SelectedValueEntry::Path(path))
    }

    /// Parse a path. Returns whether the path ended with a `.` that is followed by `{`, i.e. the
    /// dot introduces a selected object value rather than another segment.
    fn path(&mut self) -> Result<(Path, bool), FieldSelectionMapParseError> {
        let type_condition = if self.eat('<') {
            let name = self.name()?;
            self.expect('>')?;
            self.expect('.')?;
            Some(name)
        } else {
            None
        };
        let mut segments = Vec::new();
        loop {
            let field = self.name()?;
            let arguments = if self.at('(') {
                self.arguments()?
            } else {
                Vec::new()
            };
            let segment_type_condition = if self.eat('<') {
                let name = self.name()?;
                self.expect('>')?;
                Some(name)
            } else {
                None
            };
            segments.push(PathSegment {
                field,
                arguments,
                type_condition: segment_type_condition.clone(),
            });
            if self.eat('.') {
                if self.at('{') {
                    if segment_type_condition.is_some() {
                        return Err(self.error(
                            "a type condition after a field must be followed by a path segment",
                        ));
                    }
                    return Ok((
                        Path {
                            type_condition,
                            segments,
                        },
                        true,
                    ));
                }
                continue;
            }
            if segment_type_condition.is_some() {
                return Err(
                    self.error("a type condition after a field must be followed by a path segment")
                );
            }
            return Ok((
                Path {
                    type_condition,
                    segments,
                },
                false,
            ));
        }
    }

    fn selected_object_value(
        &mut self,
    ) -> Result<SelectedObjectValue, FieldSelectionMapParseError> {
        self.expect('{')?;
        let mut fields = Vec::new();
        while !self.at('}') {
            if self.peek().is_none() {
                return Err(self.error("expected `}`, found end of input"));
            }
            let name = self.name()?;
            if self.eat(':') {
                fields.push(SelectedObjectField::Labeled(name, self.selected_value()?));
            } else {
                let arguments = if self.at('(') {
                    self.arguments()?
                } else {
                    Vec::new()
                };
                fields.push(SelectedObjectField::Shorthand(name, arguments));
            }
        }
        self.expect('}')?;
        if fields.is_empty() {
            return Err(self.error("a selected object value must select at least one field"));
        }
        Ok(SelectedObjectValue { fields })
    }

    fn selected_list_value(&mut self) -> Result<SelectedListValue, FieldSelectionMapParseError> {
        self.nested(Self::selected_list_value_inner)
    }

    fn selected_list_value_inner(
        &mut self,
    ) -> Result<SelectedListValue, FieldSelectionMapParseError> {
        self.expect('[')?;
        let value = if self.at('[') {
            SelectedListValue::List(Box::new(self.selected_list_value()?))
        } else {
            SelectedListValue::Value(self.selected_value()?)
        };
        self.expect(']')?;
        Ok(value)
    }

    fn arguments(&mut self) -> Result<Vec<Node<ast::Argument>>, FieldSelectionMapParseError> {
        self.expect('(')?;
        let mut arguments = Vec::new();
        while !self.at(')') {
            if self.peek().is_none() {
                return Err(self.error("expected `)`, found end of input"));
            }
            let name = self.name()?;
            self.expect(':')?;
            let value = self.const_value()?;
            arguments.push(Node::new(ast::Argument {
                name,
                value: Node::new(value),
            }));
        }
        self.expect(')')?;
        if arguments.is_empty() {
            return Err(self.error("an argument list must not be empty"));
        }
        Ok(arguments)
    }

    fn const_value(&mut self) -> Result<ast::Value, FieldSelectionMapParseError> {
        self.nested(Self::const_value_inner)
    }

    fn const_value_inner(&mut self) -> Result<ast::Value, FieldSelectionMapParseError> {
        self.skip_ignored();
        match self.peek() {
            Some('$') => Err(self.error("variables are not allowed in a field selection map")),
            Some('"') => self.string_value().map(ast::Value::String),
            Some('[') => {
                self.pos += 1;
                let mut items = Vec::new();
                while !self.at(']') {
                    if self.peek().is_none() {
                        return Err(self.error("expected `]`, found end of input"));
                    }
                    items.push(Node::new(self.const_value()?));
                }
                self.expect(']')?;
                Ok(ast::Value::List(items))
            }
            Some('{') => {
                self.pos += 1;
                let mut fields = Vec::new();
                while !self.at('}') {
                    if self.peek().is_none() {
                        return Err(self.error("expected `}`, found end of input"));
                    }
                    let name = self.name()?;
                    self.expect(':')?;
                    fields.push((name, Node::new(self.const_value()?)));
                }
                self.expect('}')?;
                Ok(ast::Value::Object(fields))
            }
            Some(c) if c == '-' || c.is_ascii_digit() => self.number_value(),
            Some(_) if self.at_name_start() => {
                let name = self.name()?;
                Ok(match name.as_str() {
                    "true" => ast::Value::Boolean(true),
                    "false" => ast::Value::Boolean(false),
                    "null" => ast::Value::Null,
                    _ => ast::Value::Enum(name),
                })
            }
            Some(c) => Err(self.error(format!("expected a value, found `{c}`"))),
            None => Err(self.error("expected a value, found end of input")),
        }
    }

    /// An `IntValue` or `FloatValue` (GraphQL lexical rules). The source text is kept, as
    /// apollo-compiler's parser does: converting through `i32`/`f64` would turn a large integer
    /// into a float, and a float too large for `f64` into `inf`, which is not a GraphQL value.
    /// Range checks happen when the value is coerced to the argument's type.
    fn number_value(&mut self) -> Result<ast::Value, FieldSelectionMapParseError> {
        let start = self.pos;
        let bytes = self.input.as_bytes();
        let mut end = self.pos;
        if bytes.get(end) == Some(&b'-') {
            end += 1;
        }
        let digits_start = end;
        while bytes.get(end).is_some_and(u8::is_ascii_digit) {
            end += 1;
        }
        if end == digits_start {
            self.pos = end;
            return Err(self.error("expected a digit"));
        }
        if bytes[digits_start] == b'0' && end - digits_start > 1 {
            self.pos = digits_start;
            return Err(self.error("a number must not have leading zeros"));
        }
        let mut is_float = false;
        if bytes.get(end) == Some(&b'.') {
            is_float = true;
            end += 1;
            let fraction_start = end;
            while bytes.get(end).is_some_and(u8::is_ascii_digit) {
                end += 1;
            }
            if end == fraction_start {
                self.pos = end;
                return Err(self.error("expected a digit after `.`"));
            }
        }
        if matches!(bytes.get(end), Some(b'e' | b'E')) {
            is_float = true;
            end += 1;
            if matches!(bytes.get(end), Some(b'+' | b'-')) {
                end += 1;
            }
            let exponent_start = end;
            while bytes.get(end).is_some_and(u8::is_ascii_digit) {
                end += 1;
            }
            if end == exponent_start {
                self.pos = end;
                return Err(self.error("expected a digit in the exponent"));
            }
        }
        self.pos = end;
        // A number must not be directly followed by `.` or a name start (e.g. `1b`, `1.2.3`).
        if let Some(&next) = bytes.get(end)
            && (next == b'.' || next == b'_' || next.is_ascii_alphabetic())
        {
            return Err(self.error(format!("unexpected `{}` after a number", next as char)));
        }
        let text = &self.input[start..end];
        Ok(if is_float {
            ast::Value::Float(ast::FloatValue::new_parsed(text))
        } else {
            ast::Value::Int(ast::IntValue::new_parsed(text))
        })
    }

    fn string_value(&mut self) -> Result<String, FieldSelectionMapParseError> {
        if self.rest().starts_with("\"\"\"") {
            self.pos += 3;
            let mut raw = String::new();
            loop {
                let rest = self.rest();
                if rest.starts_with("\\\"\"\"") {
                    raw.push_str("\"\"\"");
                    self.pos += 4;
                } else if rest.starts_with("\"\"\"") {
                    self.pos += 3;
                    return Ok(block_string_value(&raw));
                } else if let Some(c) = rest.chars().next() {
                    raw.push(c);
                    self.pos += c.len_utf8();
                } else {
                    return Err(self.error("unterminated block string"));
                }
            }
        }
        self.pos += 1;
        let mut value = String::new();
        loop {
            let Some(c) = self.peek() else {
                return Err(self.error("unterminated string"));
            };
            self.pos += c.len_utf8();
            match c {
                '"' => return Ok(value),
                '\n' | '\r' => return Err(self.error("unterminated string")),
                '\\' => {
                    let Some(escaped) = self.peek() else {
                        return Err(self.error("unterminated string"));
                    };
                    self.pos += escaped.len_utf8();
                    match escaped {
                        '"' => value.push('"'),
                        '\\' => value.push('\\'),
                        '/' => value.push('/'),
                        'b' => value.push('\u{8}'),
                        'f' => value.push('\u{c}'),
                        'n' => value.push('\n'),
                        'r' => value.push('\r'),
                        't' => value.push('\t'),
                        'u' => {
                            let hex = self.rest().get(..4).unwrap_or_default();
                            let decoded = Some(hex)
                                .filter(|hex| {
                                    hex.len() == 4 && hex.bytes().all(|b| b.is_ascii_hexdigit())
                                })
                                .and_then(|hex| u32::from_str_radix(hex, 16).ok())
                                .and_then(char::from_u32);
                            let Some(decoded) = decoded else {
                                return Err(self.error("invalid unicode escape"));
                            };
                            self.pos += 4;
                            value.push(decoded);
                        }
                        other => {
                            return Err(self.error(format!("invalid escape sequence `\\{other}`")));
                        }
                    }
                }
                c => value.push(c),
            }
        }
    }
}

/// GraphQL's `BlockStringValue`: remove the common indentation of all lines but the first, and
/// leading and trailing blank lines.
fn block_string_value(raw: &str) -> String {
    let lines: Vec<&str> = raw
        .split("\r\n")
        .flat_map(|l| l.split(['\n', '\r']))
        .collect();
    let indent = |line: &str| line.len() - line.trim_start_matches([' ', '\t']).len();
    let is_blank = |line: &str| line.trim_matches([' ', '\t']).is_empty();
    let common_indent = lines
        .iter()
        .skip(1)
        .filter(|line| !is_blank(line))
        .map(|line| indent(line))
        .min()
        .unwrap_or(0);
    let mut lines: Vec<&str> = lines
        .iter()
        .enumerate()
        .map(|(i, line)| {
            if i == 0 {
                line
            } else {
                line.get(common_indent..).unwrap_or("")
            }
        })
        .collect();
    while lines.first().is_some_and(|line| is_blank(line)) {
        lines.remove(0);
    }
    while lines.last().is_some_and(|line| is_blank(line)) {
        lines.pop();
    }
    lines.join("\n")
}
