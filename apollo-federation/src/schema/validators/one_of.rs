use apollo_compiler::schema::ExtendedType;

use crate::error::CompositionError;
use crate::subgraph::typestate::HasMetadata;
use crate::subgraph::typestate::Subgraph;
use crate::utils::human_readable::human_readable_subgraph_names;

/// Validates that all subgraphs defining the same input object type agree on
/// whether `@oneOf` is applied. Disagreement would produce an invalid
/// supergraph, so we reject it before merging.
pub(crate) fn validate_one_of_consistency<T: HasMetadata>(
    subgraphs: &[Subgraph<T>],
) -> Result<(), Vec<CompositionError>> {
    let mut errors = Vec::new();

    // Collect all input object type names across subgraphs.
    let mut input_type_names = std::collections::BTreeSet::new();
    for subgraph in subgraphs {
        for (name, ty) in &subgraph.schema().schema().types {
            if matches!(ty, ExtendedType::InputObject(_)) {
                input_type_names.insert(name.clone());
            }
        }
    }

    for type_name in &input_type_names {
        let mut with_one_of: Vec<&str> = Vec::new();
        let mut without_one_of: Vec<&str> = Vec::new();

        for subgraph in subgraphs {
            if let Some(ExtendedType::InputObject(input_obj)) =
                subgraph.schema().schema().types.get(type_name)
            {
                if input_obj.directives.has("oneOf") {
                    with_one_of.push(&subgraph.name);
                } else {
                    without_one_of.push(&subgraph.name);
                }
            }
        }

        if !with_one_of.is_empty() && !without_one_of.is_empty() {
            let with_str = human_readable_subgraph_names(with_one_of.iter());
            let without_str = human_readable_subgraph_names(without_one_of.iter());
            errors.push(CompositionError::InputObjectOneOfMismatch {
                message: format!(
                    "Input object type \"{type_name}\" is marked with @oneOf in {with_str} but not in {without_str}",
                ),
                locations: Vec::new(),
            });
        }
    }

    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors)
    }
}
