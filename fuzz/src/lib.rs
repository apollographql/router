// The fuzzer won't compile on windows as of 1.63.0
#![cfg(not(windows))]
use std::convert::TryFrom;
use std::fs;

use apollo_parser::Parser;
use apollo_smith::Document;
use apollo_smith::DocumentBuilder;
use libfuzzer_sys::arbitrary::Error;
use libfuzzer_sys::arbitrary::Result;
use libfuzzer_sys::arbitrary::Unstructured;
use log::debug;

/// Generates an arbitrary valid GraphQL operation against the schema at `schema_path`.
pub fn generate_valid_operation(
    input: &[u8],
    schema_path: &'static str,
) -> Result<(String, String)> {
    let contents = fs::read_to_string(schema_path).expect("cannot read file");
    generate_valid_operation_from_schema(input, &contents)
}

/// Generates an arbitrary valid GraphQL operation against the given schema SDL string.
///
/// apollo-smith only emits directive applications for directives declared in the schema
/// it sees, so callers that want to fuzz `@defer` (or any other directive) must hand in
/// a schema string that declares it.
pub fn generate_valid_operation_from_schema(
    input: &[u8],
    schema_sdl: &str,
) -> Result<(String, String)> {
    drop(env_logger::try_init());

    let parser = Parser::new(schema_sdl);

    let tree = parser.parse();
    if tree.errors().len() > 0 {
        let errors = tree
            .errors()
            .map(|err| err.message())
            .collect::<Vec<&str>>()
            .join("\n");
        debug!("parser errors ========== \n{:?}", errors);
        debug!("========================");
        panic!("cannot parse the supergraph");
    }

    let mut u = Unstructured::new(input);
    let mut gql_doc = DocumentBuilder::with_document(
        &mut u,
        Document::try_from(tree.document()).expect("tree should not have errors"),
    )?;
    // apollo-smith may return `Ok(None)` when the input bytes don't yield a usable
    // operation (e.g. empty input). Treat that as a recoverable arbitrary error so
    // libfuzzer skips the iteration instead of crashing.
    let operation_def: String = gql_doc
        .operation_definition()?
        .ok_or(Error::NotEnoughData)?
        .into();
    let doc: String = gql_doc.finish().into();

    Ok((operation_def, doc))
}
