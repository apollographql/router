use apollo_compiler::ast::NamedType;
use apollo_compiler::ast::Type;
use apollo_compiler::Schema;

use crate::error::FederationError;
use crate::error::SingleFederationError;

fn is_composite_type(ty: &NamedType, schema: &Schema) -> Result<bool, FederationError> {
    Ok(matches!(
        schema
            .types
            .get(ty)
            .ok_or_else(|| SingleFederationError::Internal {
                message: format!("Cannot find type `'{}\'", ty),
            })?,
        apollo_compiler::schema::ExtendedType::Object(_)
            | apollo_compiler::schema::ExtendedType::Interface(_)
            | apollo_compiler::schema::ExtendedType::Union(_)
    ))
}

/**
 * This essentially follows the beginning of https://spec.graphql.org/draft/#SameResponseShape().
 * That is, the types cannot be merged unless:
 * - they have the same nullability and "list-ability", potentially recursively.
 * - their base type is either both composite, or are the same type.
 */
pub(crate) fn types_can_be_merged(
    t1: &Type,
    t2: &Type,
    schema: &Schema,
) -> Result<bool, FederationError> {
    let (n1, n2) = match (t1, t2) {
        (Type::Named(n1), Type::Named(n2)) | (Type::NonNullNamed(n1), Type::NonNullNamed(n2)) => {
            (n1, n2)
        }
        (Type::List(inner1), Type::List(inner2))
        | (Type::NonNullList(inner1), Type::NonNullList(inner2)) => {
            return types_can_be_merged(inner1, inner2, schema)
        }
        _ => return Ok(false),
    };
    if is_composite_type(n1, schema)? {
        return is_composite_type(n2, schema);
    }

    Ok(n1 == n2)
}
