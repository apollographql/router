#![allow(dead_code)]

use std::fs::File;
use std::io::Read;
use crate::link::spec::Version;

fn get_federation_definition(version: Version) -> Vec<String> {
    // TODO: the logic isn't done here, this is just a placeholder for now
    match (version.major, version.minor) {
        (1, 0) => vec![load_definition_from_file("fed1_0")],
        (2, 0) => vec![load_definition_from_file("fed2_0")],
        (2, 1) => vec![load_definition_from_file("fed2_1")],
        (2, 2) => vec![load_definition_from_file("fed2_2")],
        (2, 3) => vec![load_definition_from_file("fed2_3")],
        (2, 4) => vec![load_definition_from_file("fed2_4")],
        (2, 5) => vec![load_definition_from_file("fed2_5")],
        (2, 6) => vec![load_definition_from_file("fed2_6")],
        (2, 7) => vec![load_definition_from_file("fed2_7")],
        (2, 8) => vec![load_definition_from_file("fed2_8")],
        (2, 9) => vec![load_definition_from_file("fed2_9")],
        _ => vec![],
    }
}

// should this be stored in lazylock object instead? that might be more efficient
// how can i handle errors here? or is panic the only option?
fn load_definition_from_file(version: &str) -> String {
    let file_path = format!("definitions/{}.graphqls", version);
    let mut s = String::new();
    let mut file = File::open(&file_path).unwrap_or_else(|e| {
        panic!("Failed to load definition for version '{}' from file path '{}': {}", version, &file_path, e)
    });
    file.read_to_string(&mut s).unwrap_or_else(|e| {
        panic!("Failed to read definition for version '{}' from file path '{}': {}", version, &file_path, e)
    });
    s
}

#[test]
fn test_load_definition_from_file() {
    let definition = load_definition_from_file("fed2_0");
    assert!(!definition.is_empty());
    assert_eq!(&definition, EXPECTED_FED2_0_DEFINITION);
}

#[test]
fn test_get_federation_definition() {
    let definition = get_federation_definition(Version{major: 2, minor: 0});
    assert!(!definition.is_empty());
    assert_eq!(&definition[0], EXPECTED_FED2_0_DEFINITION);
}

const EXPECTED_FED2_0_DEFINITION: &str = r#"#
# https://specs.apollo.dev/federation/v2.0/federation-v2.0.graphql
#

directive @key(fields: FieldSet!, resolvable: Boolean = true) repeatable on OBJECT | INTERFACE
directive @requires(fields: FieldSet!) on FIELD_DEFINITION
directive @provides(fields: FieldSet!) on FIELD_DEFINITION
directive @external on OBJECT | FIELD_DEFINITION
directive @shareable on FIELD_DEFINITION | OBJECT
directive @extends on OBJECT | INTERFACE
directive @override(from: String!) on FIELD_DEFINITION
directive @inaccessible on
    | FIELD_DEFINITION
    | OBJECT
    | INTERFACE
    | UNION
    | ENUM
    | ENUM_VALUE
    | SCALAR
    | INPUT_OBJECT
    | INPUT_FIELD_DEFINITION
    | ARGUMENT_DEFINITION
directive @tag(name: String!) repeatable on
    | FIELD_DEFINITION
    | INTERFACE
    | OBJECT
    | UNION
    | ARGUMENT_DEFINITION
    | SCALAR
    | ENUM
    | ENUM_VALUE
    | INPUT_OBJECT
    | INPUT_FIELD_DEFINITION
scalar FieldSet

#
# https://specs.apollo.dev/link/v1.0/link-v1.0.graphql
#

directive @link(
    url: String!,
    as: String,
    import: [Import],
    for: Purpose)
repeatable on SCHEMA

scalar Import

enum Purpose {
  SECURITY
  EXECUTION
}
"#;