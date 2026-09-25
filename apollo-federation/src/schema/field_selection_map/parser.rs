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

/// Parse a `FieldSelectionMap` string.
pub(crate) fn parse(input: &str) -> Result<SelectedValue, FieldSelectionMapParseError> {
    let mut parser = Parser { input, pos: 0 };
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
                Some(c) if c.is_whitespace() || c == ',' || c == '\u{feff}' => {
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
            return Err(self.error("expected a digit"));
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
        let text = &self.input[start..end];
        let value = if is_float {
            text.parse::<f64>()
                .map(|f| ast::Value::Float(f.into()))
                .map_err(|e| e.to_string())
        } else {
            text.parse::<i32>()
                .map(|i| ast::Value::Int(i.into()))
                .or_else(|_| {
                    // Out-of-range integers are kept as floats, matching GraphQL coercion rules
                    // closely enough for literals in a selection map.
                    text.parse::<f64>()
                        .map(|f| ast::Value::Float(f.into()))
                        .map_err(|e| e.to_string())
                })
        };
        value.map_err(|message| FieldSelectionMapParseError {
            message,
            offset: start,
        })
    }

    fn string_value(&mut self) -> Result<String, FieldSelectionMapParseError> {
        if self.rest().starts_with("\"\"\"") {
            self.pos += 3;
            let Some(end) = self.rest().find("\"\"\"") else {
                return Err(self.error("unterminated block string"));
            };
            let raw = self.rest()[..end].replace("\\\"\"\"", "\"\"\"");
            self.pos += end + 3;
            return Ok(raw);
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
                            let decoded = u32::from_str_radix(hex, 16)
                                .ok()
                                .filter(|_| hex.len() == 4)
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
