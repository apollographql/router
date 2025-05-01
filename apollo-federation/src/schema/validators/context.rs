use apollo_compiler::Name;
use apollo_compiler::Schema;
use apollo_compiler::ast::DirectiveList;
use apollo_compiler::executable::Field;
use itertools::Itertools;

use crate::error::FederationError;
use crate::error::MultipleFederationErrors;
use crate::error::SingleFederationError;
use crate::schema::FederationSchema;
use crate::schema::HasFields;
use crate::schema::validators::DenyFieldsWithDirectiveApplications;
use crate::schema::validators::SchemaFieldSetValidator;

pub(crate) fn validate_context_directives(
    schema: &FederationSchema,
    errors: &mut MultipleFederationErrors,
) -> Result<(), FederationError> {
  
    for context_directive in schema.context_directive_applications()? {
        match context_directive {
            Ok(context) => {
              let name = context.arguments.name;
            },
            Err(e) => errors.push(e),
        }
    }
  Ok(())
}