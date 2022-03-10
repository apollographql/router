use std::fs;

use apollo_parser::Parser;
use apollo_smith::{Document, DocumentBuilder};
use libfuzzer_sys::arbitrary::{Result, Unstructured};
use log::debug;

/// This generate an arbitrary valid GraphQL operation
pub fn generate_valid_operation(input: &[u8]) -> Result<String> {
    drop(env_logger::try_init());

    let parser =
        Parser::new(&fs::read_to_string("fuzz/supergraph.graphql").expect("cannot read file"));

    let tree = parser.parse();
    if tree.errors().len() > 0 {
        let errors = tree
            .errors()
            .map(|err| err.message())
            .collect::<Vec<&str>>()
            .join("\n");
        debug!("Parser errors ========== \n{:?}", errors);
        debug!("========================");
        panic!("cannot parse the supergraph");
    }

    let mut u = Unstructured::new(input);
    let mut gql_doc = DocumentBuilder::with_document(&mut u, Document::from(tree.document()))?;
    let operation_def = gql_doc.operation_definition()?.unwrap();

    Ok(operation_def.into())
}
