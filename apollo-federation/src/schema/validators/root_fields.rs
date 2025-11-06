use apollo_compiler::ast::OperationType;

use crate::error::CompositionError;
use crate::subgraph::typestate::HasMetadata;
use crate::subgraph::typestate::Subgraph;

/// We rename root types to their default names prior to merging, so we should never get an error
/// from this validation.
///
/// See [`crate::subgraph::typestate::Subgraph<Upgraded>::normalize_root_types()`].
pub(crate) fn validate_consistent_root_fields<T: HasMetadata>(
    subgraphs: &[Subgraph<T>],
) -> Result<(), Vec<CompositionError>> {
    if subgraphs.is_empty() {
        return Ok(());
    }

    let mut errors = Vec::with_capacity(3);
    if !is_operation_name_consistent(OperationType::Mutation, subgraphs) {
        errors.push(CompositionError::InternalError {
            message: "Should not have incompatible root type for Mutation".to_string(),
        });
    }
    if !is_operation_name_consistent(OperationType::Query, subgraphs) {
        errors.push(CompositionError::InternalError {
            message: "Should not have incompatible root type for Query".to_string(),
        });
    }
    if !is_operation_name_consistent(OperationType::Subscription, subgraphs) {
        errors.push(CompositionError::InternalError {
            message: "Should not have incompatible root type for Subscription".to_string(),
        });
    }

    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors)
    }
}

fn is_operation_name_consistent<T: HasMetadata>(
    operation_type: OperationType,
    subgraphs: &[Subgraph<T>],
) -> bool {
    let mut operation_name = None;

    for subgraph in subgraphs {
        if let Some(name) = subgraph.schema().schema().root_operation(operation_type) {
            if let Some(existing_name) = operation_name {
                if existing_name != name {
                    return false;
                }
            } else {
                operation_name = Some(name);
            }
        }
    }

    true
}

#[cfg(test)]
mod tests {
    use itertools::Itertools;

    use super::*;
    use crate::subgraph::typestate::Subgraph;

    #[test]
    fn reports_inconsistent_root_field_types() {
        let s1 = Subgraph::parse(
            "s1",
            "",
            r#"
            type Mutation {
                data: String
            }

            type Query {
                data: String
            }

            type Subscription {
                data: String
            }
        "#,
        )
        .unwrap()
        .assume_expanded()
        .unwrap()
        .assume_validated();

        let s2 = Subgraph::parse(
            "s2",
            "",
            r#"
            extend schema {
                mutation: MyMutation
                query: MyQuery
                subscription: MySubscription
            }

            type MyMutation {
                data: String
            }

            type MyQuery {
                data: String
            }

            type MySubscription {
                data: String
            }
        "#,
        )
        .unwrap()
        .assume_expanded()
        .unwrap()
        .assume_validated();

        let res = validate_consistent_root_fields(&[s1, s2]).unwrap_err();
        let errors = res.iter().map(|e| e.to_string()).collect_vec();
        assert_eq!(
            errors,
            vec![
                "Should not have incompatible root type for Mutation".to_string(),
                "Should not have incompatible root type for Query".to_string(),
                "Should not have incompatible root type for Subscription".to_string()
            ]
        );
    }
}
