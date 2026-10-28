//! Running one decoded case through the three implementations.

use apollo_compiler::validation::Valid;
use apollo_compiler::ExecutableDocument;
use apollo_compiler::Schema;
use apollo_federation::correctness::compare_operations;
use apollo_federation::correctness::query_compare;
use apollo_federation::correctness::query_compare::Mismatch;
use apollo_federation::schema::ValidFederationSchema;

use crate::model;

/// Set to invert every `query_compare` verdict; see [`Harness::run`].
pub const SENTINEL_ENV: &str = "QUERY_INCLUSION_SENTINEL";

/// A case whose two operations parsed and validated against the shared schema.
pub struct PreparedCase {
    pub left_source: String,
    pub right_source: String,
    pub left: Valid<ExecutableDocument>,
    pub right: Valid<ExecutableDocument>,
}

/// How one case was answered by every lane, in one direction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Verdicts {
    /// `query_compare::includes` — the port under test.
    pub query_compare: bool,
    /// `compare_operations` — the independent response-shape algorithm.
    pub response_shape: bool,
    /// Set when `query_compare` rejected the input rather than deciding inclusion.
    pub out_of_scope: bool,
    /// Which failure the port reported, if it rejected. Tallying these is how the campaign shows
    /// which decision paths it actually reaches: a `Mismatch` variant that never appears is a
    /// variant the differential lane has never tested.
    pub reason: Option<&'static str>,
}

/// The name of a mismatch variant. Exhaustive on purpose — a new variant will not compile until
/// it is named here, which keeps the coverage tally from silently going stale.
pub fn reason_name(reason: &Mismatch) -> &'static str {
    match reason {
        Mismatch::RootTypeMismatch { .. } => "RootTypeMismatch",
        Mismatch::VariableDeclarationMismatch { .. } => "VariableDeclarationMismatch",
        Mismatch::MissingResponseName { .. } => "MissingResponseName",
        Mismatch::FieldNameMismatch { .. } => "FieldNameMismatch",
        Mismatch::FieldArgumentsMismatch { .. } => "FieldArgumentsMismatch",
        Mismatch::UndefinedField { .. } => "UndefinedField",
        Mismatch::FieldDirectivesMismatch { .. } => "FieldDirectivesMismatch",
        Mismatch::UndefinedFragment { .. } => "UndefinedFragment",
        Mismatch::Internal { .. } => "Internal",
    }
}

pub struct Harness {
    schema: ValidFederationSchema,
}

impl Harness {
    pub fn new() -> Self {
        let schema = Schema::parse_and_validate(model::SCHEMA_SDL, "schema.graphql")
            .expect("the shared schema parses and validates");
        Harness {
            schema: ValidFederationSchema::new(schema).expect("the shared schema is federated"),
        }
    }

    pub fn schema(&self) -> &ValidFederationSchema {
        &self.schema
    }

    /// Parses and validates a decoded case.
    ///
    /// Returns `None` when the grammar produced a document GraphQL rejects. The grammar avoids
    /// the common causes — a response name is bound to one resolver call per operation, and only
    /// overlapping type conditions are offered — but field-merge rules across covariant scopes
    /// can still be violated, and an invalid document is not a case either implementation is
    /// meant to answer. The runner reports the discard rate so this stays visible.
    pub fn prepare(&self, case: &model::Case) -> Option<PreparedCase> {
        self.prepare_explained(case).ok()
    }

    /// `prepare`, keeping the validation error. Campaigns report the discard rate, and a rate that
    /// drifts upward is usually one grammar rule producing documents GraphQL rejects — far easier
    /// to find with the message than by rereading the generator.
    pub fn prepare_explained(&self, case: &model::Case) -> Result<PreparedCase, String> {
        let left = ExecutableDocument::parse_and_validate(
            self.schema.schema(),
            &case.left,
            "left.graphql",
        )
        .map_err(|error| error.to_string())?;
        let right = ExecutableDocument::parse_and_validate(
            self.schema.schema(),
            &case.right,
            "right.graphql",
        )
        .map_err(|error| error.to_string())?;
        Ok(PreparedCase {
            left_source: case.left.clone(),
            right_source: case.right.clone(),
            left,
            right,
        })
    }

    /// Does `left` include `right`, according to each Rust implementation?
    pub fn run(&self, case: &PreparedCase, reversed: bool) -> Verdicts {
        let (left, right) = if reversed {
            (&case.right, &case.left)
        } else {
            (&case.left, &case.right)
        };
        let query_compare_result = query_compare::includes(&self.schema, left, right);
        let out_of_scope = query_compare_result
            .as_ref()
            .err()
            .is_some_and(|error| !error.is_inclusion_finding());
        let reason = query_compare_result
            .as_ref()
            .err()
            .map(|error| reason_name(error.reason()));
        // A deliberately corrupted verdict, used by the sentinel run to prove that a
        // disagreement actually travels through the decoder, the oracle transport, and the
        // comparison. Without it, "no mismatches" cannot be told apart from "no detection".
        let mutate = std::env::var_os(SENTINEL_ENV).is_some() && !case.left_source.is_empty();
        Verdicts {
            query_compare: query_compare_result.is_ok() ^ mutate,
            // Note the reversed arguments. `compare_operations(this, other)` asks whether `this`
            // is a *subset* of `other`, which is the opposite direction from
            // `includes(left, right)`. Passing the same order to both compares two different
            // questions and makes every asymmetric case look like a divergence.
            response_shape: compare_operations(&self.schema, right, left).is_ok(),
            out_of_scope,
            reason,
        }
    }

    /// The rejection explanation, for diagnosing a disagreement.
    pub fn explain(&self, case: &PreparedCase, reversed: bool) -> Option<String> {
        let (left, right) = if reversed {
            (&case.right, &case.left)
        } else {
            (&case.left, &case.right)
        };
        query_compare::includes(&self.schema, left, right)
            .err()
            .map(|error| error.to_string())
    }
}

impl Default for Harness {
    fn default() -> Self {
        Self::new()
    }
}
