//! The connectors counterpart to [`crate::schema::blueprint::FederationBlueprint`].
//!
//! Connectors are their own spec — separately versioned, separately registered in the spec registry
//! — so their validations live here rather than alongside the federation ones. Notably,
//! `FederationBlueprint::on_validation` returns early for schemas that aren't fed2, a guard that
//! makes sense for federation directives and would be arbitrary for connectors.

use crate::composition::CompositionFailure;
use crate::connectors::validation::Severity;
use crate::connectors::validation::validate;
use crate::error::CompositionError;
use crate::error::SubgraphLocation;
use crate::merger::hints::HintCode;
use crate::subgraph::typestate::HasMetadata;
use crate::subgraph::typestate::Subgraph;
use crate::supergraph::CompositionHint;

pub(crate) struct ConnectorsBlueprint {}

impl ConnectorsBlueprint {
    /// Validates the `@source` and `@connect` directives.
    ///
    /// # Where this runs
    /// This is called on an *expanded but not yet GraphQL-validated* schema, which is deliberate on
    /// both counts:
    ///
    /// - **After expansion**, so the connect and federation directive definitions are present and
    ///   the link metadata has been collected. Directive names resolve through their `@link`
    ///   imports, so renames and aliases are handled.
    /// - **Before GraphQL validation**, so a subgraph that is both connector-invalid and
    ///   GraphQL-invalid still reports its connector diagnostics. Those are the more actionable ones
    ///   for a subgraph author — `@source` not being imported reads much better as "add `@source` to
    ///   `import`" than as "cannot find directive `@source`".
    ///
    /// # Return value
    /// `Ok` carries the warnings to report as hints. `Err` carries the errors *and* those same
    /// warnings: connectors' one warning today, `NO_SOURCE_IMPORT`, is only raised alongside an
    /// error, and it happens to be the message that says how to fix it — so dropping warnings on
    /// the error path would lose the most useful diagnostic.
    #[allow(private_bounds)]
    pub(crate) fn on_validation<S: HasMetadata>(
        subgraph: &Subgraph<S>,
    ) -> Result<Vec<CompositionHint>, CompositionFailure> {
        let mut errors = vec![];
        let mut hints = vec![];

        for message in validate(subgraph) {
            let locations = message
                .locations
                .into_iter()
                .map(|range| SubgraphLocation {
                    subgraph: subgraph.name.clone(),
                    range,
                })
                .collect();
            match message.code.severity() {
                Severity::Error => errors.push(CompositionError::ConnectorsValidationError {
                    code: message.code,
                    message: message.message,
                    locations,
                }),
                Severity::Warning => hints.push(CompositionHint {
                    definition: HintCode::ConnectorsHint(message.code).definition(),
                    message: message.message,
                    locations,
                }),
            }
        }

        if errors.is_empty() {
            Ok(hints)
        } else {
            Err(CompositionFailure { errors, hints })
        }
    }
}
