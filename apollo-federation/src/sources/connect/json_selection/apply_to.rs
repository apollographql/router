/// ApplyTo is a trait for applying a JSONSelection to a JSON value, collecting
/// any/all errors encountered in the process.
use std::hash::Hash;
use std::hash::Hasher;

use apollo_compiler::collections::IndexMap;
use apollo_compiler::collections::IndexSet;
use itertools::Itertools;
use serde_json_bytes::json;
use serde_json_bytes::Map as JSONMap;
use serde_json_bytes::Value as JSON;

use super::helpers::json_type_name;
use super::immutable::InputPath;
use super::lit_expr::LitExpr;
use super::methods::ARROW_METHODS;
use super::parser::*;

pub(super) type VarsWithPathsMap<'a> = IndexMap<String, (&'a JSON, InputPath<JSON>)>;

pub trait ApplyTo {
    // Applying a selection to a JSON value produces a new JSON value, along
    // with any/all errors encountered in the process. The value is represented
    // as an Option to allow for undefined/missing values (which JSON does not
    // explicitly support), which are distinct from null values (which it does
    // support).
    fn apply_to(&self, data: &JSON) -> (Option<JSON>, Vec<ApplyToError>) {
        self.apply_with_vars(data, &IndexMap::default())
    }

    fn apply_with_vars(
        &self,
        data: &JSON,
        vars: &IndexMap<String, JSON>,
    ) -> (Option<JSON>, Vec<ApplyToError>) {
        let mut vars_with_paths: VarsWithPathsMap = IndexMap::default();
        for (var_name, var_data) in vars {
            vars_with_paths.insert(
                var_name.to_string(),
                (var_data, InputPath::Empty.append(json!(var_name))),
            );
        }
        // The $ variable initially refers to the root data value, but is
        // rebound by nested selection sets to refer to the root value the
        // selection set was applied to.
        vars_with_paths.insert("$".to_string(), (data, InputPath::Empty));
        // Using IndexSet over HashSet to preserve the order of the errors.
        let mut errors = IndexSet::default();
        let value = self.apply_to_path(data, &vars_with_paths, &InputPath::Empty, &mut errors);
        (value, errors.into_iter().collect())
    }

    // This is the trait method that should be implemented and called
    // recursively by the various JSONSelection types.
    fn apply_to_path(
        &self,
        data: &JSON,
        vars: &VarsWithPathsMap,
        input_path: &InputPath<JSON>,
        errors: &mut IndexSet<ApplyToError>,
    ) -> Option<JSON>;

    // When array is encountered, the Self selection will be applied to each
    // element of the array, producing a new array.
    fn apply_to_array(
        &self,
        data_array: &[JSON],
        vars: &VarsWithPathsMap,
        input_path: &InputPath<JSON>,
        errors: &mut IndexSet<ApplyToError>,
    ) -> Option<JSON> {
        let mut output = Vec::with_capacity(data_array.len());

        for (i, element) in data_array.iter().enumerate() {
            let input_path_with_index = input_path.append(json!(i));
            let applied = self.apply_to_path(element, vars, &input_path_with_index, errors);
            // When building an Object, we can simply omit missing properties
            // and report an error, but when building an Array, we need to
            // insert null values to preserve the original array indices/length.
            output.push(applied.unwrap_or(JSON::Null));
        }

        Some(JSON::Array(output))
    }
}

#[derive(Debug, Eq, PartialEq, Clone)]
pub struct ApplyToError(JSON);

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
    pub(crate) fn new(message: String, path: Vec<JSON>) -> Self {
        Self(json!({
            "message": message,
            "path": JSON::Array(path),
        }))
    }

    #[cfg(test)]
    pub(crate) fn from_json(json: &JSON) -> Self {
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

    pub fn message(&self) -> Option<&str> {
        self.0
            .as_object()
            .and_then(|v| v.get("message"))
            .and_then(|s| s.as_str())
    }

    pub fn path(&self) -> Option<String> {
        self.0
            .as_object()
            .and_then(|v| v.get("path"))
            .and_then(|p| p.as_array())
            .map(|l| l.iter().filter_map(|v| v.as_str()).join("."))
    }
}

impl ApplyTo for JSONSelection {
    fn apply_to_path(
        &self,
        data: &JSON,
        vars: &VarsWithPathsMap,
        input_path: &InputPath<JSON>,
        errors: &mut IndexSet<ApplyToError>,
    ) -> Option<JSON> {
        match self {
            // Because we represent a JSONSelection::Named as a SubSelection, we
            // can fully delegate apply_to_path to SubSelection::apply_to_path.
            // Even if we represented Self::Named as a Vec<NamedSelection>, we
            // could still delegate to SubSelection::apply_to_path, but we would
            // need to create a temporary SubSelection to wrap the selections
            // Vec.
            Self::Named(named_selections) => {
                named_selections.apply_to_path(data, vars, input_path, errors)
            }
            Self::Path(path_selection) => {
                path_selection.apply_to_path(data, vars, input_path, errors)
            }
        }
    }
}

impl ApplyTo for NamedSelection {
    fn apply_to_path(
        &self,
        data: &JSON,
        vars: &VarsWithPathsMap,
        input_path: &InputPath<JSON>,
        errors: &mut IndexSet<ApplyToError>,
    ) -> Option<JSON> {
        if let JSON::Array(array) = data {
            return self.apply_to_array(array, vars, input_path, errors);
        }

        let mut output = JSONMap::new();

        #[rustfmt::skip] // cargo fmt butchers this closure's formatting
        let mut field_quoted_helper = |
            alias: Option<&Alias>,
            key: Key,
            selection: &Option<SubSelection>,
        | {
            let input_path_with_key = input_path.append(key.to_json());
            let name = key.as_string();
            if let Some(child) = data.get(name.clone()) {
                let output_name = alias.map_or(&name, |alias| &alias.name);
                if let Some(selection) = selection {
                    let value = selection.apply_to_path(child, vars, &input_path_with_key, errors);
                    if let Some(value) = value {
                        output.insert(output_name.clone(), value);
                    }
                } else {
                    output.insert(output_name.clone(), child.clone());
                }
            } else {
                errors.insert(ApplyToError::new(
                    format!(
                        "Property {} not found in {}",
                        key.dotted(),
                        json_type_name(data),
                    ),
                    input_path_with_key.to_vec(),
                ));
            }
        };

        match self {
            Self::Field(alias, name, selection) => {
                field_quoted_helper(alias.as_ref(), Key::Field(name.clone()), selection);
            }
            Self::Quoted(alias, name, selection) => {
                field_quoted_helper(Some(alias), Key::Quoted(name.clone()), selection);
            }
            Self::Path(alias, path_selection) => {
                let value = path_selection.apply_to_path(data, vars, input_path, errors);
                if let Some(value) = value {
                    output.insert(alias.name.clone(), value);
                }
            }
            Self::Group(alias, sub_selection) => {
                let value = sub_selection.apply_to_path(data, vars, input_path, errors);
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
        vars: &VarsWithPathsMap,
        input_path: &InputPath<JSON>,
        errors: &mut IndexSet<ApplyToError>,
    ) -> Option<JSON> {
        match &self.path {
            // If this is a KeyPath, instead of using data as given, we need to
            // evaluate the path starting from the current value of $. To
            // evaluate the KeyPath against data, prefix it with @. This logic
            // supports method chaining like obj->has('a')->and(obj->has('b')),
            // where both obj references are interpreted as $.obj.
            PathList::Key(key, tail) => {
                if let Some((dollar_data, dollar_path)) = vars.get("$") {
                    let input_path_with_key = dollar_path.append(key.to_json());
                    if let Some(child) = dollar_data.get(key.as_string()) {
                        tail.apply_to_path(child, vars, &input_path_with_key, errors)
                    } else {
                        errors.insert(ApplyToError::new(
                            format!(
                                "Property {} not found in {}",
                                key.dotted(),
                                json_type_name(dollar_data),
                            ),
                            input_path_with_key.to_vec(),
                        ));
                        None
                    }
                } else {
                    // If $ is undefined for some reason, fall back to using data.
                    self.path.apply_to_path(data, vars, input_path, errors)
                }
            }
            path => path.apply_to_path(data, vars, input_path, errors),
        }
    }
}

impl ApplyTo for PathList {
    fn apply_to_path(
        &self,
        data: &JSON,
        vars: &VarsWithPathsMap,
        input_path: &InputPath<JSON>,
        errors: &mut IndexSet<ApplyToError>,
    ) -> Option<JSON> {
        match self {
            Self::Var(var_name, tail) => {
                if var_name == "@" {
                    // We represent @ as a variable name in PathList::Var, but
                    // it is never stored in the vars map, because it is always
                    // shorthand for the current data value.
                    tail.apply_to_path(data, vars, input_path, errors)
                } else if let Some((var_data, var_path)) = vars.get(var_name) {
                    // Variables are associated with a path, which is always
                    // just the variable name for named $variables other than $.
                    // For the special variable $, the path represents the
                    // sequence of keys from the root input data to the $ data.
                    tail.apply_to_path(var_data, vars, var_path, errors)
                } else {
                    errors.insert(ApplyToError::new(
                        format!("Variable {} not found", var_name),
                        input_path.to_vec(),
                    ));
                    None
                }
            }
            Self::Key(key, tail) => {
                if let JSON::Array(array) = data {
                    return self.apply_to_array(array, vars, input_path, errors);
                }

                let input_path_with_key = input_path.append(key.to_json());

                if !matches!(data, JSON::Object(_)) {
                    errors.insert(ApplyToError::new(
                        format!(
                            "Property {} not found in {}",
                            key.dotted(),
                            json_type_name(data),
                        ),
                        input_path_with_key.to_vec(),
                    ));
                    return None;
                }

                if let Some(child) = match key {
                    Key::Field(name) => data.get(name),
                    Key::Quoted(name) => data.get(name),
                } {
                    tail.apply_to_path(child, vars, &input_path_with_key, errors)
                } else {
                    errors.insert(ApplyToError::new(
                        format!(
                            "Property {} not found in {}",
                            key.dotted(),
                            json_type_name(data),
                        ),
                        input_path_with_key.to_vec(),
                    ));
                    None
                }
            }
            Self::Method(method_name, method_args, tail) => {
                if let Some(method) = ARROW_METHODS.get(method_name) {
                    method(
                        method_name.as_str(),
                        method_args,
                        data,
                        vars,
                        input_path,
                        tail.as_ref(),
                        errors,
                    )
                } else {
                    errors.insert(ApplyToError::new(
                        format!("Method ->{} not found", method_name),
                        input_path.to_vec(),
                    ));
                    None
                }
            }
            Self::Selection(selection) => selection.apply_to_path(data, vars, input_path, errors),
            Self::Empty => {
                // If data is not an object here, we want to preserve its value
                // without an error.
                Some(data.clone())
            }
        }
    }
}

impl ApplyTo for LitExpr {
    fn apply_to_path(
        &self,
        data: &JSON,
        vars: &VarsWithPathsMap,
        input_path: &InputPath<JSON>,
        errors: &mut IndexSet<ApplyToError>,
    ) -> Option<JSON> {
        match self {
            Self::String(s) => Some(JSON::String(s.clone().into())),
            Self::Number(n) => {
                if let Ok(JSON::Number(n)) = serde_json_bytes::serde_json::from_str(n) {
                    Some(JSON::Number(n))
                } else {
                    errors.insert(ApplyToError::new(
                        format!("Invalid number {}", n),
                        input_path.to_vec(),
                    ));
                    None
                }
            }
            Self::Bool(b) => Some(JSON::Bool(*b)),
            Self::Null => Some(JSON::Null),
            Self::Object(map) => {
                let mut output = JSONMap::new();
                for (key, value) in map {
                    if let Some(value_json) = value.apply_to_path(data, vars, input_path, errors) {
                        output.insert(key.clone(), value_json);
                    }
                }
                Some(JSON::Object(output))
            }
            Self::Array(vec) => {
                let mut output = vec![];
                for value in vec {
                    output.push(
                        value
                            .apply_to_path(data, vars, input_path, errors)
                            .unwrap_or(JSON::Null),
                    );
                }
                Some(JSON::Array(output))
            }
            Self::Path(path) => path.apply_to_path(data, vars, input_path, errors),
        }
    }
}

impl ApplyTo for SubSelection {
    fn apply_to_path(
        &self,
        data: &JSON,
        vars: &VarsWithPathsMap,
        input_path: &InputPath<JSON>,
        errors: &mut IndexSet<ApplyToError>,
    ) -> Option<JSON> {
        if let JSON::Array(array) = data {
            return self.apply_to_array(array, vars, input_path, errors);
        }

        let vars: VarsWithPathsMap = {
            let mut vars = vars.clone();
            vars.insert("$".to_string(), (data, input_path.clone()));
            vars
        };

        let (data_map, data_really_primitive) = match data {
            JSON::Object(data_map) => (data_map.clone(), false),
            _primitive => (JSONMap::new(), true),
        };

        let mut output = JSONMap::new();
        let mut input_names = IndexSet::default();

        for named_selection in &self.selections {
            let value = named_selection.apply_to_path(data, &vars, input_path, errors);

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
                        if let PathList::Key(key, _) = &path_selection.path {
                            match key {
                                Key::Field(name) | Key::Quoted(name) => {
                                    input_names.insert(name.as_str());
                                }
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
                let mut star_output = JSONMap::new();
                for (key, value) in &data_map {
                    if !input_names.contains(key.as_str()) {
                        star_output.insert(key.clone(), value.clone());
                    }
                }
                output.insert(alias.name.clone(), JSON::Object(star_output));
            }
            // Aliased and subselected, e.g. "alias: * { hello }"
            Some(StarSelection(Some(alias), Some(selection))) => {
                let mut star_output = JSONMap::new();
                for (key, value) in &data_map {
                    if !input_names.contains(key.as_str()) {
                        if let Some(selected) =
                            selection.apply_to_path(value, &vars, input_path, errors)
                        {
                            star_output.insert(key.clone(), selected);
                        }
                    }
                }
                output.insert(alias.name.clone(), JSON::Object(star_output));
            }
            // Not aliased but subselected, e.g. "parent { * { hello } }"
            Some(StarSelection(None, Some(selection))) => {
                for (key, value) in &data_map {
                    if !input_names.contains(key.as_str()) {
                        if let Some(selected) =
                            selection.apply_to_path(value, &vars, input_path, errors)
                        {
                            output.insert(key.clone(), selected);
                        }
                    }
                }
            }
            // Neither aliased nor subselected, e.g. "parent { * }" or just "*"
            Some(StarSelection(None, None)) => {
                for (key, value) in &data_map {
                    if !input_names.contains(key.as_str()) {
                        output.insert(key.clone(), value.clone());
                    }
                }
            }
            // No * selection present, e.g. "parent { just some properties }"
            None => {}
        };

        if data_really_primitive && output.is_empty() {
            return Some(data.clone());
        }

        Some(JSON::Object(output))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::selection;

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

        let check_ok = |selection: JSONSelection, expected_json: JSON| {
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
        check_ok(selection!("$.nested.hello"), json!("world"));

        check_ok(selection!(".nested.world"), json!("hello"));
        check_ok(selection!("$.nested.world"), json!("hello"));

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
            selection!("worlds: $.array.hello"),
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
            selection!("$.array.hello"),
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

        check_ok(
            selection!("nested grouped: { hello worlds: $.array.hello }"),
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

        let check_ok = |selection: JSONSelection, expected_json: JSON| {
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

        let yellow_errors_expected = vec![ApplyToError::from_json(&json!({
            "message": "Property .yellow not found in object",
            "path": ["yellow"],
        }))];
        assert_eq!(
            selection!("yellow").apply_to(&data),
            (Some(json!({})), yellow_errors_expected.clone())
        );
        assert_eq!(
            selection!(".yellow").apply_to(&data),
            (None, yellow_errors_expected.clone())
        );
        assert_eq!(
            selection!("$.yellow").apply_to(&data),
            (None, yellow_errors_expected.clone())
        );

        assert_eq!(
            selection!(".nested.hello").apply_to(&data),
            (Some(json!(123)), vec![],)
        );

        let quoted_yellow_expected = (
            None,
            vec![ApplyToError::from_json(&json!({
                "message": "Property .\"yellow\" not found in object",
                "path": ["nested", "yellow"],
            }))],
        );
        assert_eq!(
            selection!(".nested.'yellow'").apply_to(&data),
            quoted_yellow_expected,
        );
        assert_eq!(
            selection!("$.nested.'yellow'").apply_to(&data),
            quoted_yellow_expected,
        );

        let nested_path_expected = (
            Some(json!({
                "world": true,
            })),
            vec![
                ApplyToError::from_json(&json!({
                    "message": "Property .hola not found in object",
                    "path": ["nested", "hola"],
                })),
                ApplyToError::from_json(&json!({
                    "message": "Property .yellow not found in object",
                    "path": ["nested", "yellow"],
                })),
            ],
        );
        assert_eq!(
            selection!(".nested { hola yellow world }").apply_to(&data),
            nested_path_expected,
        );
        assert_eq!(
            selection!("$.nested { hola yellow world }").apply_to(&data),
            nested_path_expected,
        );

        let partial_array_expected = (
            Some(json!({
                "partial": [
                    { "hello": 1, "goodbye": "farewell" },
                    { "hello": "two" },
                    { "hello": 3.0 },
                ],
            })),
            vec![
                ApplyToError::from_json(&json!({
                    "message": "Property .goodbye not found in object",
                    "path": ["array", 1, "goodbye"],
                })),
                ApplyToError::from_json(&json!({
                    "message": "Property .goodbye not found in object",
                    "path": ["array", 2, "goodbye"],
                })),
            ],
        );
        assert_eq!(
            selection!("partial: .array { hello goodbye }").apply_to(&data),
            partial_array_expected,
        );
        assert_eq!(
            selection!("partial: $.array { hello goodbye }").apply_to(&data),
            partial_array_expected,
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
                        "message": "Property .smello not found in object",
                        "path": ["array", 0, "smello"],
                    })),
                    ApplyToError::from_json(&json!({
                        "message": "Property .smello not found in object",
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
                        "message": "Property .smello not found in object",
                        "path": ["array", 0, "smello"],
                    })),
                    ApplyToError::from_json(&json!({
                        "message": "Property .smello not found in object",
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
                    "message": "Property .smelly not found in object",
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
                    "message": "Property .smelly not found in object",
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

        let array_of_arrays_x_expected = (
            Some(json!([[0], [1, 1, 1], [2, 2], [], [null, 4, 4, null, 4],])),
            vec![
                ApplyToError::from_json(&json!({
                    "message": "Property .x not found in null",
                    "path": ["arrayOfArrays", 4, 0, "x"],
                })),
                ApplyToError::from_json(&json!({
                    "message": "Property .x not found in null",
                    "path": ["arrayOfArrays", 4, 3, "x"],
                })),
            ],
        );
        assert_eq!(
            selection!(".arrayOfArrays.x").apply_to(&data),
            array_of_arrays_x_expected,
        );
        assert_eq!(
            selection!("$.arrayOfArrays.x").apply_to(&data),
            array_of_arrays_x_expected,
        );

        let array_of_arrays_y_expected = (
            Some(json!([
                [0],
                [0, 1, 2],
                [0, 1],
                [],
                [null, 1, null, null, 4],
            ])),
            vec![
                ApplyToError::from_json(&json!({
                    "message": "Property .y not found in null",
                    "path": ["arrayOfArrays", 4, 0, "y"],
                })),
                ApplyToError::from_json(&json!({
                    "message": "Property .y not found in object",
                    "path": ["arrayOfArrays", 4, 2, "y"],
                })),
                ApplyToError::from_json(&json!({
                    "message": "Property .y not found in null",
                    "path": ["arrayOfArrays", 4, 3, "y"],
                })),
            ],
        );
        assert_eq!(
            selection!(".arrayOfArrays.y").apply_to(&data),
            array_of_arrays_y_expected
        );
        assert_eq!(
            selection!("$.arrayOfArrays.y").apply_to(&data),
            array_of_arrays_y_expected
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
                        "message": "Property .x not found in null",
                        "path": ["arrayOfArrays", 4, 0, "x"],
                    })),
                    ApplyToError::from_json(&json!({
                        "message": "Property .y not found in null",
                        "path": ["arrayOfArrays", 4, 0, "y"],
                    })),
                    ApplyToError::from_json(&json!({
                        "message": "Property .y not found in object",
                        "path": ["arrayOfArrays", 4, 2, "y"],
                    })),
                    ApplyToError::from_json(&json!({
                        "message": "Property .x not found in null",
                        "path": ["arrayOfArrays", 4, 3, "x"],
                    })),
                    ApplyToError::from_json(&json!({
                        "message": "Property .y not found in null",
                        "path": ["arrayOfArrays", 4, 3, "y"],
                    })),
                ],
            ),
        );

        let array_of_arrays_x_y_expected = (
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
                    "message": "Property .y not found in null",
                    "path": ["arrayOfArrays", 4, 0, "y"],
                })),
                ApplyToError::from_json(&json!({
                    "message": "Property .y not found in object",
                    "path": ["arrayOfArrays", 4, 2, "y"],
                })),
                ApplyToError::from_json(&json!({
                    // Reversing the order of "path" and "message" here to make
                    // sure that doesn't affect the deduplication logic.
                    "path": ["arrayOfArrays", 4, 3, "y"],
                    "message": "Property .y not found in null",
                })),
                ApplyToError::from_json(&json!({
                    "message": "Property .x not found in null",
                    "path": ["arrayOfArrays", 4, 0, "x"],
                })),
                ApplyToError::from_json(&json!({
                    "message": "Property .x not found in null",
                    "path": ["arrayOfArrays", 4, 3, "x"],
                })),
            ],
        );
        assert_eq!(
            selection!("ys: .arrayOfArrays.y xs: .arrayOfArrays.x").apply_to(&data),
            array_of_arrays_x_y_expected,
        );
        assert_eq!(
            selection!("ys: $.arrayOfArrays.y xs: $.arrayOfArrays.x").apply_to(&data),
            array_of_arrays_x_y_expected,
        );
    }

    #[test]
    fn test_apply_to_variable_expressions() {
        let id_object = selection!("id: $").apply_to(&json!(123));
        assert_eq!(id_object, (Some(json!({"id": 123})), vec![]));

        let data = json!({
            "id": 123,
            "name": "Ben",
            "friend_ids": [234, 345, 456]
        });

        assert_eq!(
            selection!("id name friends: friend_ids { id: $ }").apply_to(&data),
            (
                Some(json!({
                    "id": 123,
                    "name": "Ben",
                    "friends": [
                        { "id": 234 },
                        { "id": 345 },
                        { "id": 456 },
                    ],
                })),
                vec![],
            ),
        );

        let mut vars = IndexMap::default();
        vars.insert("$args".to_string(), json!({ "id": "id from args" }));
        assert_eq!(
            selection!("id: $args.id name").apply_with_vars(&data, &vars),
            (
                Some(json!({
                    "id": "id from args",
                    "name": "Ben"
                })),
                vec![],
            ),
        );
        assert_eq!(
            selection!("nested.path { id: $args.id name }").apply_to(&json!({
                "nested": {
                    "path": data.clone(),
                },
            })),
            (
                Some(json!({
                    "name": "Ben"
                })),
                vec![ApplyToError::from_json(&json!({
                    "message": "Variable $args not found",
                    "path": ["nested", "path"],
                }))],
            ),
        );
        let mut vars_without_args_id = IndexMap::default();
        vars_without_args_id.insert("$args".to_string(), json!({ "unused": "ignored" }));
        assert_eq!(
            selection!("id: $args.id name").apply_with_vars(&data, &vars_without_args_id),
            (
                Some(json!({
                    "name": "Ben"
                })),
                vec![ApplyToError::from_json(&json!({
                    "message": "Property .id not found in object",
                    "path": ["$args", "id"],
                }))],
            ),
        );

        // A single variable path should not be mapped over an input array.
        assert_eq!(
            selection!("$args.id").apply_with_vars(&json!([1, 2, 3]), &vars),
            (Some(json!("id from args")), vec![]),
        );
    }

    #[test]
    fn test_apply_to_variable_expressions_typename() {
        let typename_object =
            selection!("__typename: $->echo('Product') reviews { __typename: $->echo('Review') }")
                .apply_to(&json!({"reviews": [{}]}));
        assert_eq!(
            typename_object,
            (
                Some(json!({"__typename": "Product", "reviews": [{ "__typename": "Review" }] })),
                vec![]
            )
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
}
