use apollo_compiler::parser::SourceSpan;

/// Returned as an error for situations that should not happen with a valid schema or document.
///
/// Since the relevant APIs take [`Valid<_>`][crate::validation::Valid] parameters,
/// either apollo-compiler has a validation bug
/// or [`assume_valid`][crate::validation::Valid::assume_valid] was used incorrectly.
///
/// Can be [converted][std::convert] to [`GraphQLError`],
/// which populates [`extensions`][GraphQLError::extensions]
/// with a `"APOLLO_SUSPECTED_VALIDATION_BUG": true` entry.
#[derive(Debug, Clone)]
pub(crate) struct SuspectedValidationBug {
    pub message: String,
    pub location: Option<SourceSpan>,
}
