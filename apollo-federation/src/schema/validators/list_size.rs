use apollo_compiler::schema::ExtendedType;
use apollo_compiler::ty;

use crate::error::FederationError;
use crate::error::MultipleFederationErrors;
use crate::error::SingleFederationError;
use crate::schema::FederationSchema;
use crate::schema::ListSizeDirective;

pub(crate) fn validate_list_size_directives(
    schema: &FederationSchema,
    errors: &mut MultipleFederationErrors,
) -> Result<(), FederationError> {
    for list_size_directive in schema.list_size_directive_applications()? {
        match list_size_directive {
            Ok(list_size) => {
                validate_applied_to_list(&list_size, errors);
                validate_assumed_size_not_negative(&list_size, errors);
                validate_slicing_arguments_are_valid_integers(&list_size, errors);
                validate_sized_fields_are_valid_lists(schema, &list_size, errors);
            }
            Err(e) => errors.push(e),
        }
    }
    Ok(())
}

/// Validate that `@listSize` is only applied to lists per
/// https://ibm.github.io/graphql-specs/cost-spec.html#sec-Valid-List-Size-Target
fn validate_applied_to_list(list_size: &ListSizeDirective, errors: &mut MultipleFederationErrors) {
    let has_sized_fields = list_size
        .directive
        .sized_fields
        .as_ref()
        .is_some_and(|s| !s.is_empty());
    if !has_sized_fields && !list_size.target.ty.is_list() {
        errors
            .errors
            .push(SingleFederationError::ListSizeAppliedToNonList {
                message: format!(
                    "\"{}.{}\" is not a list",
                    list_size.parent_type, list_size.target.name
                ),
            });
    }
}

/// Validate assumed size, but we differ from https://ibm.github.io/graphql-specs/cost-spec.html#sec-Valid-Assumed-Size.
/// Assumed size is used as a backup for slicing arguments in the event they are both specified.
/// The spec aims to rule out cases when the assumed size will never be used because there is always
/// a slicing argument. Two applications which are compliant with that validation rule can be merged
/// into an application which is not compliant, thus we need to handle this case gracefully at runtime regardless.
/// We omit this check to keep the validations to those that will otherwise cause runtime failures.
///
/// With all that said, assumed size should not be negative.
fn validate_assumed_size_not_negative(
    list_size: &ListSizeDirective,
    errors: &mut MultipleFederationErrors,
) {
    if let Some(size) = list_size.directive.assumed_size
        && size < 0
    {
        errors
            .errors
            .push(SingleFederationError::ListSizeInvalidAssumedSize {
                message: format!(
                    "Assumed size of \"{}.{}\" cannot be negative",
                    list_size.parent_type, list_size.target.name
                ),
            });
    }
}

/// Validate `slicingArguments` select valid integer arguments on the target type per
/// https://ibm.github.io/graphql-specs/cost-spec.html#sec-Valid-Slicing-Arguments-Target
fn validate_slicing_arguments_are_valid_integers(
    list_size: &ListSizeDirective,
    errors: &mut MultipleFederationErrors,
) {
    let Some(slicing_argument_names) = list_size.directive.slicing_argument_names.as_ref() else {
        return;
    };
    for arg_name in slicing_argument_names {
        if let Some(slicing_argument) = list_size.target.argument_by_name(arg_name.as_str()) {
            if *slicing_argument.ty != ty!(Int) && *slicing_argument.ty != ty!(Int!) {
                errors
                    .errors
                    .push(SingleFederationError::ListSizeInvalidSlicingArgument {
                        message: format!(
                            "Slicing argument \"{}.{}({}:)\" must be Int or Int!",
                            list_size.parent_type, list_size.target.name, arg_name,
                        ),
                    });
            }
        } else {
            errors
                .errors
                .push(SingleFederationError::ListSizeInvalidSlicingArgument {
                    message: format!(
                        "Slicing argument \"{arg_name}\" is not an argument of \"{}.{}\"",
                        list_size.parent_type, list_size.target.name
                    ),
                });
        }
    }
}

/// Validate `sizedFields` select valid list fields on the target type per
/// https://ibm.github.io/graphql-specs/cost-spec.html#sec-Valid-Sized-Fields-Target
fn validate_sized_fields_are_valid_lists(
    schema: &FederationSchema,
    list_size: &ListSizeDirective,
    errors: &mut MultipleFederationErrors,
) {
    let Some(sized_field_names) = list_size.directive.sized_fields.as_ref() else {
        return;
    };
    let target_type = list_size.target.ty.inner_named_type();
    let fields = match schema.schema().types.get(target_type) {
        Some(ExtendedType::Object(obj)) => &obj.fields,
        Some(ExtendedType::Interface(itf)) => &itf.fields,
        _ => {
            errors
            .errors
            .push(SingleFederationError::ListSizeInvalidSizedField {
                message: format!(
                    "Sized fields cannot be used because \"{target_type}\" is not a composite type"
                ),
            });
            return;
        }
    };
    for field_name in sized_field_names {
        if let Some(field) = fields.get(field_name.as_str()) {
            if !field.ty.is_list() {
                errors
                    .errors
                    .push(SingleFederationError::ListSizeAppliedToNonList {
                        message: format!(
                            "Sized field \"{target_type}.{field_name}\" is not a list"
                        ),
                    });
            }
        } else {
            errors
                .errors
                .push(SingleFederationError::ListSizeInvalidSizedField {
                    message: format!(
                        "Sized field \"{field_name}\" is not a field on type \"{target_type}\""
                    ),
                })
        }
    }
}
