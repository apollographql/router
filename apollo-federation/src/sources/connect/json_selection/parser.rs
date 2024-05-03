use std::hash::Hash;
use std::hash::Hasher;

use indexmap::IndexSet;
use itertools::Itertools;
use nom::branch::alt;
use nom::character::complete::char;
use nom::character::complete::multispace0;
use nom::character::complete::one_of;
use nom::combinator::all_consuming;
use nom::combinator::map;
use nom::combinator::opt;
use nom::combinator::recognize;
use nom::multi::many0;
use nom::multi::many1;
use nom::sequence::pair;
use nom::sequence::preceded;
use nom::sequence::tuple;
use nom::IResult;
use serde::Serialize;
use serde_json_bytes::json;
use serde_json_bytes::Map;
use serde_json_bytes::Value as JSON;

// Consumes any amount of whitespace and/or comments starting with # until the
// end of the line.
fn spaces_or_comments(input: &str) -> IResult<&str, &str> {
    let mut suffix = input;
    loop {
        (suffix, _) = multispace0(suffix)?;
        let mut chars = suffix.chars();
        if let Some('#') = chars.next() {
            for c in chars.by_ref() {
                if c == '\n' {
                    break;
                }
            }
            suffix = chars.as_str();
        } else {
            return Ok((suffix, &input[0..input.len() - suffix.len()]));
        }
    }
}

// Selection ::= NamedSelection* StarSelection? | PathSelection

#[derive(Debug, PartialEq, Clone, Serialize)]
pub(super) enum Selection {
    // Although we reuse the SubSelection type for the Selection::Named case, we
    // parse it as a sequence of NamedSelection items without the {...} curly
    // braces that SubSelection::parse expects.
    Named(SubSelection),
    Path(PathSelection),
}

impl Selection {
    pub(super) fn parse(input: &str) -> IResult<&str, Self> {
        alt((
            all_consuming(map(
                tuple((
                    many0(NamedSelection::parse),
                    // When a * selection is used, it must be the last selection
                    // in the sequence, since it is not a NamedSelection.
                    opt(StarSelection::parse),
                    // In case there were no named selections and no * selection, we
                    // still want to consume any space before the end of the input.
                    spaces_or_comments,
                )),
                |(selections, star, _)| Self::Named(SubSelection { selections, star }),
            )),
            all_consuming(map(PathSelection::parse, Self::Path)),
        ))(input)
    }
}

// NamedSelection ::=
//     | Alias? Identifier SubSelection?
//     | Alias StringLiteral SubSelection?
//     | Alias PathSelection
//     | Alias SubSelection

#[derive(Debug, PartialEq, Clone, Serialize)]
enum NamedSelection {
    Field(Option<Alias>, String, Option<SubSelection>),
    Quoted(Alias, String, Option<SubSelection>),
    Path(Alias, PathSelection),
    Group(Alias, SubSelection),
}

impl NamedSelection {
    fn parse(input: &str) -> IResult<&str, Self> {
        alt((
            Self::parse_field,
            Self::parse_quoted,
            Self::parse_path,
            Self::parse_group,
        ))(input)
    }

    fn parse_field(input: &str) -> IResult<&str, Self> {
        tuple((
            opt(Alias::parse),
            parse_identifier,
            opt(SubSelection::parse),
        ))(input)
        .map(|(input, (alias, name, selection))| (input, Self::Field(alias, name, selection)))
    }

    fn parse_quoted(input: &str) -> IResult<&str, Self> {
        tuple((Alias::parse, parse_string_literal, opt(SubSelection::parse)))(input)
            .map(|(input, (alias, name, selection))| (input, Self::Quoted(alias, name, selection)))
    }

    fn parse_path(input: &str) -> IResult<&str, Self> {
        tuple((Alias::parse, PathSelection::parse))(input)
            .map(|(input, (alias, path))| (input, Self::Path(alias, path)))
    }

    fn parse_group(input: &str) -> IResult<&str, Self> {
        tuple((Alias::parse, SubSelection::parse))(input)
            .map(|(input, (alias, group))| (input, Self::Group(alias, group)))
    }

    #[allow(dead_code)]
    fn name(&self) -> &str {
        match self {
            Self::Field(alias, name, _) => {
                if let Some(alias) = alias {
                    alias.name.as_str()
                } else {
                    name.as_str()
                }
            }
            Self::Quoted(alias, _, _) => alias.name.as_str(),
            Self::Path(alias, _) => alias.name.as_str(),
            Self::Group(alias, _) => alias.name.as_str(),
        }
    }
}

// PathSelection ::= ("." Property)+ SubSelection?

#[derive(Debug, PartialEq, Clone, Serialize)]
pub(super) enum PathSelection {
    // We use a recursive structure here instead of a Vec<Property> to make
    // applying the selection to a JSON value easier.
    Path(Property, Box<PathSelection>),
    Selection(SubSelection),
    Empty,
}

impl PathSelection {
    fn parse(input: &str) -> IResult<&str, Self> {
        tuple((
            spaces_or_comments,
            many1(preceded(char('.'), Property::parse)),
            opt(SubSelection::parse),
            spaces_or_comments,
        ))(input)
        .map(|(input, (_, path, selection, _))| (input, Self::from_slice(&path, selection)))
    }

    fn from_slice(properties: &[Property], selection: Option<SubSelection>) -> Self {
        match properties {
            [] => selection.map_or(Self::Empty, Self::Selection),
            [head, tail @ ..] => {
                Self::Path(head.clone(), Box::new(Self::from_slice(tail, selection)))
            }
        }
    }
}

// SubSelection ::= "{" NamedSelection* StarSelection? "}"

#[derive(Debug, PartialEq, Clone, Serialize)]
pub(super) struct SubSelection {
    selections: Vec<NamedSelection>,
    star: Option<StarSelection>,
}

impl SubSelection {
    fn parse(input: &str) -> IResult<&str, Self> {
        tuple((
            spaces_or_comments,
            char('{'),
            many0(NamedSelection::parse),
            // Note that when a * selection is used, it must be the last
            // selection in the SubSelection, since it does not count as a
            // NamedSelection, and is stored as a separate field from the
            // selections vector.
            opt(StarSelection::parse),
            spaces_or_comments,
            char('}'),
            spaces_or_comments,
        ))(input)
        .map(|(input, (_, _, selections, star, _, _, _))| (input, Self { selections, star }))
    }
}

// StarSelection ::= Alias? "*" SubSelection?

#[derive(Debug, PartialEq, Clone, Serialize)]
struct StarSelection(Option<Alias>, Option<Box<SubSelection>>);

impl StarSelection {
    fn parse(input: &str) -> IResult<&str, Self> {
        tuple((
            // The spaces_or_comments separators are necessary here because
            // Alias::parse and SubSelection::parse only consume surrounding
            // spaces when they match, and they are both optional here.
            opt(Alias::parse),
            spaces_or_comments,
            char('*'),
            spaces_or_comments,
            opt(SubSelection::parse),
        ))(input)
        .map(|(remainder, (alias, _, _, _, selection))| {
            (remainder, Self(alias, selection.map(Box::new)))
        })
    }
}

// Alias ::= Identifier ":"

#[derive(Debug, PartialEq, Clone, Serialize)]
struct Alias {
    name: String,
}

impl Alias {
    fn parse(input: &str) -> IResult<&str, Self> {
        tuple((parse_identifier, char(':'), spaces_or_comments))(input)
            .map(|(input, (name, _, _))| (input, Self { name }))
    }
}

// Property ::= Identifier | StringLiteral

#[derive(Debug, PartialEq, Clone, Serialize)]
pub(super) enum Property {
    Field(String),
    Quoted(String),
    Index(usize),
}

impl Property {
    fn parse(input: &str) -> IResult<&str, Self> {
        alt((
            map(parse_identifier, Self::Field),
            map(parse_string_literal, Self::Quoted),
        ))(input)
    }
}

// Identifier ::= [a-zA-Z_][0-9a-zA-Z_]*

fn parse_identifier(input: &str) -> IResult<&str, String> {
    tuple((
        spaces_or_comments,
        recognize(pair(
            one_of("abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ_"),
            many0(one_of(
                "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ_0123456789",
            )),
        )),
        spaces_or_comments,
    ))(input)
    .map(|(input, (_, name, _))| (input, name.to_string()))
}

// StringLiteral ::=
//     | "'" ("\'" | [^'])* "'"
//     | '"' ('\"' | [^"])* '"'

fn parse_string_literal(input: &str) -> IResult<&str, String> {
    let input = spaces_or_comments(input).map(|(input, _)| input)?;
    let mut input_char_indices = input.char_indices();

    match input_char_indices.next() {
        Some((0, quote @ '\'')) | Some((0, quote @ '"')) => {
            let mut escape_next = false;
            let mut chars: Vec<char> = vec![];
            let mut remainder: Option<&str> = None;

            for (i, c) in input_char_indices {
                if escape_next {
                    match c {
                        'n' => chars.push('\n'),
                        _ => chars.push(c),
                    }
                    escape_next = false;
                    continue;
                }
                if c == '\\' {
                    escape_next = true;
                    continue;
                }
                if c == quote {
                    remainder = Some(spaces_or_comments(&input[i + 1..])?.0);
                    break;
                }
                chars.push(c);
            }

            if let Some(remainder) = remainder {
                Ok((remainder, chars.iter().collect::<String>()))
            } else {
                Err(nom::Err::Error(nom::error::Error::new(
                    input,
                    nom::error::ErrorKind::Eof,
                )))
            }
        }

        _ => Err(nom::Err::Error(nom::error::Error::new(
            input,
            nom::error::ErrorKind::IsNot,
        ))),
    }
}

/// ApplyTo is a trait for applying a Selection to a JSON value, collecting
/// any/all errors encountered in the process.

pub(crate) trait ApplyTo {
    // Applying a selection to a JSON value produces a new JSON value, along
    // with any/all errors encountered in the process. The value is represented
    // as an Option to allow for undefined/missing values (which JSON does not
    // explicitly support), which are distinct from null values (which it does
    // support).
    fn apply_to(&self, data: &JSON) -> (Option<JSON>, Vec<ApplyToError>) {
        let mut input_path = vec![];
        // Using IndexSet over HashSet to preserve the order of the errors.
        let mut errors = IndexSet::new();
        let value = self.apply_to_path(data, &mut input_path, &mut errors);
        (value, errors.into_iter().collect())
    }

    // This is the trait method that should be implemented and called
    // recursively by the various Selection types.
    fn apply_to_path(
        &self,
        data: &JSON,
        input_path: &mut Vec<Property>,
        errors: &mut IndexSet<ApplyToError>,
    ) -> Option<JSON>;

    // When array is encountered, the Self selection will be applied to each
    // element of the array, producing a new array.
    fn apply_to_array(
        &self,
        data_array: &Vec<JSON>,
        input_path: &mut Vec<Property>,
        errors: &mut IndexSet<ApplyToError>,
    ) -> Option<JSON> {
        let mut output = Vec::with_capacity(data_array.len());

        for (i, element) in data_array.iter().enumerate() {
            input_path.push(Property::Index(i));
            let value = self.apply_to_path(element, input_path, errors);
            input_path.pop();
            // When building an Object, we can simply omit missing properties
            // and report an error, but when building an Array, we need to
            // insert null values to preserve the original array indices/length.
            output.push(value.unwrap_or(JSON::Null));
        }

        Some(JSON::Array(output))
    }
}

#[derive(Debug, Eq, PartialEq, Clone)]
pub(super) struct ApplyToError(JSON);

impl Hash for ApplyToError {
    fn hash<H: Hasher>(&self, hasher: &mut H) {
        // Although serde_json::Value (aka JSON) does not implement the Hash
        // trait, we can convert self.0 to a JSON string and hash that. To do
        // this properly, we should ensure all object keys are serialized in
        // lexicographic order before hashing, but the only object keys we use
        // are "message" and "path", and they always appear in that order.
        self.0.to_string().hash(hasher)
    }
}

impl ApplyToError {
    fn new(message: &str, path: &[Property]) -> Self {
        Self(json!({
            "message": message,
            "path": path.iter().map(|property| match property {
                Property::Field(name) => json!(name),
                Property::Quoted(name) => json!(name),
                Property::Index(index) => json!(index),
            }).collect::<Vec<JSON>>(),
        }))
    }

    #[cfg(test)]
    fn from_json(json: &JSON) -> Self {
        if let JSON::Object(error) = json {
            if let Some(JSON::String(message)) = error.get("message") {
                if let Some(JSON::Array(path)) = error.get("path") {
                    if path
                        .iter()
                        .all(|element| matches!(element, JSON::String(_) | JSON::Number(_)))
                    {
                        // Instead of simply returning Self(json.clone()), we
                        // enforce that the "message" and "path" properties are
                        // always in that order, as promised in the comment in
                        // the hash method above.
                        return Self(json!({
                            "message": message,
                            "path": path,
                        }));
                    }
                }
            }
        }
        panic!("invalid ApplyToError JSON: {:?}", json);
    }

    pub(super) fn message(&self) -> Option<&str> {
        self.0
            .as_object()
            .and_then(|v| v.get("message"))
            .and_then(|s| s.as_str())
    }

    pub(super) fn path(&self) -> Option<String> {
        self.0
            .as_object()
            .and_then(|v| v.get("path"))
            .and_then(|p| p.as_array())
            .map(|l| l.iter().filter_map(|v| v.as_str()).join("."))
    }
}

impl ApplyTo for Selection {
    fn apply_to_path(
        &self,
        data: &JSON,
        input_path: &mut Vec<Property>,
        errors: &mut IndexSet<ApplyToError>,
    ) -> Option<JSON> {
        let data = match data {
            JSON::Array(array) => return self.apply_to_array(array, input_path, errors),
            JSON::Object(_) => data,
            _ => {
                errors.insert(ApplyToError::new("not an object", input_path));
                return None;
            }
        };

        match self {
            // Because we represent a Selection::Named as a SubSelection, we can
            // fully delegate apply_to_path to SubSelection::apply_to_path. Even
            // if we represented Self::Named as a Vec<NamedSelection>, we could
            // still delegate to SubSelection::apply_to_path, but we would need
            // to create a temporary SubSelection to wrap the selections Vec.
            Self::Named(named_selections) => {
                named_selections.apply_to_path(data, input_path, errors)
            }
            Self::Path(path_selection) => path_selection.apply_to_path(data, input_path, errors),
        }
    }
}

impl ApplyTo for NamedSelection {
    fn apply_to_path(
        &self,
        data: &JSON,
        input_path: &mut Vec<Property>,
        errors: &mut IndexSet<ApplyToError>,
    ) -> Option<JSON> {
        let data = match data {
            JSON::Array(array) => return self.apply_to_array(array, input_path, errors),
            JSON::Object(_) => data,
            _ => {
                errors.insert(ApplyToError::new("not an object", input_path));
                return None;
            }
        };

        let mut output = Map::new();

        #[rustfmt::skip] // cargo fmt butchers this closure's formatting
        let mut field_quoted_helper = |
            alias: Option<&Alias>,
            name: &String,
            selection: &Option<SubSelection>,
            input_path: &mut Vec<Property>,
        | {
            if let Some(child) = data.get(name) {
                let output_name = alias.map_or(name, |alias| &alias.name);
                if let Some(selection) = selection {
                    let value = selection.apply_to_path(child, input_path, errors);
                    if let Some(value) = value {
                        output.insert(output_name.clone(), value);
                    }
                } else {
                    output.insert(output_name.clone(), child.clone());
                }
            } else {
                errors.insert(ApplyToError::new(
                    format!("Response field {} not found", name).as_str(),
                    input_path,
                ));
            }
        };

        match self {
            Self::Field(alias, name, selection) => {
                input_path.push(Property::Field(name.clone()));
                field_quoted_helper(alias.as_ref(), name, selection, input_path);
                input_path.pop();
            }
            Self::Quoted(alias, name, selection) => {
                input_path.push(Property::Quoted(name.clone()));
                field_quoted_helper(Some(alias), name, selection, input_path);
                input_path.pop();
            }
            Self::Path(alias, path_selection) => {
                let value = path_selection.apply_to_path(data, input_path, errors);
                if let Some(value) = value {
                    output.insert(alias.name.clone(), value);
                }
            }
            Self::Group(alias, sub_selection) => {
                let value = sub_selection.apply_to_path(data, input_path, errors);
                if let Some(value) = value {
                    output.insert(alias.name.clone(), value);
                }
            }
        };

        Some(JSON::Object(output))
    }
}

impl ApplyTo for PathSelection {
    fn apply_to_path(
        &self,
        data: &JSON,
        input_path: &mut Vec<Property>,
        errors: &mut IndexSet<ApplyToError>,
    ) -> Option<JSON> {
        if let JSON::Array(array) = data {
            return self.apply_to_array(array, input_path, errors);
        }

        match self {
            Self::Path(head, tail) => {
                match data {
                    JSON::Object(_) => {}
                    _ => {
                        errors.insert(ApplyToError::new(
                            format!(
                                "Expected an object in response, received {}",
                                json_type_name(data)
                            )
                            .as_str(),
                            input_path,
                        ));
                        return None;
                    }
                };

                input_path.push(head.clone());
                if let Some(child) = match head {
                    Property::Field(name) => data.get(name),
                    Property::Quoted(name) => data.get(name),
                    Property::Index(index) => data.get(index),
                } {
                    let result = tail.apply_to_path(child, input_path, errors);
                    input_path.pop();
                    result
                } else {
                    let message = match head {
                        Property::Field(name) => format!("Response field {} not found", name),
                        Property::Quoted(name) => format!("Response field {} not found", name),
                        Property::Index(index) => format!("Response field {} not found", index),
                    };
                    errors.insert(ApplyToError::new(message.as_str(), input_path));
                    input_path.pop();
                    None
                }
            }
            Self::Selection(selection) => {
                // If data is not an object here, this recursive apply_to_path
                // call will handle the error.
                selection.apply_to_path(data, input_path, errors)
            }
            Self::Empty => {
                // If data is not an object here, we want to preserve its value
                // without an error.
                Some(data.clone())
            }
        }
    }
}

impl ApplyTo for SubSelection {
    fn apply_to_path(
        &self,
        data: &JSON,
        input_path: &mut Vec<Property>,
        errors: &mut IndexSet<ApplyToError>,
    ) -> Option<JSON> {
        let data_map = match data {
            JSON::Array(array) => return self.apply_to_array(array, input_path, errors),
            JSON::Object(data_map) => data_map,
            _ => {
                errors.insert(ApplyToError::new(
                    format!(
                        "Expected an object in response, received {}",
                        json_type_name(data)
                    )
                    .as_str(),
                    input_path,
                ));
                return None;
            }
        };

        let mut output = Map::new();
        let mut input_names = IndexSet::new();

        for named_selection in &self.selections {
            let value = named_selection.apply_to_path(data, input_path, errors);

            // If value is an object, extend output with its keys and their values.
            if let Some(JSON::Object(key_and_value)) = value {
                output.extend(key_and_value);
            }

            // If there is a star selection, we need to keep track of the
            // *original* names of the fields that were explicitly selected,
            // because we will need to omit them from what the * matches.
            if self.star.is_some() {
                match named_selection {
                    NamedSelection::Field(_, name, _) => {
                        input_names.insert(name.as_str());
                    }
                    NamedSelection::Quoted(_, name, _) => {
                        input_names.insert(name.as_str());
                    }
                    NamedSelection::Path(_, path_selection) => {
                        if let PathSelection::Path(head, _) = path_selection {
                            match head {
                                Property::Field(name) | Property::Quoted(name) => {
                                    input_names.insert(name.as_str());
                                }
                                // While Property::Index may be used to
                                // represent the input_path during apply_to_path
                                // when arrays are encountered, it will never be
                                // used to represent the parsed structure of any
                                // actual selection string, becase arrays are
                                // processed automatically/implicitly and their
                                // indices are never explicitly selected. This
                                // means the numeric Property::Index case cannot
                                // affect the keys selected by * selections, so
                                // input_names does not need updating here.
                                Property::Index(_) => {}
                            };
                        }
                    }
                    // The contents of groups do not affect the keys matched by
                    // * selections in the parent object (outside the group).
                    NamedSelection::Group(_, _) => {}
                };
            }
        }

        match &self.star {
            // Aliased but not subselected, e.g. "a b c rest: *"
            Some(StarSelection(Some(alias), None)) => {
                let mut star_output = Map::new();
                for (key, value) in data_map {
                    if !input_names.contains(key.as_str()) {
                        star_output.insert(key.clone(), value.clone());
                    }
                }
                output.insert(alias.name.clone(), JSON::Object(star_output));
            }
            // Aliased and subselected, e.g. "alias: * { hello }"
            Some(StarSelection(Some(alias), Some(selection))) => {
                let mut star_output = Map::new();
                for (key, value) in data_map {
                    if !input_names.contains(key.as_str()) {
                        if let Some(selected) = selection.apply_to_path(value, input_path, errors) {
                            star_output.insert(key.clone(), selected);
                        }
                    }
                }
                output.insert(alias.name.clone(), JSON::Object(star_output));
            }
            // Not aliased but subselected, e.g. "parent { * { hello } }"
            Some(StarSelection(None, Some(selection))) => {
                for (key, value) in data_map {
                    if !input_names.contains(key.as_str()) {
                        if let Some(selected) = selection.apply_to_path(value, input_path, errors) {
                            output.insert(key.clone(), selected);
                        }
                    }
                }
            }
            // Neither aliased nor subselected, e.g. "parent { * }" or just "*"
            Some(StarSelection(None, None)) => {
                for (key, value) in data_map {
                    if !input_names.contains(key.as_str()) {
                        output.insert(key.clone(), value.clone());
                    }
                }
            }
            // No * selection present, e.g. "parent { just some properties }"
            None => {}
        };

        Some(JSON::Object(output))
    }
}

fn json_type_name(v: &JSON) -> &str {
    match v {
        JSON::Array(_) => "array",
        JSON::Object(_) => "object",
        JSON::String(_) => "string",
        JSON::Number(_) => "number",
        JSON::Bool(_) => "boolean",
        JSON::Null => "null",
    }
}

// GraphQL Selection Set -------------------------------------------------------

use apollo_compiler::ast;
use apollo_compiler::ast::Selection as GraphQLSelection;

struct GraphQLSelections(Vec<Result<GraphQLSelection, String>>);

impl Default for GraphQLSelections {
    fn default() -> Self {
        Self(Default::default())
    }
}

impl GraphQLSelections {
    fn valid_selections(self) -> Vec<GraphQLSelection> {
        self.0.into_iter().filter_map(|i| i.ok()).collect()
    }
}

impl From<Vec<GraphQLSelection>> for GraphQLSelections {
    fn from(val: Vec<GraphQLSelection>) -> Self {
        Self(val.into_iter().map(|i| Ok(i)).collect())
    }
}

impl From<Selection> for Vec<GraphQLSelection> {
    fn from(val: Selection) -> Vec<GraphQLSelection> {
        match val {
            Selection::Named(named_selections) => {
                GraphQLSelections::from(named_selections).valid_selections()
            }
            Selection::Path(path_selection) => path_selection.into(),
        }
    }
}

fn new_field(name: String, selection: Option<GraphQLSelections>) -> GraphQLSelection {
    GraphQLSelection::Field(
        apollo_compiler::ast::Field {
            alias: None,
            name: ast::Name::new_unchecked(name.into()),
            arguments: Default::default(),
            directives: Default::default(),
            selection_set: selection
                .map(GraphQLSelections::valid_selections)
                .unwrap_or_default(),
        }
        .into(),
    )
}

impl From<NamedSelection> for Vec<GraphQLSelection> {
    fn from(val: NamedSelection) -> Vec<GraphQLSelection> {
        match val {
            NamedSelection::Field(alias, name, selection) => vec![new_field(
                alias.map(|a| a.name).unwrap_or(name),
                selection.map(|s| s.into()),
            )],
            NamedSelection::Quoted(alias, _name, selection) => {
                vec![new_field(
                    alias.name,
                    selection.map(|s| GraphQLSelections::from(s)),
                )]
            }
            NamedSelection::Path(alias, path_selection) => {
                let graphql_selection: Vec<GraphQLSelection> = path_selection.into();
                vec![new_field(
                    alias.name,
                    Some(GraphQLSelections::from(graphql_selection)),
                )]
            }
            NamedSelection::Group(alias, sub_selection) => {
                vec![new_field(alias.name, Some(sub_selection.into()))]
            }
        }
    }
}

impl From<PathSelection> for Vec<GraphQLSelection> {
    fn from(val: PathSelection) -> Vec<GraphQLSelection> {
        match val {
            PathSelection::Path(_head, tail) => {
                let tail = *tail;
                tail.into()
            }
            PathSelection::Selection(selection) => {
                GraphQLSelections::from(selection).valid_selections()
            }
            PathSelection::Empty => vec![],
        }
    }
}

impl From<SubSelection> for GraphQLSelections {
    // give as much as we can, yield errors for star selection without alias.
    fn from(val: SubSelection) -> GraphQLSelections {
        let mut selections = val
            .selections
            .into_iter()
            .flat_map(|named_selection| {
                let selections: Vec<GraphQLSelection> = named_selection.into();
                GraphQLSelections::from(selections).0
            })
            .collect::<Vec<Result<GraphQLSelection, String>>>();

        if let Some(StarSelection(alias, sub_selection)) = val.star {
            if let Some(alias) = alias {
                let star = new_field(
                    alias.name,
                    sub_selection.map(|s| GraphQLSelections::from(*s)),
                );
                selections.push(Ok(star));
            } else {
                selections.push(Err(
                    "star selection without alias cannot be converted to GraphQL".to_string(),
                ));
            }
        }
        GraphQLSelections(selections)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // This macro is handy for tests, but it absolutely should never be used with
    // dynamic input at runtime, since it panics if the selection string fails to
    // parse for any reason.
    macro_rules! selection {
        ($input:expr) => {
            if let Ok((remainder, parsed)) = Selection::parse($input) {
                assert_eq!(remainder, "");
                parsed
            } else {
                panic!("invalid selection: {:?}", $input);
            }
        };
    }

    #[test]
    fn test_spaces_or_comments() {
        assert_eq!(spaces_or_comments(""), Ok(("", "")));
        assert_eq!(spaces_or_comments(" "), Ok(("", " ")));
        assert_eq!(spaces_or_comments("  "), Ok(("", "  ")));

        assert_eq!(spaces_or_comments("#"), Ok(("", "#")));
        assert_eq!(spaces_or_comments("# "), Ok(("", "# ")));
        assert_eq!(spaces_or_comments(" # "), Ok(("", " # ")));
        assert_eq!(spaces_or_comments(" #"), Ok(("", " #")));

        assert_eq!(spaces_or_comments("#\n"), Ok(("", "#\n")));
        assert_eq!(spaces_or_comments("# \n"), Ok(("", "# \n")));
        assert_eq!(spaces_or_comments(" # \n"), Ok(("", " # \n")));
        assert_eq!(spaces_or_comments(" #\n"), Ok(("", " #\n")));
        assert_eq!(spaces_or_comments(" # \n "), Ok(("", " # \n ")));

        assert_eq!(spaces_or_comments("hello"), Ok(("hello", "")));
        assert_eq!(spaces_or_comments(" hello"), Ok(("hello", " ")));
        assert_eq!(spaces_or_comments("hello "), Ok(("hello ", "")));
        assert_eq!(spaces_or_comments("hello#"), Ok(("hello#", "")));
        assert_eq!(spaces_or_comments("hello #"), Ok(("hello #", "")));
        assert_eq!(spaces_or_comments("hello # "), Ok(("hello # ", "")));
        assert_eq!(spaces_or_comments("   hello # "), Ok(("hello # ", "   ")));
        assert_eq!(
            spaces_or_comments("  hello # world "),
            Ok(("hello # world ", "  "))
        );

        assert_eq!(spaces_or_comments("#comment"), Ok(("", "#comment")));
        assert_eq!(spaces_or_comments(" #comment"), Ok(("", " #comment")));
        assert_eq!(spaces_or_comments("#comment "), Ok(("", "#comment ")));
        assert_eq!(spaces_or_comments("#comment#"), Ok(("", "#comment#")));
        assert_eq!(spaces_or_comments("#comment #"), Ok(("", "#comment #")));
        assert_eq!(spaces_or_comments("#comment # "), Ok(("", "#comment # ")));
        assert_eq!(
            spaces_or_comments("  #comment # world "),
            Ok(("", "  #comment # world "))
        );
        assert_eq!(
            spaces_or_comments("  # comment # world "),
            Ok(("", "  # comment # world "))
        );

        assert_eq!(
            spaces_or_comments("  # comment\nnot a comment"),
            Ok(("not a comment", "  # comment\n"))
        );
        assert_eq!(
            spaces_or_comments("  # comment\nnot a comment\n"),
            Ok(("not a comment\n", "  # comment\n"))
        );
        assert_eq!(
            spaces_or_comments("not a comment\n  # comment\nasdf"),
            Ok(("not a comment\n  # comment\nasdf", ""))
        );

        #[rustfmt::skip]
        assert_eq!(spaces_or_comments("
            # This is a comment
            # And so is this
            not a comment
        "),
        Ok(("not a comment
        ", "
            # This is a comment
            # And so is this
            ")));

        #[rustfmt::skip]
        assert_eq!(spaces_or_comments("
            # This is a comment
            not a comment
            # Another comment
        "),
        Ok(("not a comment
            # Another comment
        ", "
            # This is a comment
            ")));

        #[rustfmt::skip]
        assert_eq!(spaces_or_comments("
            not a comment
            # This is a comment
            # Another comment
        "),
        Ok(("not a comment
            # This is a comment
            # Another comment
        ", "
            ")));
    }

    #[test]
    fn test_identifier() {
        assert_eq!(parse_identifier("hello"), Ok(("", "hello".to_string())),);

        assert_eq!(
            parse_identifier("hello_world"),
            Ok(("", "hello_world".to_string())),
        );

        assert_eq!(
            parse_identifier("hello_world_123"),
            Ok(("", "hello_world_123".to_string())),
        );

        assert_eq!(parse_identifier(" hello "), Ok(("", "hello".to_string())),);
    }

    #[test]
    fn test_string_literal() {
        assert_eq!(
            parse_string_literal("'hello world'"),
            Ok(("", "hello world".to_string())),
        );
        assert_eq!(
            parse_string_literal("\"hello world\""),
            Ok(("", "hello world".to_string())),
        );
        assert_eq!(
            parse_string_literal("'hello \"world\"'"),
            Ok(("", "hello \"world\"".to_string())),
        );
        assert_eq!(
            parse_string_literal("\"hello \\\"world\\\"\""),
            Ok(("", "hello \"world\"".to_string())),
        );
        assert_eq!(
            parse_string_literal("'hello \\'world\\''"),
            Ok(("", "hello 'world'".to_string())),
        );
    }
    #[test]
    fn test_property() {
        assert_eq!(
            Property::parse("hello"),
            Ok(("", Property::Field("hello".to_string()))),
        );

        assert_eq!(
            Property::parse("'hello'"),
            Ok(("", Property::Quoted("hello".to_string()))),
        );
    }

    #[test]
    fn test_alias() {
        assert_eq!(
            Alias::parse("hello:"),
            Ok((
                "",
                Alias {
                    name: "hello".to_string(),
                },
            )),
        );

        assert_eq!(
            Alias::parse("hello :"),
            Ok((
                "",
                Alias {
                    name: "hello".to_string(),
                },
            )),
        );

        assert_eq!(
            Alias::parse("hello : "),
            Ok((
                "",
                Alias {
                    name: "hello".to_string(),
                },
            )),
        );

        assert_eq!(
            Alias::parse("  hello :"),
            Ok((
                "",
                Alias {
                    name: "hello".to_string(),
                },
            )),
        );

        assert_eq!(
            Alias::parse("hello: "),
            Ok((
                "",
                Alias {
                    name: "hello".to_string(),
                },
            )),
        );
    }

    #[test]
    fn test_named_selection() {
        fn assert_result_and_name(input: &str, expected: NamedSelection, name: &str) {
            let actual = NamedSelection::parse(input);
            assert_eq!(actual, Ok(("", expected.clone())));
            assert_eq!(actual.unwrap().1.name(), name);
            assert_eq!(
                selection!(input),
                Selection::Named(SubSelection {
                    selections: vec![expected],
                    star: None,
                }),
            );
        }

        assert_result_and_name(
            "hello",
            NamedSelection::Field(None, "hello".to_string(), None),
            "hello",
        );

        assert_result_and_name(
            "hello { world }",
            NamedSelection::Field(
                None,
                "hello".to_string(),
                Some(SubSelection {
                    selections: vec![NamedSelection::Field(None, "world".to_string(), None)],
                    star: None,
                }),
            ),
            "hello",
        );

        assert_result_and_name(
            "hi: hello",
            NamedSelection::Field(
                Some(Alias {
                    name: "hi".to_string(),
                }),
                "hello".to_string(),
                None,
            ),
            "hi",
        );

        assert_result_and_name(
            "hi: 'hello world'",
            NamedSelection::Quoted(
                Alias {
                    name: "hi".to_string(),
                },
                "hello world".to_string(),
                None,
            ),
            "hi",
        );

        assert_result_and_name(
            "hi: hello { world }",
            NamedSelection::Field(
                Some(Alias {
                    name: "hi".to_string(),
                }),
                "hello".to_string(),
                Some(SubSelection {
                    selections: vec![NamedSelection::Field(None, "world".to_string(), None)],
                    star: None,
                }),
            ),
            "hi",
        );

        assert_result_and_name(
            "hey: hello { world again }",
            NamedSelection::Field(
                Some(Alias {
                    name: "hey".to_string(),
                }),
                "hello".to_string(),
                Some(SubSelection {
                    selections: vec![
                        NamedSelection::Field(None, "world".to_string(), None),
                        NamedSelection::Field(None, "again".to_string(), None),
                    ],
                    star: None,
                }),
            ),
            "hey",
        );

        assert_result_and_name(
            "hey: 'hello world' { again }",
            NamedSelection::Quoted(
                Alias {
                    name: "hey".to_string(),
                },
                "hello world".to_string(),
                Some(SubSelection {
                    selections: vec![NamedSelection::Field(None, "again".to_string(), None)],
                    star: None,
                }),
            ),
            "hey",
        );

        assert_result_and_name(
            "leggo: 'my ego'",
            NamedSelection::Quoted(
                Alias {
                    name: "leggo".to_string(),
                },
                "my ego".to_string(),
                None,
            ),
            "leggo",
        );
    }

    #[test]
    fn test_selection() {
        assert_eq!(
            selection!(""),
            Selection::Named(SubSelection {
                selections: vec![],
                star: None,
            }),
        );

        assert_eq!(
            selection!("   "),
            Selection::Named(SubSelection {
                selections: vec![],
                star: None,
            }),
        );

        assert_eq!(
            selection!("hello"),
            Selection::Named(SubSelection {
                selections: vec![NamedSelection::Field(None, "hello".to_string(), None),],
                star: None,
            }),
        );

        assert_eq!(
            selection!(".hello"),
            Selection::Path(PathSelection::from_slice(
                &[Property::Field("hello".to_string()),],
                None
            )),
        );

        assert_eq!(
            selection!("hi: .hello.world"),
            Selection::Named(SubSelection {
                selections: vec![NamedSelection::Path(
                    Alias {
                        name: "hi".to_string(),
                    },
                    PathSelection::from_slice(
                        &[
                            Property::Field("hello".to_string()),
                            Property::Field("world".to_string()),
                        ],
                        None
                    ),
                )],
                star: None,
            }),
        );

        assert_eq!(
            selection!("before hi: .hello.world after"),
            Selection::Named(SubSelection {
                selections: vec![
                    NamedSelection::Field(None, "before".to_string(), None),
                    NamedSelection::Path(
                        Alias {
                            name: "hi".to_string(),
                        },
                        PathSelection::from_slice(
                            &[
                                Property::Field("hello".to_string()),
                                Property::Field("world".to_string()),
                            ],
                            None
                        ),
                    ),
                    NamedSelection::Field(None, "after".to_string(), None),
                ],
                star: None,
            }),
        );

        let before_path_nested_after_result = Selection::Named(SubSelection {
            selections: vec![
                NamedSelection::Field(None, "before".to_string(), None),
                NamedSelection::Path(
                    Alias {
                        name: "hi".to_string(),
                    },
                    PathSelection::from_slice(
                        &[
                            Property::Field("hello".to_string()),
                            Property::Field("world".to_string()),
                        ],
                        Some(SubSelection {
                            selections: vec![
                                NamedSelection::Field(None, "nested".to_string(), None),
                                NamedSelection::Field(None, "names".to_string(), None),
                            ],
                            star: None,
                        }),
                    ),
                ),
                NamedSelection::Field(None, "after".to_string(), None),
            ],
            star: None,
        });

        assert_eq!(
            selection!("before hi: .hello.world { nested names } after"),
            before_path_nested_after_result,
        );

        assert_eq!(
            selection!("before hi:.hello.world{nested names}after"),
            before_path_nested_after_result,
        );

        assert_eq!(
            selection!(
                "
            # Comments are supported because we parse them as whitespace
            topLevelAlias: topLevelField {
                # Non-identifier properties must be aliased as an identifier
                nonIdentifier: 'property name with spaces'

                # This extracts the value located at the given path and applies a
                # selection set to it before renaming the result to pathSelection
                pathSelection: .some.nested.path {
                    still: yet
                    more
                    properties
                }

                # An aliased SubSelection of fields nests the fields together
                # under the given alias
                siblingGroup: { brother sister }
            }"
            ),
            Selection::Named(SubSelection {
                selections: vec![NamedSelection::Field(
                    Some(Alias {
                        name: "topLevelAlias".to_string(),
                    }),
                    "topLevelField".to_string(),
                    Some(SubSelection {
                        selections: vec![
                            NamedSelection::Quoted(
                                Alias {
                                    name: "nonIdentifier".to_string(),
                                },
                                "property name with spaces".to_string(),
                                None,
                            ),
                            NamedSelection::Path(
                                Alias {
                                    name: "pathSelection".to_string(),
                                },
                                PathSelection::from_slice(
                                    &[
                                        Property::Field("some".to_string()),
                                        Property::Field("nested".to_string()),
                                        Property::Field("path".to_string()),
                                    ],
                                    Some(SubSelection {
                                        selections: vec![
                                            NamedSelection::Field(
                                                Some(Alias {
                                                    name: "still".to_string(),
                                                }),
                                                "yet".to_string(),
                                                None,
                                            ),
                                            NamedSelection::Field(None, "more".to_string(), None,),
                                            NamedSelection::Field(
                                                None,
                                                "properties".to_string(),
                                                None,
                                            ),
                                        ],
                                        star: None,
                                    })
                                ),
                            ),
                            NamedSelection::Group(
                                Alias {
                                    name: "siblingGroup".to_string(),
                                },
                                SubSelection {
                                    selections: vec![
                                        NamedSelection::Field(None, "brother".to_string(), None,),
                                        NamedSelection::Field(None, "sister".to_string(), None,),
                                    ],
                                    star: None,
                                },
                            ),
                        ],
                        star: None,
                    }),
                )],
                star: None,
            }),
        );
    }

    #[test]
    fn test_path_selection() {
        fn check_path_selection(input: &str, expected: PathSelection) {
            assert_eq!(PathSelection::parse(input), Ok(("", expected.clone())));
            assert_eq!(selection!(input), Selection::Path(expected.clone()));
        }

        check_path_selection(
            ".hello",
            PathSelection::from_slice(&[Property::Field("hello".to_string())], None),
        );

        check_path_selection(
            ".hello.world",
            PathSelection::from_slice(
                &[
                    Property::Field("hello".to_string()),
                    Property::Field("world".to_string()),
                ],
                None,
            ),
        );

        check_path_selection(
            ".hello.world { hello }",
            PathSelection::from_slice(
                &[
                    Property::Field("hello".to_string()),
                    Property::Field("world".to_string()),
                ],
                Some(SubSelection {
                    selections: vec![NamedSelection::Field(None, "hello".to_string(), None)],
                    star: None,
                }),
            ),
        );

        check_path_selection(
            ".nested.'string literal'.\"property\".name",
            PathSelection::from_slice(
                &[
                    Property::Field("nested".to_string()),
                    Property::Quoted("string literal".to_string()),
                    Property::Quoted("property".to_string()),
                    Property::Field("name".to_string()),
                ],
                None,
            ),
        );

        check_path_selection(
            ".nested.'string literal' { leggo: 'my ego' }",
            PathSelection::from_slice(
                &[
                    Property::Field("nested".to_string()),
                    Property::Quoted("string literal".to_string()),
                ],
                Some(SubSelection {
                    selections: vec![NamedSelection::Quoted(
                        Alias {
                            name: "leggo".to_string(),
                        },
                        "my ego".to_string(),
                        None,
                    )],
                    star: None,
                }),
            ),
        );
    }

    #[test]
    fn test_subselection() {
        assert_eq!(
            SubSelection::parse(" { \n } "),
            Ok((
                "",
                SubSelection {
                    selections: vec![],
                    star: None,
                },
            )),
        );

        assert_eq!(
            SubSelection::parse("{hello}"),
            Ok((
                "",
                SubSelection {
                    selections: vec![NamedSelection::Field(None, "hello".to_string(), None),],
                    star: None,
                },
            )),
        );

        assert_eq!(
            SubSelection::parse("{ hello }"),
            Ok((
                "",
                SubSelection {
                    selections: vec![NamedSelection::Field(None, "hello".to_string(), None),],
                    star: None,
                },
            )),
        );

        assert_eq!(
            SubSelection::parse("  { padded  } "),
            Ok((
                "",
                SubSelection {
                    selections: vec![NamedSelection::Field(None, "padded".to_string(), None),],
                    star: None,
                },
            )),
        );

        assert_eq!(
            SubSelection::parse("{ hello world }"),
            Ok((
                "",
                SubSelection {
                    selections: vec![
                        NamedSelection::Field(None, "hello".to_string(), None),
                        NamedSelection::Field(None, "world".to_string(), None),
                    ],
                    star: None,
                },
            )),
        );

        assert_eq!(
            SubSelection::parse("{ hello { world } }"),
            Ok((
                "",
                SubSelection {
                    selections: vec![NamedSelection::Field(
                        None,
                        "hello".to_string(),
                        Some(SubSelection {
                            selections: vec![NamedSelection::Field(
                                None,
                                "world".to_string(),
                                None
                            ),],
                            star: None,
                        })
                    ),],
                    star: None,
                },
            )),
        );
    }

    #[test]
    fn test_star_selection() {
        assert_eq!(
            StarSelection::parse("rest: *"),
            Ok((
                "",
                StarSelection(
                    Some(Alias {
                        name: "rest".to_string(),
                    }),
                    None
                ),
            )),
        );

        assert_eq!(
            StarSelection::parse("*"),
            Ok(("", StarSelection(None, None),)),
        );

        assert_eq!(
            StarSelection::parse(" * "),
            Ok(("", StarSelection(None, None),)),
        );

        assert_eq!(
            StarSelection::parse(" * { hello } "),
            Ok((
                "",
                StarSelection(
                    None,
                    Some(Box::new(SubSelection {
                        selections: vec![NamedSelection::Field(None, "hello".to_string(), None),],
                        star: None,
                    }))
                ),
            )),
        );

        assert_eq!(
            StarSelection::parse("hi: * { hello }"),
            Ok((
                "",
                StarSelection(
                    Some(Alias {
                        name: "hi".to_string(),
                    }),
                    Some(Box::new(SubSelection {
                        selections: vec![NamedSelection::Field(None, "hello".to_string(), None),],
                        star: None,
                    }))
                ),
            )),
        );

        assert_eq!(
            StarSelection::parse("alias: * { x y z rest: * }"),
            Ok((
                "",
                StarSelection(
                    Some(Alias {
                        name: "alias".to_string()
                    }),
                    Some(Box::new(SubSelection {
                        selections: vec![
                            NamedSelection::Field(None, "x".to_string(), None),
                            NamedSelection::Field(None, "y".to_string(), None),
                            NamedSelection::Field(None, "z".to_string(), None),
                        ],
                        star: Some(StarSelection(
                            Some(Alias {
                                name: "rest".to_string(),
                            }),
                            None
                        )),
                    })),
                ),
            )),
        );

        assert_eq!(
            selection!(" before alias: * { * { a b c } } "),
            Selection::Named(SubSelection {
                selections: vec![NamedSelection::Field(None, "before".to_string(), None),],
                star: Some(StarSelection(
                    Some(Alias {
                        name: "alias".to_string()
                    }),
                    Some(Box::new(SubSelection {
                        selections: vec![],
                        star: Some(StarSelection(
                            None,
                            Some(Box::new(SubSelection {
                                selections: vec![
                                    NamedSelection::Field(None, "a".to_string(), None),
                                    NamedSelection::Field(None, "b".to_string(), None),
                                    NamedSelection::Field(None, "c".to_string(), None),
                                ],
                                star: None,
                            }))
                        )),
                    })),
                )),
            }),
        );

        assert_eq!(
            selection!(" before group: { * { a b c } } after "),
            Selection::Named(SubSelection {
                selections: vec![
                    NamedSelection::Field(None, "before".to_string(), None),
                    NamedSelection::Group(
                        Alias {
                            name: "group".to_string(),
                        },
                        SubSelection {
                            selections: vec![],
                            star: Some(StarSelection(
                                None,
                                Some(Box::new(SubSelection {
                                    selections: vec![
                                        NamedSelection::Field(None, "a".to_string(), None),
                                        NamedSelection::Field(None, "b".to_string(), None),
                                        NamedSelection::Field(None, "c".to_string(), None),
                                    ],
                                    star: None,
                                }))
                            )),
                        },
                    ),
                    NamedSelection::Field(None, "after".to_string(), None),
                ],
                star: None,
            }),
        );
    }

    #[test]
    fn test_apply_to_selection() {
        let data = json!({
            "hello": "world",
            "nested": {
                "hello": "world",
                "world": "hello",
            },
            "array": [
                { "hello": "world 0" },
                { "hello": "world 1" },
                { "hello": "world 2" },
            ],
        });

        let check_ok = |selection: Selection, expected_json: JSON| {
            let (actual_json, errors) = selection.apply_to(&data);
            assert_eq!(actual_json, Some(expected_json));
            assert_eq!(errors, vec![]);
        };

        check_ok(selection!("hello"), json!({"hello": "world"}));

        check_ok(
            selection!("nested"),
            json!({
                "nested": {
                    "hello": "world",
                    "world": "hello",
                },
            }),
        );

        check_ok(selection!(".nested.hello"), json!("world"));

        check_ok(selection!(".nested.world"), json!("hello"));

        check_ok(
            selection!("nested hello"),
            json!({
                "hello": "world",
                "nested": {
                    "hello": "world",
                    "world": "hello",
                },
            }),
        );

        check_ok(
            selection!("array { hello }"),
            json!({
                "array": [
                    { "hello": "world 0" },
                    { "hello": "world 1" },
                    { "hello": "world 2" },
                ],
            }),
        );

        check_ok(
            selection!("greetings: array { hello }"),
            json!({
                "greetings": [
                    { "hello": "world 0" },
                    { "hello": "world 1" },
                    { "hello": "world 2" },
                ],
            }),
        );

        check_ok(
            selection!(".array { hello }"),
            json!([
                { "hello": "world 0" },
                { "hello": "world 1" },
                { "hello": "world 2" },
            ]),
        );

        check_ok(
            selection!("worlds: .array.hello"),
            json!({
                "worlds": [
                    "world 0",
                    "world 1",
                    "world 2",
                ],
            }),
        );

        check_ok(
            selection!(".array.hello"),
            json!(["world 0", "world 1", "world 2",]),
        );

        check_ok(
            selection!("nested grouped: { hello worlds: .array.hello }"),
            json!({
                "nested": {
                    "hello": "world",
                    "world": "hello",
                },
                "grouped": {
                    "hello": "world",
                    "worlds": [
                        "world 0",
                        "world 1",
                        "world 2",
                    ],
                },
            }),
        );
    }

    #[test]
    fn test_apply_to_star_selections() {
        let data = json!({
            "englishAndGreekLetters": {
                "a": { "en": "ay", "gr": "alpha" },
                "b": { "en": "bee", "gr": "beta" },
                "c": { "en": "see", "gr": "gamma" },
                "d": { "en": "dee", "gr": "delta" },
                "e": { "en": "ee", "gr": "epsilon" },
                "f": { "en": "eff", "gr": "phi" },
            },
            "englishAndSpanishNumbers": [
                { "en": "one", "es": "uno" },
                { "en": "two", "es": "dos" },
                { "en": "three", "es": "tres" },
                { "en": "four", "es": "cuatro" },
                { "en": "five", "es": "cinco" },
                { "en": "six", "es": "seis" },
            ],
            "asciiCharCodes": {
                "A": 65,
                "B": 66,
                "C": 67,
                "D": 68,
                "E": 69,
                "F": 70,
                "G": 71,
            },
            "books": {
                "9780262533751": {
                    "title": "The Geometry of Meaning",
                    "author": "Peter Gärdenfors",
                },
                "978-1492674313": {
                    "title": "P is for Pterodactyl: The Worst Alphabet Book Ever",
                    "author": "Raj Haldar",
                },
                "9780262542456": {
                    "title": "A Biography of the Pixel",
                    "author": "Alvy Ray Smith",
                },
            }
        });

        let check_ok = |selection: Selection, expected_json: JSON| {
            let (actual_json, errors) = selection.apply_to(&data);
            assert_eq!(actual_json, Some(expected_json));
            assert_eq!(errors, vec![]);
        };

        check_ok(
            selection!("englishAndGreekLetters { * { en }}"),
            json!({
                "englishAndGreekLetters": {
                    "a": { "en": "ay" },
                    "b": { "en": "bee" },
                    "c": { "en": "see" },
                    "d": { "en": "dee" },
                    "e": { "en": "ee" },
                    "f": { "en": "eff" },
                },
            }),
        );

        check_ok(
            selection!("englishAndGreekLetters { C: .c.en * { gr }}"),
            json!({
                "englishAndGreekLetters": {
                    "a": { "gr": "alpha" },
                    "b": { "gr": "beta" },
                    "C": "see",
                    "d": { "gr": "delta" },
                    "e": { "gr": "epsilon" },
                    "f": { "gr": "phi" },
                },
            }),
        );

        check_ok(
            selection!("englishAndGreekLetters { A: a B: b rest: * }"),
            json!({
                "englishAndGreekLetters": {
                    "A": { "en": "ay", "gr": "alpha" },
                    "B": { "en": "bee", "gr": "beta" },
                    "rest": {
                        "c": { "en": "see", "gr": "gamma" },
                        "d": { "en": "dee", "gr": "delta" },
                        "e": { "en": "ee", "gr": "epsilon" },
                        "f": { "en": "eff", "gr": "phi" },
                    },
                },
            }),
        );

        check_ok(
            selection!(".'englishAndSpanishNumbers' { en rest: * }"),
            json!([
                { "en": "one", "rest": { "es": "uno" } },
                { "en": "two", "rest": { "es": "dos" } },
                { "en": "three", "rest": { "es": "tres" } },
                { "en": "four", "rest": { "es": "cuatro" } },
                { "en": "five", "rest": { "es": "cinco" } },
                { "en": "six", "rest": { "es": "seis" } },
            ]),
        );

        // To include/preserve all remaining properties from an object in the output
        // object, we support a naked * selection (no alias or subselection). This
        // is useful when the values of the properties are scalar, so a subselection
        // isn't possible, and we want to preserve all properties of the original
        // object. These unnamed properties may not be useful for GraphQL unless the
        // whole object is considered as opaque JSON scalar data, but we still need
        // to support preserving JSON when it has scalar properties.
        check_ok(
            selection!("asciiCharCodes { ay: A bee: B * }"),
            json!({
                "asciiCharCodes": {
                    "ay": 65,
                    "bee": 66,
                    "C": 67,
                    "D": 68,
                    "E": 69,
                    "F": 70,
                    "G": 71,
                },
            }),
        );

        check_ok(
            selection!("asciiCharCodes { * } gee: .asciiCharCodes.G"),
            json!({
                "asciiCharCodes": data.get("asciiCharCodes").unwrap(),
                "gee": 71,
            }),
        );

        check_ok(
            selection!("books { * { title } }"),
            json!({
                "books": {
                    "9780262533751": {
                        "title": "The Geometry of Meaning",
                    },
                    "978-1492674313": {
                        "title": "P is for Pterodactyl: The Worst Alphabet Book Ever",
                    },
                    "9780262542456": {
                        "title": "A Biography of the Pixel",
                    },
                },
            }),
        );

        check_ok(
            selection!("books { authorsByISBN: * { author } }"),
            json!({
                "books": {
                    "authorsByISBN": {
                        "9780262533751": {
                            "author": "Peter Gärdenfors",
                        },
                        "978-1492674313": {
                            "author": "Raj Haldar",
                        },
                        "9780262542456": {
                            "author": "Alvy Ray Smith",
                        },
                    },
                },
            }),
        );
    }

    #[test]
    fn test_apply_to_errors() {
        let data = json!({
            "hello": "world",
            "nested": {
                "hello": 123,
                "world": true,
            },
            "array": [
                { "hello": 1, "goodbye": "farewell" },
                { "hello": "two" },
                { "hello": 3.0, "smello": "yellow" },
            ],
        });

        assert_eq!(
            selection!("hello").apply_to(&data),
            (Some(json!({"hello": "world"})), vec![],)
        );

        assert_eq!(
            selection!("yellow").apply_to(&data),
            (
                Some(json!({})),
                vec![ApplyToError::from_json(&json!({
                    "message": "Response field yellow not found",
                    "path": ["yellow"],
                })),],
            )
        );

        assert_eq!(
            selection!(".nested.hello").apply_to(&data),
            (Some(json!(123)), vec![],)
        );

        assert_eq!(
            selection!(".nested.'yellow'").apply_to(&data),
            (
                None,
                vec![ApplyToError::from_json(&json!({
                    "message": "Response field yellow not found",
                    "path": ["nested", "yellow"],
                })),],
            )
        );

        assert_eq!(
            selection!(".nested { hola yellow world }").apply_to(&data),
            (
                Some(json!({
                    "world": true,
                })),
                vec![
                    ApplyToError::from_json(&json!({
                        "message": "Response field hola not found",
                        "path": ["nested", "hola"],
                    })),
                    ApplyToError::from_json(&json!({
                        "message": "Response field yellow not found",
                        "path": ["nested", "yellow"],
                    })),
                ],
            )
        );

        assert_eq!(
            selection!("partial: .array { hello goodbye }").apply_to(&data),
            (
                Some(json!({
                    "partial": [
                        { "hello": 1, "goodbye": "farewell" },
                        { "hello": "two" },
                        { "hello": 3.0 },
                    ],
                })),
                vec![
                    ApplyToError::from_json(&json!({
                        "message": "Response field goodbye not found",
                        "path": ["array", 1, "goodbye"],
                    })),
                    ApplyToError::from_json(&json!({
                        "message": "Response field goodbye not found",
                        "path": ["array", 2, "goodbye"],
                    })),
                ],
            )
        );

        assert_eq!(
            selection!("good: .array.hello bad: .array.smello").apply_to(&data),
            (
                Some(json!({
                    "good": [
                        1,
                        "two",
                        3.0,
                    ],
                    "bad": [
                        null,
                        null,
                        "yellow",
                    ],
                })),
                vec![
                    ApplyToError::from_json(&json!({
                        "message": "Response field smello not found",
                        "path": ["array", 0, "smello"],
                    })),
                    ApplyToError::from_json(&json!({
                        "message": "Response field smello not found",
                        "path": ["array", 1, "smello"],
                    })),
                ],
            )
        );

        assert_eq!(
            selection!("array { hello smello }").apply_to(&data),
            (
                Some(json!({
                    "array": [
                        { "hello": 1 },
                        { "hello": "two" },
                        { "hello": 3.0, "smello": "yellow" },
                    ],
                })),
                vec![
                    ApplyToError::from_json(&json!({
                        "message": "Response field smello not found",
                        "path": ["array", 0, "smello"],
                    })),
                    ApplyToError::from_json(&json!({
                        "message": "Response field smello not found",
                        "path": ["array", 1, "smello"],
                    })),
                ],
            )
        );

        assert_eq!(
            selection!(".nested { grouped: { hello smelly world } }").apply_to(&data),
            (
                Some(json!({
                    "grouped": {
                        "hello": 123,
                        "world": true,
                    },
                })),
                vec![ApplyToError::from_json(&json!({
                    "message": "Response field smelly not found",
                    "path": ["nested", "smelly"],
                })),],
            )
        );

        assert_eq!(
            selection!("alias: .nested { grouped: { hello smelly world } }").apply_to(&data),
            (
                Some(json!({
                    "alias": {
                        "grouped": {
                            "hello": 123,
                            "world": true,
                        },
                    },
                })),
                vec![ApplyToError::from_json(&json!({
                    "message": "Response field smelly not found",
                    "path": ["nested", "smelly"],
                })),],
            )
        );
    }

    #[test]
    fn test_apply_to_nested_arrays() {
        let data = json!({
            "arrayOfArrays": [
                [
                    { "x": 0, "y": 0 },
                ],
                [
                    { "x": 1, "y": 0 },
                    { "x": 1, "y": 1 },
                    { "x": 1, "y": 2 },
                ],
                [
                    { "x": 2, "y": 0 },
                    { "x": 2, "y": 1 },
                ],
                [],
                [
                    null,
                    { "x": 4, "y": 1 },
                    { "x": 4, "why": 2 },
                    null,
                    { "x": 4, "y": 4 },
                ]
            ],
        });

        assert_eq!(
            selection!(".arrayOfArrays.x").apply_to(&data),
            (
                Some(json!([[0], [1, 1, 1], [2, 2], [], [null, 4, 4, null, 4],])),
                vec![
                    ApplyToError::from_json(&json!({
                        "message": "Expected an object in response, received null",
                        "path": ["arrayOfArrays", 4, 0],
                    })),
                    ApplyToError::from_json(&json!({
                        "message": "Expected an object in response, received null",
                        "path": ["arrayOfArrays", 4, 3],
                    })),
                ],
            ),
        );

        assert_eq!(
            selection!(".arrayOfArrays.y").apply_to(&data),
            (
                Some(json!([
                    [0],
                    [0, 1, 2],
                    [0, 1],
                    [],
                    [null, 1, null, null, 4],
                ])),
                vec![
                    ApplyToError::from_json(&json!({
                        "message": "Expected an object in response, received null",
                        "path": ["arrayOfArrays", 4, 0],
                    })),
                    ApplyToError::from_json(&json!({
                        "message": "Response field y not found",
                        "path": ["arrayOfArrays", 4, 2, "y"],
                    })),
                    ApplyToError::from_json(&json!({
                        "message": "Expected an object in response, received null",
                        "path": ["arrayOfArrays", 4, 3],
                    })),
                ],
            ),
        );

        assert_eq!(
            selection!("alias: arrayOfArrays { x y }").apply_to(&data),
            (
                Some(json!({
                    "alias": [
                        [
                            { "x": 0, "y": 0 },
                        ],
                        [
                            { "x": 1, "y": 0 },
                            { "x": 1, "y": 1 },
                            { "x": 1, "y": 2 },
                        ],
                        [
                            { "x": 2, "y": 0 },
                            { "x": 2, "y": 1 },
                        ],
                        [],
                        [
                            null,
                            { "x": 4, "y": 1 },
                            { "x": 4 },
                            null,
                            { "x": 4, "y": 4 },
                        ]
                    ],
                })),
                vec![
                    ApplyToError::from_json(&json!({
                        "message": "Expected an object in response, received null",
                        "path": ["arrayOfArrays", 4, 0],
                    })),
                    ApplyToError::from_json(&json!({
                        "message": "Response field y not found",
                        "path": ["arrayOfArrays", 4, 2, "y"],
                    })),
                    ApplyToError::from_json(&json!({
                        "message": "Expected an object in response, received null",
                        "path": ["arrayOfArrays", 4, 3],
                    })),
                ],
            ),
        );

        assert_eq!(
            selection!("ys: .arrayOfArrays.y xs: .arrayOfArrays.x").apply_to(&data),
            (
                Some(json!({
                    "ys": [
                        [0],
                        [0, 1, 2],
                        [0, 1],
                        [],
                        [null, 1, null, null, 4],
                    ],
                    "xs": [
                        [0],
                        [1, 1, 1],
                        [2, 2],
                        [],
                        [null, 4, 4, null, 4],
                    ],
                })),
                vec![
                    ApplyToError::from_json(&json!({
                        "message": "Expected an object in response, received null",
                        "path": ["arrayOfArrays", 4, 0],
                    })),
                    ApplyToError::from_json(&json!({
                        "message": "Response field y not found",
                        "path": ["arrayOfArrays", 4, 2, "y"],
                    })),
                    ApplyToError::from_json(&json!({
                        // Reversing the order of "path" and "message" here to make
                        // sure that doesn't affect the deduplication logic.
                        "path": ["arrayOfArrays", 4, 3],
                        "message": "Expected an object in response, received null",
                    })),
                    // These errors have already been reported along different paths, above.
                    // ApplyToError::from_json(&json!({
                    //     "message": "not an object",
                    //     "path": ["arrayOfArrays", 4, 0],
                    // })),
                    // ApplyToError::from_json(&json!({
                    //     "message": "not an object",
                    //     "path": ["arrayOfArrays", 4, 3],
                    // })),
                ],
            ),
        );
    }

    #[test]
    fn test_apply_to_non_identifier_properties() {
        let data = json!({
            "not an identifier": [
                { "also.not.an.identifier": 0 },
                { "also.not.an.identifier": 1 },
                { "also.not.an.identifier": 2 },
            ],
            "another": {
                "pesky string literal!": {
                    "identifier": 123,
                    "{ evil braces }": true,
                },
            },
        });

        assert_eq!(
            // The grammar enforces that we must always provide identifier aliases
            // for non-identifier properties, so the data we get back will always be
            // GraphQL-safe.
            selection!("alias: 'not an identifier' { safe: 'also.not.an.identifier' }")
                .apply_to(&data),
            (
                Some(json!({
                    "alias": [
                        { "safe": 0 },
                        { "safe": 1 },
                        { "safe": 2 },
                    ],
                })),
                vec![],
            ),
        );

        assert_eq!(
            selection!(".'not an identifier'.'also.not.an.identifier'").apply_to(&data),
            (Some(json!([0, 1, 2])), vec![],),
        );

        assert_eq!(
            selection!(".\"not an identifier\" { safe: \"also.not.an.identifier\" }")
                .apply_to(&data),
            (
                Some(json!([
                    { "safe": 0 },
                    { "safe": 1 },
                    { "safe": 2 },
                ])),
                vec![],
            ),
        );

        assert_eq!(
            selection!(
                "another {
                pesky: 'pesky string literal!' {
                    identifier
                    evil: '{ evil braces }'
                }
            }"
            )
            .apply_to(&data),
            (
                Some(json!({
                    "another": {
                        "pesky": {
                            "identifier": 123,
                            "evil": true,
                        },
                    },
                })),
                vec![],
            ),
        );

        assert_eq!(
            selection!(".another.'pesky string literal!'.'{ evil braces }'").apply_to(&data),
            (Some(json!(true)), vec![],),
        );

        assert_eq!(
            selection!(".another.'pesky string literal!'.\"identifier\"").apply_to(&data),
            (Some(json!(123)), vec![],),
        );
    }

    use apollo_compiler::ast::Selection as GraphQLSelection;

    fn print_set(set: &[apollo_compiler::ast::Selection]) -> String {
        set.iter()
            .map(|s| s.serialize().to_string())
            .collect::<Vec<_>>()
            .join(" ")
    }

    #[test]
    fn into_selection_set() {
        let selection = selection!("f");
        let set: Vec<GraphQLSelection> = selection.try_into().unwrap();
        assert_eq!(print_set(&set), "f");

        let selection = selection!("f f2 f3");
        let set: Vec<GraphQLSelection> = selection.try_into().unwrap();
        assert_eq!(print_set(&set), "f f2 f3");

        let selection = selection!("f { f2 f3 }");
        let set: Vec<GraphQLSelection> = selection.try_into().unwrap();
        assert_eq!(print_set(&set), "f {\n  f2\n  f3\n}");

        let selection = selection!("a: f { b: f2 }");
        let set: Vec<GraphQLSelection> = selection.try_into().unwrap();
        assert_eq!(print_set(&set), "a {\n  b\n}");

        let selection = selection!(".a { b c }");
        let set: Vec<GraphQLSelection> = selection.try_into().unwrap();
        assert_eq!(print_set(&set), "b c");

        let selection = selection!(".a.b { c: .d e }");
        let set: Vec<GraphQLSelection> = selection.try_into().unwrap();
        assert_eq!(print_set(&set), "c e");

        let selection = selection!("a: { b c }");
        let set: Vec<GraphQLSelection> = selection.try_into().unwrap();
        assert_eq!(print_set(&set), "a {\n  b\n  c\n}");

        let selection = selection!("a: 'quoted'");
        let set: Vec<GraphQLSelection> = selection.try_into().unwrap();
        assert_eq!(print_set(&set), "a");

        let selection = selection!("a b: *");
        let set: Vec<GraphQLSelection> = selection.try_into().unwrap();
        assert_eq!(print_set(&set), "a b");

        let selection = selection!("a *");
        let set: Vec<GraphQLSelection> = selection.try_into().unwrap();
        assert_eq!(print_set(&set), "a");
    }
}
