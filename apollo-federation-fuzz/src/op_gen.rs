//! Layer 3: generate valid operations against an api-schema string using
//! `apollo-smith`, then validate each via `apollo-compiler`.
//!
//! Mirrors the existing pattern at `fuzz/src/lib.rs:14-44` and
//! `apollo-router-benchmarks/build.rs:11-32` so we get consistent behavior
//! with the rest of the workspace.

use apollo_compiler::ExecutableDocument;
use apollo_compiler::validation::Valid;
use apollo_parser::Parser;
use apollo_smith::{Document, DocumentBuilder};
use arbitrary::Unstructured;

#[derive(Debug)]
pub enum OpGenError {
    SchemaParse(String),
    SchemaValidate(String),
    Generator(String),
    Validate(String),
}

impl std::fmt::Display for OpGenError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::SchemaParse(s) => write!(f, "schema parse: {s}"),
            Self::SchemaValidate(s) => write!(f, "schema validate: {s}"),
            Self::Generator(s) => write!(f, "smith generator: {s}"),
            Self::Validate(s) => write!(f, "operation validate: {s}"),
        }
    }
}

impl std::error::Error for OpGenError {}

/// Generate one operation document from the bytes in `seed`. Returns the
/// validated operation as a string. Discards (returns `Err`) when smith
/// produces no operation, or when the operation fails validation.
pub fn generate_operation(api_schema_sdl: &str, seed: &[u8]) -> Result<String, OpGenError> {
    let parsed = Parser::new(api_schema_sdl).parse();
    if parsed.errors().next().is_some() {
        let detail = parsed
            .errors()
            .map(|e| e.message().to_string())
            .collect::<Vec<_>>()
            .join("; ");
        return Err(OpGenError::SchemaParse(detail));
    }
    let smith_doc: Document = parsed
        .document()
        .try_into()
        .map_err(|e: apollo_smith::FromError| OpGenError::SchemaParse(e.to_string()))?;

    let mut u = Unstructured::new(seed);
    let mut builder = DocumentBuilder::with_document(&mut u, smith_doc)
        .map_err(|e| OpGenError::Generator(e.to_string()))?;
    let op = builder
        .operation_definition()
        .map_err(|e| OpGenError::Generator(e.to_string()))?
        .ok_or_else(|| OpGenError::Generator("no operation produced".to_string()))?;
    let op_text: String = op.into();

    // Validate against the api schema. We re-parse the schema here because
    // the planner adapters do their own parse anyway; this is the cheap
    // pre-flight check that keeps obvious garbage out of the diff loop.
    let valid_schema =
        apollo_compiler::Schema::parse_and_validate(api_schema_sdl, "api.graphql")
            .map_err(|e| OpGenError::SchemaValidate(e.to_string()))?;
    let _doc: Valid<ExecutableDocument> =
        ExecutableDocument::parse_and_validate(&valid_schema, &op_text, "operation.graphql")
            .map_err(|e| OpGenError::Validate(e.to_string()))?;

    Ok(op_text)
}
