// The fuzzer won't compile on windows as of 1.63.0
#![cfg(not(windows))]
use std::convert::TryFrom;
use std::fs;

use apollo_parser::Parser;
use apollo_smith::Document;
use apollo_smith::DocumentBuilder;
use libfuzzer_sys::arbitrary::Result;
use libfuzzer_sys::arbitrary::Unstructured;
use log::debug;

/// This generate an arbitrary valid GraphQL operation
pub fn generate_valid_operation(
    input: &[u8],
    schema_path: &'static str,
) -> Result<(String, String)> {
    drop(env_logger::try_init());

    let contents = fs::read_to_string(schema_path).expect("cannot read file");
    let parser = Parser::new(&contents);

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
    let operation_def: String = gql_doc.operation_definition()?.unwrap().into();
    let doc: String = gql_doc.finish().into();

    Ok((operation_def, doc))
}
