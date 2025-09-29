pub(crate) mod suggestion;

use std::cmp::Ordering;
use std::fmt::Display;
use std::fmt::Formatter;
use std::fmt::Write;
use std::ops::Range;
use std::sync::LazyLock;

use apollo_compiler::InvalidNameError;
use apollo_compiler::Name;
use apollo_compiler::ast::OperationType;
use apollo_compiler::parser::LineColumn;
use apollo_compiler::validation::DiagnosticList;
use apollo_compiler::validation::WithErrors;

use crate::subgraph::SubgraphError;
use crate::subgraph::spec::FederationSpecError;
use crate::subgraph::typestate::HasMetadata;
use crate::subgraph::typestate::Subgraph;

/// Create an internal error.
///
/// # Example
/// ```rust
/// use apollo_federation::internal_error;
/// use apollo_federation::error::FederationError;
/// # fn may_be_none() -> Option<()> { None }
///
/// const NAME: &str = "the thing";
/// let result: Result<(), FederationError> = may_be_none()
///     .ok_or_else(|| internal_error!("Expected {NAME} to be Some"));
/// ```
#[macro_export]
macro_rules! internal_error {
    ( $( $arg:tt )+ ) => {
        $crate::error::FederationError::internal(format!( $( $arg )+ ))
    }
}

/// Break out of the current function, returning an internal error.
///
/// # Example
/// ```rust
/// use apollo_federation::bail;
/// use apollo_federation::error::FederationError;
/// # fn may_be_none() -> Option<()> { None }
///
/// fn example() -> Result<(), FederationError> {
///     bail!("Something went horribly wrong");
///     unreachable!()
/// }
/// #
/// # _ = example();
/// ```
#[macro_export]
macro_rules! bail {
    ( $( $arg:tt )+ ) => {
        return Err($crate::internal_error!( $( $arg )+ ).into())
    }
}

/// A safe assertion: in debug mode, it panicks on failure, and in production, it returns an
/// internal error.
///
/// Treat this as an assertion. It must only be used for conditions that *should never happen*
/// in normal operation.
///
/// # Example
/// ```rust,no_run
/// use apollo_federation::ensure;
/// use apollo_federation::error::FederationError;
/// # fn may_be_none() -> Option<()> { None }
///
/// fn example() -> Result<(), FederationError> {
///     ensure!(1 == 0, "Something went horribly wrong");
///     unreachable!()
/// }
/// ```
#[macro_export]
macro_rules! ensure {
    ( $expr:expr, $( $arg:tt )+ ) => {
        #[cfg(debug_assertions)]
        {
            if false {
                return Err($crate::error::FederationError::internal("ensure!() must be used in a function that returns a Result").into());
            }
            assert!($expr, $( $arg )+);
        }

        #[cfg(not(debug_assertions))]
        if !$expr {
            $crate::bail!( $( $arg )+ );
        }
    }
}

// What we really needed here was the string representations in enum form, this isn't meant to
// replace AST components.
#[derive(Clone, Debug, strum_macros::Display)]
enum SchemaRootKind {
    #[strum(to_string = "query")]
    Query,
    #[strum(to_string = "mutation")]
    Mutation,
    #[strum(to_string = "subscription")]
    Subscription,
}

impl From<SchemaRootKind> for String {
    fn from(value: SchemaRootKind) -> Self {
        value.to_string()
    }
}

#[derive(Clone, Debug, strum_macros::Display, PartialEq, Eq)]
pub enum UnsupportedFeatureKind {
    #[strum(to_string = "alias")]
    Alias,
}

/// Modeled after `SubgraphLocation` defined in `apollo_composition`, so this struct can be
/// converted to it.
#[derive(Clone, Debug)]
pub struct SubgraphLocation {
    /// Subgraph name
    pub subgraph: String, // TODO: Change this to `Arc<str>`, once `Merger` is updated.
    /// Source code range in the subgraph schema document
    pub range: Range<LineColumn>,
}

pub type Locations = Vec<SubgraphLocation>;

pub(crate) trait HasLocations {
    fn locations<T: HasMetadata>(&self, subgraph: &Subgraph<T>) -> Locations;
}

#[derive(Debug, Clone, thiserror::Error)]
pub enum CompositionError {
    #[error("[{subgraph}] {error}")]
    SubgraphError {
        subgraph: String,
        error: SingleFederationError,
        locations: Locations,
    },
    #[error("{message}")]
    ContextualArgumentNotContextualInAllSubgraphs {
        message: String,
        locations: Locations,
    },
    #[error("{message}")]
    EmptyMergedEnumType {
        message: String,
        locations: Locations,
    },
    #[error("{message}")]
    EnumValueMismatch { message: String },
    #[error("{message}")]
    ExternalArgumentTypeMismatch { message: String },
    #[error("{message}")]
    ExternalTypeMismatch { message: String },
    #[error("{message}")]
    ExternalArgumentDefaultMismatch { message: String },
    #[error("{message}")]
    InvalidGraphQL { message: String },
    #[error(transparent)]
    InvalidGraphQLName(InvalidNameError),
    #[error(r#"{message} in @fromContext substring "{context}""#)]
    FromContextParseError { context: String, message: String },
    #[error(
        "Unsupported custom directive @{name} on fragment spread. Due to query transformations during planning, the router requires directives on fragment spreads to support both the FRAGMENT_SPREAD and INLINE_FRAGMENT locations."
    )]
    UnsupportedSpreadDirective { name: Name },
    #[error("{message}")]
    DirectiveDefinitionInvalid { message: String },
    #[error("{message}")]
    TypeDefinitionInvalid { message: String },
    #[error("{message}")]
    InterfaceObjectUsageError { message: String },
    #[error("{message}")]
    InterfaceKeyMissingImplementationType { message: String },
    #[error("{message}")]
    TypeKindMismatch { message: String },
    #[error("{message}")]
    ShareableHasMismatchedRuntimeTypes { message: String },
    #[error("{message}")]
    SatisfiabilityError { message: String },
    #[error("{message}")]
    MaxValidationSubgraphPathsExceeded { message: String },
    #[error("{message}")]
    InternalError { message: String },
    #[error("{message}")]
    ExternalArgumentMissing { message: String },
    #[error("{message}")]
    ExternalMissingOnBase { message: String },
    #[error("{message}")]
    MergedDirectiveApplicationOnExternal { message: String },
    #[error("{message}")]
    LinkImportNameMismatch { message: String },
    #[error("{message}")]
    InvalidFieldSharing {
        message: String,
        locations: Locations,
    },
    #[error(
        "[{subgraph}] Type \"{dest}\" is an extension type, but there is no type definition for \"{dest}\" in any subgraph."
    )]
    ExtensionWithNoBase {
        subgraph: String,
        dest: String,
        locations: Locations,
    },
    #[error("{message}")]
    DirectiveCompositionError { message: String },
    #[error("{message}")]
    InconsistentInputObjectField { message: String },
    #[error("{message}")]
    RequiredArgumentMissingInSomeSubgraph {
        message: String,
        locations: Locations,
    },
    #[error("{message}")]
    RequiredInputFieldMissingInSomeSubgraph {
        message: String,
        locations: Locations,
    },
    #[error("{message}")]
    EmptyMergedInputType {
        message: String,
        locations: Locations,
    },
    #[error("{message}")]
    InputFieldMergeFailed {
        message: String,
        locations: Locations,
    },
    #[error("{message}")]
    FieldArgumentTypeMismatch { message: String },
    #[error("{message}")]
    FieldTypeMismatch { message: String },
    #[error("{message}")]
    OverrideCollisionWithAnotherDirective { message: String },
    #[error("{message}")]
    OverrideFromSelfError { message: String },
    #[error("{message}")]
    OverrideLabelInvalid { message: String },
    #[error("{message}")]
    OverrideOnInterface { message: String },
    #[error("{message}")]
    OverrideSourceHasOverride { message: String },
    #[error("{message}")]
    QueryRootMissing { message: String },
    #[error("{message}")]
    ArgumentDefaultMismatch {
        message: String,
        locations: Locations,
    },
    #[error("{message}")]
    InputFieldDefaultMismatch {
        message: String,
        locations: Locations,
    },
}

impl CompositionError {
    pub fn code(&self) -> ErrorCode {
        match self {
            Self::SubgraphError { error, .. } => error.code(),
            Self::ContextualArgumentNotContextualInAllSubgraphs { .. } => {
                ErrorCode::ContextualArgumentNotContextualInAllSubgraphs
            }
            Self::EmptyMergedEnumType { .. } => ErrorCode::EmptyMergedEnumType,
            Self::EnumValueMismatch { .. } => ErrorCode::EnumValueMismatch,
            Self::ExternalTypeMismatch { .. } => ErrorCode::ExternalTypeMismatch,
            Self::ExternalArgumentTypeMismatch { .. } => ErrorCode::ExternalArgumentTypeMismatch,
            Self::ExternalArgumentDefaultMismatch { .. } => {
                ErrorCode::ExternalArgumentDefaultMismatch
            }
            Self::InvalidGraphQL { .. } => ErrorCode::InvalidGraphQL,
            Self::InvalidGraphQLName(..) => ErrorCode::InvalidGraphQL,
            Self::FromContextParseError { .. } => ErrorCode::InvalidGraphQL,
            Self::UnsupportedSpreadDirective { .. } => ErrorCode::InvalidGraphQL,
            Self::DirectiveDefinitionInvalid { .. } => ErrorCode::DirectiveDefinitionInvalid,
            Self::TypeDefinitionInvalid { .. } => ErrorCode::TypeDefinitionInvalid,
            Self::InterfaceObjectUsageError { .. } => ErrorCode::InterfaceObjectUsageError,
            Self::InterfaceKeyMissingImplementationType { .. } => {
                ErrorCode::InterfaceKeyMissingImplementationType
            }
            Self::TypeKindMismatch { .. } => ErrorCode::TypeKindMismatch,
            Self::ShareableHasMismatchedRuntimeTypes { .. } => {
                ErrorCode::ShareableHasMismatchedRuntimeTypes
            }
            Self::SatisfiabilityError { .. } => ErrorCode::SatisfiabilityError,
            Self::MaxValidationSubgraphPathsExceeded { .. } => {
                ErrorCode::MaxValidationSubgraphPathsExceeded
            }
            Self::InternalError { .. } => ErrorCode::Internal,
            Self::ExternalArgumentMissing { .. } => ErrorCode::ExternalArgumentMissing,
            Self::ExternalMissingOnBase { .. } => ErrorCode::ExternalMissingOnBase,
            Self::MergedDirectiveApplicationOnExternal { .. } => {
                ErrorCode::MergedDirectiveApplicationOnExternal
            }
            Self::LinkImportNameMismatch { .. } => ErrorCode::LinkImportNameMismatch,
            Self::InvalidFieldSharing { .. } => ErrorCode::InvalidFieldSharing,
            Self::InconsistentInputObjectField { .. } => ErrorCode::Internal, // This is for hints, not errors
            Self::RequiredArgumentMissingInSomeSubgraph { .. } => {
                ErrorCode::RequiredArgumentMissingInSomeSubgraph
            }
            Self::RequiredInputFieldMissingInSomeSubgraph { .. } => {
                ErrorCode::RequiredInputFieldMissingInSomeSubgraph
            }
            Self::EmptyMergedInputType { .. } => ErrorCode::EmptyMergedInputType,
            Self::InputFieldMergeFailed { .. } => ErrorCode::InputFieldMergeFailed,
            Self::ExtensionWithNoBase { .. } => ErrorCode::ExtensionWithNoBase,
            Self::DirectiveCompositionError { .. } => ErrorCode::DirectiveCompositionError,
            Self::FieldArgumentTypeMismatch { .. } => ErrorCode::FieldArgumentTypeMismatch,
            Self::FieldTypeMismatch { .. } => ErrorCode::FieldTypeMismatch,
            Self::OverrideCollisionWithAnotherDirective { .. } => {
                ErrorCode::OverrideCollisionWithAnotherDirective
            }
            Self::OverrideFromSelfError { .. } => ErrorCode::OverrideFromSelfError,
            Self::OverrideLabelInvalid { .. } => ErrorCode::OverrideLabelInvalid,
            Self::OverrideOnInterface { .. } => ErrorCode::OverrideOnInterface,
            Self::OverrideSourceHasOverride { .. } => ErrorCode::OverrideSourceHasOverride,
            Self::QueryRootMissing { .. } => ErrorCode::QueryRootMissing,
            Self::ArgumentDefaultMismatch { .. } => ErrorCode::FieldArgumentDefaultMismatch,
            Self::InputFieldDefaultMismatch { .. } => ErrorCode::InputFieldDefaultMismatch,
        }
    }

    pub(crate) fn append_message(self, appendix: impl Display) -> Self {
        match self {
            Self::EmptyMergedEnumType { message, locations } => Self::EmptyMergedEnumType {
                message: format!("{message}{appendix}"),
                locations,
            },
            Self::EnumValueMismatch { message } => Self::EnumValueMismatch {
                message: format!("{message}{appendix}"),
            },
            Self::ExternalTypeMismatch { message } => Self::ExternalTypeMismatch {
                message: format!("{message}{appendix}"),
            },
            Self::ExternalArgumentTypeMismatch { message } => Self::ExternalArgumentTypeMismatch {
                message: format!("{message}{appendix}"),
            },
            Self::ExternalArgumentDefaultMismatch { message } => {
                Self::ExternalArgumentDefaultMismatch {
                    message: format!("{message}{appendix}"),
                }
            }
            Self::InvalidGraphQL { message } => Self::InvalidGraphQL {
                message: format!("{message}{appendix}"),
            },
            Self::DirectiveDefinitionInvalid { message } => Self::DirectiveDefinitionInvalid {
                message: format!("{message}{appendix}"),
            },
            Self::TypeDefinitionInvalid { message } => Self::TypeDefinitionInvalid {
                message: format!("{message}{appendix}"),
            },
            Self::InterfaceObjectUsageError { message } => Self::InterfaceObjectUsageError {
                message: format!("{message}{appendix}"),
            },
            Self::InterfaceKeyMissingImplementationType { message } => {
                Self::InterfaceKeyMissingImplementationType {
                    message: format!("{message}{appendix}"),
                }
            }
            Self::TypeKindMismatch { message } => Self::TypeKindMismatch {
                message: format!("{message}{appendix}"),
            },
            Self::ShareableHasMismatchedRuntimeTypes { message } => {
                Self::ShareableHasMismatchedRuntimeTypes {
                    message: format!("{message}{appendix}"),
                }
            }
            Self::SatisfiabilityError { message } => Self::SatisfiabilityError {
                message: format!("{message}{appendix}"),
            },
            Self::MaxValidationSubgraphPathsExceeded { message } => {
                Self::MaxValidationSubgraphPathsExceeded {
                    message: format!("{message}{appendix}"),
                }
            }
            Self::InternalError { message } => Self::InternalError {
                message: format!("{message}{appendix}"),
            },
            Self::ExternalArgumentMissing { message } => Self::ExternalArgumentMissing {
                message: format!("{message}{appendix}"),
            },
            Self::ExternalMissingOnBase { message } => Self::ExternalMissingOnBase {
                message: format!("{message}{appendix}"),
            },
            Self::MergedDirectiveApplicationOnExternal { message } => {
                Self::MergedDirectiveApplicationOnExternal {
                    message: format!("{message}{appendix}"),
                }
            }
            Self::LinkImportNameMismatch { message } => Self::LinkImportNameMismatch {
                message: format!("{message}{appendix}"),
            },
            Self::InvalidFieldSharing { message, locations } => Self::InvalidFieldSharing {
                message: format!("{message}{appendix}"),
                locations,
            },
            Self::DirectiveCompositionError { message } => Self::DirectiveCompositionError {
                message: format!("{message}{appendix}"),
            },
            Self::InconsistentInputObjectField { message } => Self::InconsistentInputObjectField {
                message: format!("{message}{appendix}"),
            },
            Self::RequiredArgumentMissingInSomeSubgraph { message, locations } => {
                Self::RequiredArgumentMissingInSomeSubgraph {
                    message: format!("{message}{appendix}"),
                    locations,
                }
            }
            Self::RequiredInputFieldMissingInSomeSubgraph { message, locations } => {
                Self::RequiredInputFieldMissingInSomeSubgraph {
                    message: format!("{message}{appendix}"),
                    locations,
                }
            }
            Self::EmptyMergedInputType { message, locations } => Self::EmptyMergedInputType {
                message: format!("{message}{appendix}"),
                locations,
            },
            Self::InputFieldMergeFailed { message, locations } => Self::InputFieldMergeFailed {
                message: format!("{message}{appendix}"),
                locations,
            },
            Self::FieldArgumentTypeMismatch { message } => Self::FieldArgumentTypeMismatch {
                message: format!("{message}{appendix}"),
            },
            Self::FieldTypeMismatch { message } => Self::FieldTypeMismatch {
                message: format!("{message}{appendix}"),
            },
            Self::ContextualArgumentNotContextualInAllSubgraphs { message, locations } => {
                Self::ContextualArgumentNotContextualInAllSubgraphs {
                    message: format!("{message}{appendix}"),
                    locations,
                }
            }
            Self::ArgumentDefaultMismatch { message, locations } => Self::ArgumentDefaultMismatch {
                message: format!("{message}{appendix}"),
                locations,
            },
            Self::InputFieldDefaultMismatch { message, locations } => {
                Self::InputFieldDefaultMismatch {
                    message: format!("{message}{appendix}"),
                    locations,
                }
            }
            // Remaining errors do not have an obvious way to appending a message, so we just return self.
            Self::SubgraphError { .. }
            | Self::InvalidGraphQLName(..)
            | Self::FromContextParseError { .. }
            | Self::UnsupportedSpreadDirective { .. }
            | Self::ExtensionWithNoBase { .. }
            | Self::OverrideCollisionWithAnotherDirective { .. }
            | Self::OverrideFromSelfError { .. }
            | Self::OverrideLabelInvalid { .. }
            | Self::OverrideOnInterface { .. }
            | Self::OverrideSourceHasOverride { .. }
            | Self::QueryRootMissing { .. } => self,
        }
    }

    pub fn locations(&self) -> &[SubgraphLocation] {
        match self {
            Self::SubgraphError { locations, .. }
            | Self::EmptyMergedEnumType { locations, .. }
            | Self::InputFieldMergeFailed { locations, .. }
            | Self::ExtensionWithNoBase { locations, .. }
            | Self::RequiredArgumentMissingInSomeSubgraph { locations, .. }
            | Self::RequiredInputFieldMissingInSomeSubgraph { locations, .. }
            | Self::EmptyMergedInputType { locations, .. }
            | Self::InvalidFieldSharing { locations, .. }
            | Self::ArgumentDefaultMismatch { locations, .. }
            | Self::InputFieldDefaultMismatch { locations, .. } => locations,
            _ => &[],
        }
    }
}

impl SubgraphError {
    pub fn to_composition_errors(&self) -> impl Iterator<Item = CompositionError> {
        self.errors
            .iter()
            .map(move |error| CompositionError::SubgraphError {
                subgraph: self.subgraph.clone(),
                error: error.error.clone(),
                locations: error
                    .locations
                    .iter()
                    .map(|range| SubgraphLocation {
                        subgraph: self.subgraph.clone(),
                        range: range.clone(),
                    })
                    .collect(),
            })
    }
}

/* TODO(@tylerbloom): This is currently not needed. SingleFederation errors are aggregated using
 * MultipleFederationErrors. This is then turned into a FederationError, then in a SubgraphError,
 * and finally into a CompositionError. Not implementing this yet also ensures that any
 * SingleFederationErrors that are intented on becoming SubgraphErrors still do.
impl<E: Into<FederationError>> From<E> for SingleCompositionError {
    fn from(_value: E) -> Self {
        todo!()
    }
}
*/

#[derive(Debug, Clone, thiserror::Error)]
pub enum SingleFederationError {
    #[error(
        "An internal error has occurred, please report this bug to Apollo.\n\nDetails: {message}"
    )]
    Internal { message: String },
    #[error("An internal error has occurred, please report this bug to Apollo. Details: {0}")]
    #[allow(private_interfaces)] // users should not inspect this.
    InternalRebaseError(#[from] crate::operation::RebaseError),
    // This is a known bug that will take time to fix, and does not require reporting.
    #[error("{message}")]
    InternalUnmergeableFields { message: String },
    // InvalidGraphQL: We need to be able to modify the message text from apollo-compiler. So, we
    //                 format the DiagnosticData into String here. We can add additional data as
    //                 necessary.
    #[error("{message}")]
    InvalidGraphQL { message: String },
    #[error(transparent)]
    InvalidGraphQLName(#[from] InvalidNameError),
    #[error("Subgraph invalid: {message}")]
    InvalidSubgraph { message: String },
    #[error("Operation name not found")]
    UnknownOperation,
    #[error("Must provide operation name if query contains multiple operations")]
    OperationNameNotProvided,
    #[error(r#"{message} in @fromContext substring "{context}""#)]
    FromContextParseError { context: String, message: String },
    #[error(
        "Unsupported custom directive @{name} on fragment spread. Due to query transformations during planning, the router requires directives on fragment spreads to support both the FRAGMENT_SPREAD and INLINE_FRAGMENT locations."
    )]
    UnsupportedSpreadDirective { name: Name },
    #[error("{message}")]
    DirectiveDefinitionInvalid { message: String },
    #[error("{message}")]
    TypeDefinitionInvalid { message: String },
    #[error("{message}")]
    UnsupportedFederationDirective { message: String },
    #[error("{message}")]
    UnsupportedFederationVersion { message: String },
    #[error("{message}")]
    UnsupportedLinkedFeature { message: String },
    #[error("{message}")]
    UnknownFederationLinkVersion { message: String },
    #[error("{message}")]
    UnknownLinkVersion { message: String },
    #[error(
        "On type \"{target_type}\", for {application}: field {inner_coordinate} cannot be included because it has arguments (fields with argument are not allowed in @key)"
    )]
    KeyFieldsHasArgs {
        target_type: Name,
        application: String,
        inner_coordinate: String,
    },
    #[error(
        "On field \"{coordinate}\", for {application}: field {inner_coordinate} cannot be included because it has arguments (fields with argument are not allowed in @provides)"
    )]
    ProvidesFieldsHasArgs {
        coordinate: String,
        application: String,
        inner_coordinate: String,
    },
    #[error("On field \"{coordinate}\", for {application}: {message}")]
    ProvidesFieldsMissingExternal {
        coordinate: String,
        application: String,
        message: String,
    },
    #[error("On field \"{coordinate}\", for {application}: {message}")]
    RequiresFieldsMissingExternal {
        coordinate: String,
        application: String,
        message: String,
    },
    #[error("{message}")]
    KeyUnsupportedOnInterface { message: String },
    #[error("{message}")]
    ProvidesUnsupportedOnInterface { message: String },
    #[error("{message}")]
    RequiresUnsupportedOnInterface { message: String },
    #[error(
        "On type \"{target_type}\", for {application}: cannot have directive applications in the @key(fields:) argument but found {applied_directives}."
    )]
    KeyHasDirectiveInFieldsArg {
        target_type: Name,
        application: String,
        applied_directives: String,
    },
    #[error(
        "On field \"{coordinate}\", for {application}: cannot have directive applications in the @provides(fields:) argument but found {applied_directives}."
    )]
    ProvidesHasDirectiveInFieldsArg {
        coordinate: String,
        application: String,
        applied_directives: String,
    },
    #[error(
        "On field \"{coordinate}\", for {application}: cannot have directive applications in the @requires(fields:) argument but found {applied_directives}."
    )]
    RequiresHasDirectiveInFieldsArg {
        coordinate: String,
        application: String,
        applied_directives: String,
    },
    #[error("{message}")]
    ExternalUnused { message: String },
    #[error(
        "Type {type_name} contains only external fields and all those fields are all unused (they do not appear in any @key, @provides or @requires)."
    )]
    TypeWithOnlyUnusedExternal { type_name: Name },
    #[error("{message}")]
    ProvidesOnNonObjectField { message: String },
    #[error(
        "On type \"{target_type}\", for {application}: Invalid value for argument \"fields\": must be a string."
    )]
    KeyInvalidFieldsType {
        target_type: Name,
        application: String,
    },
    #[error(
        "On field \"{coordinate}\", for {application}: Invalid value for argument \"fields\": must be a string."
    )]
    ProvidesInvalidFieldsType {
        coordinate: String,
        application: String,
    },
    #[error(
        "On field \"{coordinate}\", for {application}: Invalid value for argument \"fields\": must be a string."
    )]
    RequiresInvalidFieldsType {
        coordinate: String,
        application: String,
    },
    #[error("On type \"{target_type}\", for {application}: {message}")]
    KeyInvalidFields {
        target_type: Name,
        application: String,
        message: String,
    },
    #[error("On field \"{coordinate}\", for {application}: {message}")]
    ProvidesInvalidFields {
        coordinate: String,
        application: String,
        message: String,
    },
    #[error("On field \"{coordinate}\", for {application}: {message}")]
    RequiresInvalidFields {
        coordinate: String,
        application: String,
        message: String,
    },
    #[error("On type \"{target_type}\", for {application}: {message}")]
    KeyFieldsSelectInvalidType {
        target_type: Name,
        application: String,
        message: String,
    },
    #[error(
        "The schema has a type named \"{expected_name}\" but it is not set as the query root type (\"{found_name}\" is instead): this is not supported by federation. If a root type does not use its default name, there should be no other type with that default name."
    )]
    RootQueryUsed {
        expected_name: Name,
        found_name: Name,
    },
    #[error(
        "The schema has a type named \"{expected_name}\" but it is not set as the mutation root type (\"{found_name}\" is instead): this is not supported by federation. If a root type does not use its default name, there should be no other type with that default name."
    )]
    RootMutationUsed {
        expected_name: Name,
        found_name: Name,
    },
    #[error(
        "The schema has a type named \"{expected_name}\" but it is not set as the subscription root type (\"{found_name}\" is instead): this is not supported by federation. If a root type does not use its default name, there should be no other type with that default name."
    )]
    RootSubscriptionUsed {
        expected_name: Name,
        found_name: Name,
    },
    #[error("{message}")]
    InvalidSubgraphName { message: String },
    #[error("{message}")]
    NoQueries { message: String },
    #[error("{message}")]
    InterfaceFieldNoImplem { message: String },
    #[error("{message}")]
    ExternalTypeMismatch { message: String },
    #[error("{message}")]
    ExternalCollisionWithAnotherDirective { message: String },
    #[error("{message}")]
    ExternalArgumentMissing { message: String },
    #[error("{message}")]
    ExternalArgumentTypeMismatch { message: String },
    #[error("{message}")]
    ExternalArgumentDefaultMismatch { message: String },
    #[error("{message}")]
    ExternalOnInterface { message: String },
    #[error("{message}")]
    MergedDirectiveApplicationOnExternal { message: String },
    #[error("{message}")]
    FieldTypeMismatch { message: String },
    #[error("{message}")]
    FieldArgumentTypeMismatch { message: String },
    #[error("{message}")]
    InputFieldDefaultMismatch { message: String },
    #[error("{message}")]
    FieldArgumentDefaultMismatch { message: String },
    #[error("{message}")]
    ExtensionWithNoBase { message: String },
    #[error("{message}")]
    ExternalMissingOnBase { message: String },
    #[error("{message}")]
    InvalidFieldSharing { message: String },
    #[error("{message}")]
    InvalidShareableUsage { message: String },
    #[error("{message}")]
    InvalidLinkDirectiveUsage { message: String },
    #[error("{message}")]
    InvalidLinkIdentifier { message: String },
    #[error("{message}")]
    ReferencedInaccessible { message: String },
    #[error("{message}")]
    DefaultValueUsesInaccessible { message: String },
    #[error("{message}")]
    QueryRootTypeInaccessible { message: String },
    #[error("{message}")]
    RequiredInaccessible { message: String },
    #[error("{message}")]
    ImplementedByInaccessible { message: String },
    #[error("{message}")]
    DisallowedInaccessible { message: String },
    #[error("{message}")]
    OnlyInaccessibleChildren { message: String },
    #[error("{message}")]
    RequiredInputFieldMissingInSomeSubgraph { message: String },
    #[error("{message}")]
    RequiredArgumentMissingInSomeSubgraph { message: String },
    #[error("{message}")]
    EmptyMergedInputType { message: String },
    #[error("{message}")]
    EnumValueMismatch { message: String },
    #[error("{message}")]
    EmptyMergedEnumType { message: String },
    #[error("{message}")]
    ShareableHasMismatchedRuntimeTypes { message: String },
    #[error("{message}")]
    SatisfiabilityError { message: String },
    #[error("{message}")]
    OverrideFromSelfError { message: String },
    #[error("{message}")]
    OverrideSourceHasOverride { message: String },
    #[error("{message}")]
    OverrideCollisionWithAnotherDirective { message: String },
    #[error("{message}")]
    OverrideOnInterface { message: String },
    #[error("{message}")]
    UnsupportedFeature {
        message: String,
        kind: UnsupportedFeatureKind,
    },
    #[error("{message}")]
    InvalidFederationSupergraph { message: String },
    #[error("{message}")]
    DownstreamServiceError { message: String },
    #[error("{message}")]
    DirectiveCompositionError { message: String },
    #[error("{message}")]
    InterfaceObjectUsageError { message: String },
    #[error("{message}")]
    InterfaceKeyNotOnImplementation { message: String },
    #[error("{message}")]
    InterfaceKeyMissingImplementationType { message: String },
    #[error("@defer is not supported on subscriptions")]
    DeferredSubscriptionUnsupported,
    #[error("{message}")]
    QueryPlanComplexityExceeded { message: String },
    #[error("the caller requested cancellation")]
    PlanningCancelled,
    #[error("No plan was found when subgraphs were disabled")]
    NoPlanFoundWithDisabledSubgraphs,
    #[error("Context name \"{name}\" may not contain an underscore.")]
    ContextNameContainsUnderscore { name: String },
    #[error("Context name \"{name}\" is invalid. It should have only alphanumeric characters.")]
    ContextNameInvalid { name: String },
    #[error("{message}")]
    ContextNotSet { message: String },
    #[error("{message}")]
    NoContextReferenced { message: String },
    #[error("{message}")]
    NoSelectionForContext { message: String },
    #[error("{message}")]
    ContextNoResolvableKey { message: String },
    #[error("@cost cannot be applied to interface \"{interface}.{field}\"")]
    CostAppliedToInterfaceField { interface: Name, field: Name },
    #[error("{message}")]
    ContextSelectionInvalid { message: String },
    #[error("{message}")]
    ListSizeAppliedToNonList { message: String },
    #[error("{message}")]
    ListSizeInvalidAssumedSize { message: String },
    #[error("{message}")]
    ListSizeInvalidSlicingArgument { message: String },
    #[error("{message}")]
    ListSizeInvalidSizedField { message: String },
    #[error("{message}")]
    InvalidTagName { message: String },
    #[error("{message}")]
    QueryRootMissing { message: String },
}

impl SingleFederationError {
    pub fn code(&self) -> ErrorCode {
        match self {
            SingleFederationError::Internal { .. } => ErrorCode::Internal,
            SingleFederationError::InternalRebaseError { .. } => ErrorCode::Internal,
            SingleFederationError::InternalUnmergeableFields { .. } => ErrorCode::Internal,
            SingleFederationError::InvalidGraphQL { .. }
            | SingleFederationError::InvalidGraphQLName(_) => ErrorCode::InvalidGraphQL,
            SingleFederationError::InvalidSubgraph { .. } => ErrorCode::InvalidGraphQL,
            // Technically it's not invalid graphql, but it is invalid syntax inside graphql...
            SingleFederationError::FromContextParseError { .. } => ErrorCode::InvalidGraphQL,
            // TODO(@goto-bus-stop): this should have a different error code: it's not invalid,
            // just unsupported due to internal limitations.
            SingleFederationError::UnsupportedSpreadDirective { .. } => ErrorCode::InvalidGraphQL,
            // TODO(@goto-bus-stop): this should have a different error code: it's not the graphql
            // that's invalid, but the operation name
            SingleFederationError::UnknownOperation => ErrorCode::InvalidGraphQL,
            SingleFederationError::OperationNameNotProvided => ErrorCode::InvalidGraphQL,
            SingleFederationError::DirectiveDefinitionInvalid { .. } => {
                ErrorCode::DirectiveDefinitionInvalid
            }
            SingleFederationError::TypeDefinitionInvalid { .. } => ErrorCode::TypeDefinitionInvalid,
            SingleFederationError::UnsupportedFederationDirective { .. } => {
                ErrorCode::UnsupportedFederationDirective
            }
            SingleFederationError::UnsupportedFederationVersion { .. } => {
                ErrorCode::UnsupportedFederationVersion
            }
            SingleFederationError::UnsupportedLinkedFeature { .. } => {
                ErrorCode::UnsupportedLinkedFeature
            }
            SingleFederationError::UnknownFederationLinkVersion { .. } => {
                ErrorCode::UnknownFederationLinkVersion
            }
            SingleFederationError::UnknownLinkVersion { .. } => ErrorCode::UnknownLinkVersion,
            SingleFederationError::KeyFieldsHasArgs { .. } => ErrorCode::KeyFieldsHasArgs,
            SingleFederationError::ProvidesFieldsHasArgs { .. } => ErrorCode::ProvidesFieldsHasArgs,
            SingleFederationError::ProvidesFieldsMissingExternal { .. } => {
                ErrorCode::ProvidesFieldsMissingExternal
            }
            SingleFederationError::RequiresFieldsMissingExternal { .. } => {
                ErrorCode::RequiresFieldsMissingExternal
            }
            SingleFederationError::KeyUnsupportedOnInterface { .. } => {
                ErrorCode::KeyUnsupportedOnInterface
            }
            SingleFederationError::ProvidesUnsupportedOnInterface { .. } => {
                ErrorCode::ProvidesUnsupportedOnInterface
            }
            SingleFederationError::RequiresUnsupportedOnInterface { .. } => {
                ErrorCode::RequiresUnsupportedOnInterface
            }
            SingleFederationError::KeyHasDirectiveInFieldsArg { .. } => {
                ErrorCode::KeyDirectiveInFieldsArgs
            }
            SingleFederationError::ProvidesHasDirectiveInFieldsArg { .. } => {
                ErrorCode::ProvidesDirectiveInFieldsArgs
            }
            SingleFederationError::RequiresHasDirectiveInFieldsArg { .. } => {
                ErrorCode::RequiresDirectiveInFieldsArgs
            }
            SingleFederationError::ExternalUnused { .. } => ErrorCode::ExternalUnused,
            SingleFederationError::TypeWithOnlyUnusedExternal { .. } => {
                ErrorCode::TypeWithOnlyUnusedExternal
            }
            SingleFederationError::ProvidesOnNonObjectField { .. } => {
                ErrorCode::ProvidesOnNonObjectField
            }
            SingleFederationError::KeyInvalidFieldsType { .. } => ErrorCode::KeyInvalidFieldsType,
            SingleFederationError::ProvidesInvalidFieldsType { .. } => {
                ErrorCode::ProvidesInvalidFieldsType
            }
            SingleFederationError::RequiresInvalidFieldsType { .. } => {
                ErrorCode::RequiresInvalidFieldsType
            }
            SingleFederationError::KeyInvalidFields { .. } => ErrorCode::KeyInvalidFields,
            SingleFederationError::ProvidesInvalidFields { .. } => ErrorCode::ProvidesInvalidFields,
            SingleFederationError::RequiresInvalidFields { .. } => ErrorCode::RequiresInvalidFields,
            SingleFederationError::KeyFieldsSelectInvalidType { .. } => {
                ErrorCode::KeyFieldsSelectInvalidType
            }
            SingleFederationError::RootQueryUsed { .. } => ErrorCode::RootQueryUsed,
            SingleFederationError::RootMutationUsed { .. } => ErrorCode::RootMutationUsed,
            SingleFederationError::RootSubscriptionUsed { .. } => ErrorCode::RootSubscriptionUsed,
            SingleFederationError::InvalidSubgraphName { .. } => ErrorCode::InvalidSubgraphName,
            SingleFederationError::NoQueries { .. } => ErrorCode::NoQueries,
            SingleFederationError::InterfaceFieldNoImplem { .. } => {
                ErrorCode::InterfaceFieldNoImplem
            }
            SingleFederationError::ExternalTypeMismatch { .. } => ErrorCode::ExternalTypeMismatch,
            SingleFederationError::ExternalCollisionWithAnotherDirective { .. } => {
                ErrorCode::ExternalCollisionWithAnotherDirective
            }
            SingleFederationError::ExternalArgumentMissing { .. } => {
                ErrorCode::ExternalArgumentMissing
            }
            SingleFederationError::ExternalArgumentTypeMismatch { .. } => {
                ErrorCode::ExternalArgumentTypeMismatch
            }
            SingleFederationError::ExternalArgumentDefaultMismatch { .. } => {
                ErrorCode::ExternalArgumentDefaultMismatch
            }
            SingleFederationError::ExternalOnInterface { .. } => ErrorCode::ExternalOnInterface,
            SingleFederationError::MergedDirectiveApplicationOnExternal { .. } => {
                ErrorCode::MergedDirectiveApplicationOnExternal
            }
            SingleFederationError::FieldTypeMismatch { .. } => ErrorCode::FieldTypeMismatch,
            SingleFederationError::FieldArgumentTypeMismatch { .. } => {
                ErrorCode::FieldArgumentTypeMismatch
            }
            SingleFederationError::InputFieldDefaultMismatch { .. } => {
                ErrorCode::InputFieldDefaultMismatch
            }
            SingleFederationError::FieldArgumentDefaultMismatch { .. } => {
                ErrorCode::FieldArgumentDefaultMismatch
            }
            SingleFederationError::ExtensionWithNoBase { .. } => ErrorCode::ExtensionWithNoBase,
            SingleFederationError::ExternalMissingOnBase { .. } => ErrorCode::ExternalMissingOnBase,
            SingleFederationError::InvalidFieldSharing { .. } => ErrorCode::InvalidFieldSharing,
            SingleFederationError::InvalidShareableUsage { .. } => ErrorCode::InvalidShareableUsage,
            SingleFederationError::InvalidLinkDirectiveUsage { .. } => {
                ErrorCode::InvalidLinkDirectiveUsage
            }
            SingleFederationError::InvalidLinkIdentifier { .. } => ErrorCode::InvalidLinkIdentifier,
            SingleFederationError::ReferencedInaccessible { .. } => {
                ErrorCode::ReferencedInaccessible
            }
            SingleFederationError::DefaultValueUsesInaccessible { .. } => {
                ErrorCode::DefaultValueUsesInaccessible
            }
            SingleFederationError::QueryRootTypeInaccessible { .. } => {
                ErrorCode::QueryRootTypeInaccessible
            }
            SingleFederationError::RequiredInaccessible { .. } => ErrorCode::RequiredInaccessible,
            SingleFederationError::ImplementedByInaccessible { .. } => {
                ErrorCode::ImplementedByInaccessible
            }
            SingleFederationError::DisallowedInaccessible { .. } => {
                ErrorCode::DisallowedInaccessible
            }
            SingleFederationError::OnlyInaccessibleChildren { .. } => {
                ErrorCode::OnlyInaccessibleChildren
            }
            SingleFederationError::RequiredInputFieldMissingInSomeSubgraph { .. } => {
                ErrorCode::RequiredInputFieldMissingInSomeSubgraph
            }
            SingleFederationError::RequiredArgumentMissingInSomeSubgraph { .. } => {
                ErrorCode::RequiredArgumentMissingInSomeSubgraph
            }
            SingleFederationError::EmptyMergedInputType { .. } => ErrorCode::EmptyMergedInputType,
            SingleFederationError::EnumValueMismatch { .. } => ErrorCode::EnumValueMismatch,
            SingleFederationError::EmptyMergedEnumType { .. } => ErrorCode::EmptyMergedEnumType,
            SingleFederationError::ShareableHasMismatchedRuntimeTypes { .. } => {
                ErrorCode::ShareableHasMismatchedRuntimeTypes
            }
            SingleFederationError::SatisfiabilityError { .. } => ErrorCode::SatisfiabilityError,
            SingleFederationError::OverrideFromSelfError { .. } => ErrorCode::OverrideFromSelfError,
            SingleFederationError::OverrideSourceHasOverride { .. } => {
                ErrorCode::OverrideSourceHasOverride
            }
            SingleFederationError::OverrideCollisionWithAnotherDirective { .. } => {
                ErrorCode::OverrideCollisionWithAnotherDirective
            }
            SingleFederationError::OverrideOnInterface { .. } => ErrorCode::OverrideOnInterface,
            SingleFederationError::UnsupportedFeature { .. } => ErrorCode::UnsupportedFeature,
            SingleFederationError::InvalidFederationSupergraph { .. } => {
                ErrorCode::InvalidFederationSupergraph
            }
            SingleFederationError::DownstreamServiceError { .. } => {
                ErrorCode::DownstreamServiceError
            }
            SingleFederationError::DirectiveCompositionError { .. } => {
                ErrorCode::DirectiveCompositionError
            }
            SingleFederationError::InterfaceObjectUsageError { .. } => {
                ErrorCode::InterfaceObjectUsageError
            }
            SingleFederationError::InterfaceKeyNotOnImplementation { .. } => {
                ErrorCode::InterfaceKeyNotOnImplementation
            }
            SingleFederationError::InterfaceKeyMissingImplementationType { .. } => {
                ErrorCode::InterfaceKeyMissingImplementationType
            }
            SingleFederationError::DeferredSubscriptionUnsupported => ErrorCode::Internal,
            SingleFederationError::QueryPlanComplexityExceeded { .. } => {
                ErrorCode::QueryPlanComplexityExceededError
            }
            SingleFederationError::PlanningCancelled => ErrorCode::Internal,
            SingleFederationError::NoPlanFoundWithDisabledSubgraphs => {
                ErrorCode::NoPlanFoundWithDisabledSubgraphs
            }
            SingleFederationError::ContextNameContainsUnderscore { .. } => {
                ErrorCode::ContextNameContainsUnderscore
            }
            SingleFederationError::ContextNameInvalid { .. } => ErrorCode::ContextNameInvalid,
            SingleFederationError::ContextNotSet { .. } => ErrorCode::ContextNotSet,
            SingleFederationError::NoContextReferenced { .. } => ErrorCode::NoContextReferenced,
            SingleFederationError::NoSelectionForContext { .. } => ErrorCode::NoSelectionForContext,
            SingleFederationError::ContextNoResolvableKey { .. } => {
                ErrorCode::ContextNoResolvableKey
            }
            SingleFederationError::ContextSelectionInvalid { .. } => {
                ErrorCode::ContextSelectionInvalid
            }
            SingleFederationError::CostAppliedToInterfaceField { .. } => {
                ErrorCode::CostAppliedToInterfaceField
            }
            SingleFederationError::ListSizeAppliedToNonList { .. } => {
                ErrorCode::ListSizeAppliedToNonList
            }
            SingleFederationError::ListSizeInvalidAssumedSize { .. } => {
                ErrorCode::ListSizeInvalidAssumedSize
            }
            SingleFederationError::ListSizeInvalidSlicingArgument { .. } => {
                ErrorCode::ListSizeInvalidSlicingArgument
            }
            SingleFederationError::ListSizeInvalidSizedField { .. } => {
                ErrorCode::ListSizeInvalidSizedField
            }
            #[allow(unused)]
            SingleFederationError::InvalidFieldSharing { .. } => ErrorCode::InvalidFieldSharing,
            SingleFederationError::InvalidTagName { .. } => ErrorCode::InvalidTagName,
            SingleFederationError::QueryRootMissing { .. } => ErrorCode::QueryRootMissing,
        }
    }

    pub fn code_string(&self) -> String {
        self.code().definition().code().to_string()
    }

    pub(crate) fn root_already_used(
        operation_type: OperationType,
        expected_name: Name,
        found_name: Name,
    ) -> Self {
        match operation_type {
            OperationType::Query => Self::RootQueryUsed {
                expected_name,
                found_name,
            },
            OperationType::Mutation => Self::RootMutationUsed {
                expected_name,
                found_name,
            },
            OperationType::Subscription => Self::RootSubscriptionUsed {
                expected_name,
                found_name,
            },
        }
    }
}

impl From<InvalidNameError> for FederationError {
    fn from(err: InvalidNameError) -> Self {
        SingleFederationError::from(err).into()
    }
}

impl From<FederationSpecError> for FederationError {
    fn from(err: FederationSpecError) -> Self {
        // TODO: When we get around to finishing the composition port, we should really switch it to
        // using FederationError instead of FederationSpecError.
        let message = err.to_string();
        match err {
            FederationSpecError::UnsupportedVersionError { .. } => {
                SingleFederationError::UnsupportedFederationVersion { message }.into()
            }
            FederationSpecError::UnsupportedFederationDirective { .. } => {
                SingleFederationError::UnsupportedFederationDirective { message }.into()
            }
            FederationSpecError::InvalidGraphQLName(message) => message.into(),
        }
    }
}

#[derive(Debug, Clone, thiserror::Error, Default)]
pub struct MultipleFederationErrors {
    pub(crate) errors: Vec<SingleFederationError>,
}

impl MultipleFederationErrors {
    pub fn new() -> Self {
        Self { errors: vec![] }
    }

    pub fn push(&mut self, error: FederationError) {
        match error {
            FederationError::SingleFederationError(error) => {
                self.errors.push(error);
            }
            FederationError::MultipleFederationErrors(errors) => {
                self.errors.extend(errors.errors);
            }
            FederationError::AggregateFederationError(errors) => {
                self.errors.extend(errors.causes);
            }
        }
    }

    pub(crate) fn and_try(mut self, other: Result<(), FederationError>) -> Self {
        match other {
            Ok(_) => self,
            Err(e) => {
                self.push(e);
                self
            }
        }
    }
}

impl Display for MultipleFederationErrors {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "The following errors occurred:")?;
        for error in &self.errors {
            write!(f, "\n  - ")?;
            for c in error.to_string().chars() {
                if c == '\n' {
                    write!(f, "\n    ")?;
                } else {
                    f.write_char(c)?;
                }
            }
        }
        Ok(())
    }
}

impl FromIterator<SingleFederationError> for MultipleFederationErrors {
    fn from_iter<T: IntoIterator<Item = SingleFederationError>>(iter: T) -> Self {
        Self {
            errors: iter.into_iter().collect(),
        }
    }
}

#[derive(Debug, Clone, thiserror::Error)]
pub struct AggregateFederationError {
    pub code: String,
    pub message: String,
    pub causes: Vec<SingleFederationError>,
}

impl Display for AggregateFederationError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "[{}] {}\nCaused by:", self.code, self.message)?;
        for error in &self.causes {
            write!(f, "\n\n  - ")?;
            for c in error.to_string().chars() {
                if c == '\n' {
                    write!(f, "\n    ")?;
                } else {
                    f.write_char(c)?;
                }
            }
        }
        Ok(())
    }
}

// PORT_NOTE: Often times, JS functions would either throw/return a GraphQLError, return a vector
// of GraphQLErrors, or take a vector of GraphQLErrors and group them together under an
// AggregateGraphQLError which itself would have a specific error message and code, and throw that.
// We represent all these cases with an enum, and delegate to the members.
#[derive(Clone, thiserror::Error)]
pub enum FederationError {
    #[error(transparent)]
    SingleFederationError(#[from] SingleFederationError),
    #[error(transparent)]
    MultipleFederationErrors(#[from] MultipleFederationErrors),
    #[error(transparent)]
    AggregateFederationError(#[from] AggregateFederationError),
}

impl std::fmt::Debug for FederationError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::SingleFederationError(inner) => std::fmt::Debug::fmt(inner, f),
            Self::MultipleFederationErrors(inner) => std::fmt::Debug::fmt(inner, f),
            Self::AggregateFederationError(inner) => std::fmt::Debug::fmt(inner, f),
        }
    }
}

impl From<DiagnosticList> for FederationError {
    fn from(value: DiagnosticList) -> Self {
        let errors: Vec<_> = value
            .iter()
            .map(|d| SingleFederationError::InvalidGraphQL {
                message: d.to_string(),
            })
            .collect();
        match errors.len().cmp(&1) {
            Ordering::Less => internal_error!("diagnostic list is unexpectedly empty"),
            Ordering::Equal => errors[0].clone().into(),
            Ordering::Greater => MultipleFederationErrors { errors }.into(),
        }
    }
}

impl<T> From<WithErrors<T>> for FederationError {
    fn from(value: WithErrors<T>) -> Self {
        value.errors.into()
    }
}

// Used for when we condition on a type `T: TryInto<U>`, but we have an infallible conversion of
// `T: Into<U>`. This allows us to unwrap the `Result<U, Infallible>` with `?`.
impl From<std::convert::Infallible> for FederationError {
    fn from(_: std::convert::Infallible) -> Self {
        unreachable!("Infallible should never be converted to FederationError")
    }
}

impl FederationError {
    pub fn internal(message: impl Into<String>) -> Self {
        SingleFederationError::Internal {
            message: message.into(),
        }
        .into()
    }

    pub fn merge(self, other: Self) -> Self {
        let mut result = MultipleFederationErrors::new();
        result.push(self);
        result.push(other);
        result.into()
    }

    pub fn into_errors(self) -> Vec<SingleFederationError> {
        match self {
            FederationError::SingleFederationError(e) => vec![e],
            FederationError::MultipleFederationErrors(e) => e.errors,
            FederationError::AggregateFederationError(e) => e.causes,
        }
    }

    pub fn errors(&self) -> Vec<&SingleFederationError> {
        match self {
            FederationError::SingleFederationError(e) => vec![e],
            FederationError::MultipleFederationErrors(e) => e.errors.iter().collect(),
            FederationError::AggregateFederationError(e) => e.causes.iter().collect(),
        }
    }

    pub fn has_invalid_graphql_error(&self) -> bool {
        self.errors()
            .into_iter()
            .any(|e| matches!(e, SingleFederationError::InvalidGraphQL { .. }))
    }
}

// Similar to `multi_try` crate, but with `FederationError` instead of `Vec<E>`.
pub trait MultiTry<U> {
    type Output;

    fn and_try(self, other: Result<U, FederationError>) -> Self::Output;
}

impl<U> MultiTry<U> for Result<(), FederationError> {
    type Output = Result<U, FederationError>;

    fn and_try(self, other: Result<U, FederationError>) -> Result<U, FederationError> {
        match (self, other) {
            (Ok(_a), Ok(b)) => Ok(b),
            (Ok(_a), Err(b)) => Err(b),
            (Err(a), Ok(_b)) => Err(a),
            (Err(a), Err(b)) => Err(a.merge(b)),
        }
    }
}

pub trait MultiTryAll: Sized + Iterator {
    /// Apply `predicate` on all elements of the iterator, collecting all errors (if any).
    /// - Returns Ok(()), if all elements are Ok.
    /// - Otherwise, returns a FederationError with all errors.
    /// - Note: Not to be confused with `try_for_each`, which stops on the first error.
    fn try_for_all<F>(self, mut predicate: F) -> Result<(), FederationError>
    where
        F: FnMut(Self::Item) -> Result<(), FederationError>,
    {
        let mut errors = MultipleFederationErrors::new();
        for item in self {
            match predicate(item) {
                Ok(()) => {}
                Err(e) => errors.push(e),
            }
        }
        errors.into_result()
    }
}

impl<I: Iterator> MultiTryAll for I {}

impl MultipleFederationErrors {
    /// Converts into `Result<(), FederationError>`.
    /// - The return value can be either Ok, Err with a SingleFederationError or MultipleFederationErrors,
    ///   depending on the number of errors in the input.
    pub fn into_result(self) -> Result<(), FederationError> {
        match self.errors.len().cmp(&1) {
            Ordering::Less => Ok(()),
            Ordering::Equal => Err(self.errors[0].clone().into()),
            Ordering::Greater => Err(self.into()),
        }
    }
}

// We didn't track errors addition precisely pre-2.0 and tracking it now has an unclear ROI, so we
// just mark all the error code that predates 2.0 as 0.x.
const FED1_CODE: &str = "0.x";

#[derive(Debug, Clone)]
pub struct ErrorCodeMetadata {
    pub added_in: &'static str,
    pub replaces: &'static [&'static str],
}

#[derive(Debug)]
pub struct ErrorCodeDefinition {
    code: String,
    // PORT_NOTE: Known as "description" in the JS code. The name was changed to distinguish it from
    // Error.description().
    doc_description: String,
    metadata: ErrorCodeMetadata,
}

impl ErrorCodeDefinition {
    fn new(code: String, doc_description: String, metadata: Option<ErrorCodeMetadata>) -> Self {
        Self {
            code,
            doc_description,
            metadata: metadata.unwrap_or_else(|| DEFAULT_METADATA.clone()),
        }
    }

    pub fn code(&self) -> &str {
        &self.code
    }

    pub fn doc_description(&self) -> &str {
        &self.doc_description
    }

    pub fn metadata(&self) -> &ErrorCodeMetadata {
        &self.metadata
    }
}

/*
 * Most codes currently originate from the initial fed 2 release so we use this for convenience.
 * This can be changed later, inline versions everywhere, if that becomes irrelevant.
 */
static DEFAULT_METADATA: ErrorCodeMetadata = ErrorCodeMetadata {
    added_in: "2.0.0",
    replaces: &[],
};

struct ErrorCodeCategory<TElement: Clone + Into<String>> {
    // Fn(element: TElement) -> String
    extract_code: Box<dyn 'static + Send + Sync + Fn(TElement) -> String>,
    // Fn(element: TElement) -> String
    make_doc_description: Box<dyn 'static + Send + Sync + Fn(TElement) -> String>,
    metadata: ErrorCodeMetadata,
}

impl<TElement: Clone + Into<String>> ErrorCodeCategory<TElement> {
    fn new(
        extract_code: Box<dyn 'static + Send + Sync + Fn(TElement) -> String>,
        make_doc_description: Box<dyn 'static + Send + Sync + Fn(TElement) -> String>,
        metadata: Option<ErrorCodeMetadata>,
    ) -> Self {
        Self {
            extract_code,
            make_doc_description,
            metadata: metadata.unwrap_or_else(|| DEFAULT_METADATA.clone()),
        }
    }

    // PORT_NOTE: The Typescript type in the JS code only has get(), but I also added createCode()
    // here since it's used in the return type of makeErrorCodeCategory().
    fn create_code(&self, element: TElement) -> ErrorCodeDefinition {
        ErrorCodeDefinition::new(
            (self.extract_code)(element.clone()),
            (self.make_doc_description)(element),
            Some(self.metadata.clone()),
        )
    }
}

impl ErrorCodeCategory<String> {
    fn new_federation_directive(
        code_suffix: String,
        make_doc_description: Box<dyn 'static + Send + Sync + Fn(String) -> String>,
        metadata: Option<ErrorCodeMetadata>,
    ) -> Self {
        Self::new(
            Box::new(move |element: String| format!("{}_{}", element.to_uppercase(), code_suffix)),
            make_doc_description,
            metadata,
        )
    }
}

static INVALID_GRAPHQL: LazyLock<ErrorCodeDefinition> = LazyLock::new(|| {
    ErrorCodeDefinition::new(
        "INVALID_GRAPHQL".to_owned(),
        "A schema is invalid GraphQL: it violates one of the rule of the specification.".to_owned(),
        None,
    )
});
static DIRECTIVE_DEFINITION_INVALID: LazyLock<ErrorCodeDefinition> = LazyLock::new(|| {
    ErrorCodeDefinition::new(
        "DIRECTIVE_DEFINITION_INVALID".to_owned(),
        "A built-in or federation directive has an invalid definition in the schema.".to_owned(),
        Some(ErrorCodeMetadata {
            replaces: &["TAG_DEFINITION_INVALID"],
            ..DEFAULT_METADATA.clone()
        }),
    )
});

static TYPE_DEFINITION_INVALID: LazyLock<ErrorCodeDefinition> = LazyLock::new(|| {
    ErrorCodeDefinition::new(
        "TYPE_DEFINITION_INVALID".to_owned(),
        "A built-in or federation type has an invalid definition in the schema.".to_owned(),
        None,
    )
});

static UNSUPPORTED_LINKED_FEATURE: LazyLock<ErrorCodeDefinition> = LazyLock::new(|| {
    ErrorCodeDefinition::new(
        "UNSUPPORTED_LINKED_FEATURE".to_owned(),
        "Indicates that a feature used in a @link is either unsupported or is used with unsupported options.".to_owned(),
        None,
    )
});

static UNKNOWN_FEDERATION_LINK_VERSION: LazyLock<ErrorCodeDefinition> = LazyLock::new(|| {
    ErrorCodeDefinition::new(
        "UNKNOWN_FEDERATION_LINK_VERSION".to_owned(),
        "The version of federation in a @link directive on the schema is unknown.".to_owned(),
        None,
    )
});

static UNKNOWN_LINK_VERSION: LazyLock<ErrorCodeDefinition> = LazyLock::new(|| {
    ErrorCodeDefinition::new(
        "UNKNOWN_LINK_VERSION".to_owned(),
        "The version of @link set on the schema is unknown.".to_owned(),
        Some(ErrorCodeMetadata {
            added_in: "2.1.0",
            replaces: &[],
        }),
    )
});

static FIELDS_HAS_ARGS: LazyLock<ErrorCodeCategory<String>> = LazyLock::new(|| {
    ErrorCodeCategory::new_federation_directive(
        "FIELDS_HAS_ARGS".to_owned(),
        Box::new(|directive| {
            format!(
                "The `fields` argument of a `@{directive}` directive includes a field defined with arguments (which is not currently supported)."
            )
        }),
        None,
    )
});

static KEY_FIELDS_HAS_ARGS: LazyLock<ErrorCodeDefinition> =
    LazyLock::new(|| FIELDS_HAS_ARGS.create_code("key".to_owned()));

static PROVIDES_FIELDS_HAS_ARGS: LazyLock<ErrorCodeDefinition> =
    LazyLock::new(|| FIELDS_HAS_ARGS.create_code("provides".to_owned()));

static DIRECTIVE_FIELDS_MISSING_EXTERNAL: LazyLock<ErrorCodeCategory<String>> = LazyLock::new(
    || {
        ErrorCodeCategory::new_federation_directive(
            "FIELDS_MISSING_EXTERNAL".to_owned(),
            Box::new(|directive| {
                format!(
                    "The `fields` argument of a `@{directive}` directive includes a field that is not marked as `@external`."
                )
            }),
            Some(ErrorCodeMetadata {
                added_in: FED1_CODE,
                replaces: &[],
            }),
        )
    },
);

static PROVIDES_FIELDS_MISSING_EXTERNAL: LazyLock<ErrorCodeDefinition> =
    LazyLock::new(|| DIRECTIVE_FIELDS_MISSING_EXTERNAL.create_code("provides".to_owned()));
static REQUIRES_FIELDS_MISSING_EXTERNAL: LazyLock<ErrorCodeDefinition> =
    LazyLock::new(|| DIRECTIVE_FIELDS_MISSING_EXTERNAL.create_code("requires".to_owned()));

static DIRECTIVE_UNSUPPORTED_ON_INTERFACE: LazyLock<ErrorCodeCategory<String>> =
    LazyLock::new(|| {
        ErrorCodeCategory::new_federation_directive(
            "UNSUPPORTED_ON_INTERFACE".to_owned(),
            Box::new(|directive| {
                let suffix = if directive == "key" {
                    "only supported when @linking to federation 2.3+"
                } else {
                    "not (yet) supported"
                };
                format!("A `@{directive}` directive is used on an interface, which is {suffix}.")
            }),
            None,
        )
    });

static KEY_UNSUPPORTED_ON_INTERFACE: LazyLock<ErrorCodeDefinition> =
    LazyLock::new(|| DIRECTIVE_UNSUPPORTED_ON_INTERFACE.create_code("key".to_owned()));
static PROVIDES_UNSUPPORTED_ON_INTERFACE: LazyLock<ErrorCodeDefinition> =
    LazyLock::new(|| DIRECTIVE_UNSUPPORTED_ON_INTERFACE.create_code("provides".to_owned()));
static REQUIRES_UNSUPPORTED_ON_INTERFACE: LazyLock<ErrorCodeDefinition> =
    LazyLock::new(|| DIRECTIVE_UNSUPPORTED_ON_INTERFACE.create_code("requires".to_owned()));

static DIRECTIVE_IN_FIELDS_ARG: LazyLock<ErrorCodeCategory<String>> = LazyLock::new(|| {
    ErrorCodeCategory::new_federation_directive(
        "DIRECTIVE_IN_FIELDS_ARG".to_owned(),
        Box::new(|directive| {
            format!(
                "The `fields` argument of a `@{directive}` directive includes some directive applications. This is not supported"
            )
        }),
        Some(ErrorCodeMetadata {
            added_in: "2.1.0",
            replaces: &[],
        }),
    )
});

static KEY_DIRECTIVE_IN_FIELDS_ARGS: LazyLock<ErrorCodeDefinition> =
    LazyLock::new(|| DIRECTIVE_IN_FIELDS_ARG.create_code("key".to_owned()));
static PROVIDES_DIRECTIVE_IN_FIELDS_ARGS: LazyLock<ErrorCodeDefinition> =
    LazyLock::new(|| DIRECTIVE_IN_FIELDS_ARG.create_code("provides".to_owned()));
static REQUIRES_DIRECTIVE_IN_FIELDS_ARGS: LazyLock<ErrorCodeDefinition> =
    LazyLock::new(|| DIRECTIVE_IN_FIELDS_ARG.create_code("requires".to_owned()));

static EXTERNAL_UNUSED: LazyLock<ErrorCodeDefinition> = LazyLock::new(|| {
    ErrorCodeDefinition::new(
        "EXTERNAL_UNUSED".to_owned(),
        "An `@external` field is not being used by any instance of `@key`, `@requires`, `@provides` or to satisfy an interface implementation.".to_owned(),
        Some(ErrorCodeMetadata {
            added_in: FED1_CODE,
            replaces: &[],
        }),
)
});

static TYPE_WITH_ONLY_UNUSED_EXTERNAL: LazyLock<ErrorCodeDefinition> = LazyLock::new(|| {
    ErrorCodeDefinition::new(
        "TYPE_WITH_ONLY_UNUSED_EXTERNAL".to_owned(),
        [
            "A federation 1 schema has a composite type comprised only of unused external fields.".to_owned(),
            format!("Note that this error can _only_ be raised for federation 1 schema as federation 2 schema do not allow unused external fields (and errors with code {} will be raised in that case).", EXTERNAL_UNUSED.code),
            "But when federation 1 schema are automatically migrated to federation 2 ones, unused external fields are automatically removed, and in rare case this can leave a type empty. If that happens, an error with this code will be raised".to_owned()
        ].join(" "),
        None,
)
});

static PROVIDES_ON_NON_OBJECT_FIELD: LazyLock<ErrorCodeDefinition> = LazyLock::new(|| {
    ErrorCodeDefinition::new(
        "PROVIDES_ON_NON_OBJECT_FIELD".to_owned(),
        "A `@provides` directive is used to mark a field whose base type is not an object type."
            .to_owned(),
        None,
    )
});

static DIRECTIVE_INVALID_FIELDS_TYPE: LazyLock<ErrorCodeCategory<String>> = LazyLock::new(|| {
    ErrorCodeCategory::new_federation_directive(
        "INVALID_FIELDS_TYPE".to_owned(),
        Box::new(|directive| {
            format!(
                "The value passed to the `fields` argument of a `@{directive}` directive is not a string."
            )
        }),
        None,
    )
});

static KEY_INVALID_FIELDS_TYPE: LazyLock<ErrorCodeDefinition> =
    LazyLock::new(|| DIRECTIVE_INVALID_FIELDS_TYPE.create_code("key".to_owned()));
static PROVIDES_INVALID_FIELDS_TYPE: LazyLock<ErrorCodeDefinition> =
    LazyLock::new(|| DIRECTIVE_INVALID_FIELDS_TYPE.create_code("provides".to_owned()));
static REQUIRES_INVALID_FIELDS_TYPE: LazyLock<ErrorCodeDefinition> =
    LazyLock::new(|| DIRECTIVE_INVALID_FIELDS_TYPE.create_code("requires".to_owned()));

static DIRECTIVE_INVALID_FIELDS: LazyLock<ErrorCodeCategory<String>> = LazyLock::new(|| {
    ErrorCodeCategory::new_federation_directive(
        "INVALID_FIELDS".to_owned(),
        Box::new(|directive| {
            format!(
                "The `fields` argument of a `@{directive}` directive is invalid (it has invalid syntax, includes unknown fields, ...)."
            )
        }),
        None,
    )
});

static KEY_INVALID_FIELDS: LazyLock<ErrorCodeDefinition> =
    LazyLock::new(|| DIRECTIVE_INVALID_FIELDS.create_code("key".to_owned()));
static PROVIDES_INVALID_FIELDS: LazyLock<ErrorCodeDefinition> =
    LazyLock::new(|| DIRECTIVE_INVALID_FIELDS.create_code("provides".to_owned()));
static REQUIRES_INVALID_FIELDS: LazyLock<ErrorCodeDefinition> =
    LazyLock::new(|| DIRECTIVE_INVALID_FIELDS.create_code("requires".to_owned()));

static KEY_FIELDS_SELECT_INVALID_TYPE: LazyLock<ErrorCodeDefinition> = LazyLock::new(|| {
    ErrorCodeDefinition::new(
        "KEY_FIELDS_SELECT_INVALID_TYPE".to_owned(),
        "The `fields` argument of `@key` directive includes a field whose type is a list, interface, or union type. Fields of these types cannot be part of a `@key`".to_owned(),
        Some(ErrorCodeMetadata {
            added_in: FED1_CODE,
            replaces: &[],
        }),
)
});

static ROOT_TYPE_USED: LazyLock<ErrorCodeCategory<SchemaRootKind>> = LazyLock::new(|| {
    ErrorCodeCategory::new(
        Box::new(|element| {
            let kind: String = element.into();
            format!("ROOT_{}_USED", kind.to_uppercase())
        }),
        Box::new(|element| {
            let kind: String = element.into();
            format!(
                "A subgraph's schema defines a type with the name `{kind}`, while also specifying a _different_ type name as the root query object. This is not allowed."
            )
        }),
        Some(ErrorCodeMetadata {
            added_in: FED1_CODE,
            replaces: &[],
        }),
    )
});

static ROOT_QUERY_USED: LazyLock<ErrorCodeDefinition> =
    LazyLock::new(|| ROOT_TYPE_USED.create_code(SchemaRootKind::Query));
static ROOT_MUTATION_USED: LazyLock<ErrorCodeDefinition> =
    LazyLock::new(|| ROOT_TYPE_USED.create_code(SchemaRootKind::Mutation));
static ROOT_SUBSCRIPTION_USED: LazyLock<ErrorCodeDefinition> =
    LazyLock::new(|| ROOT_TYPE_USED.create_code(SchemaRootKind::Subscription));

static INVALID_SUBGRAPH_NAME: LazyLock<ErrorCodeDefinition> = LazyLock::new(|| {
    ErrorCodeDefinition::new(
        "INVALID_SUBGRAPH_NAME".to_owned(),
        "A subgraph name is invalid (subgraph names cannot be a single underscore (\"_\"))."
            .to_owned(),
        None,
    )
});

static NO_QUERIES: LazyLock<ErrorCodeDefinition> = LazyLock::new(|| {
    ErrorCodeDefinition::new(
        "NO_QUERIES".to_owned(),
        "None of the composed subgraphs expose any query.".to_owned(),
        None,
    )
});

static INTERFACE_FIELD_NO_IMPLEM: LazyLock<ErrorCodeDefinition> = LazyLock::new(|| {
    ErrorCodeDefinition::new(
        "INTERFACE_FIELD_NO_IMPLEM".to_owned(),
        "After subgraph merging, an implementation is missing a field of one of the interface it implements (which can happen for valid subgraphs).".to_owned(),
        None,
    )
});

static TYPE_KIND_MISMATCH: LazyLock<ErrorCodeDefinition> = LazyLock::new(|| {
    ErrorCodeDefinition::new(
        "TYPE_KIND_MISMATCH".to_owned(),
        "A type has the same name in different subgraphs, but a different kind. For instance, one definition is an object type but another is an interface.".to_owned(),
        Some(ErrorCodeMetadata {
            replaces: &["VALUE_TYPE_KIND_MISMATCH", "EXTENSION_OF_WRONG_KIND", "ENUM_MISMATCH_TYPE"],
            ..DEFAULT_METADATA.clone()
        }),
    )
});

static EXTERNAL_TYPE_MISMATCH: LazyLock<ErrorCodeDefinition> = LazyLock::new(|| {
    ErrorCodeDefinition::new(
        "EXTERNAL_TYPE_MISMATCH".to_owned(),
        "An `@external` field has a type that is incompatible with the declaration(s) of that field in other subgraphs.".to_owned(),
        Some(ErrorCodeMetadata {
            added_in: FED1_CODE,
            replaces: &[],
        }),
    )
});

static EXTERNAL_COLLISION_WITH_ANOTHER_DIRECTIVE: LazyLock<ErrorCodeDefinition> =
    LazyLock::new(|| {
        ErrorCodeDefinition::new(
            "EXTERNAL_COLLISION_WITH_ANOTHER_DIRECTIVE".to_owned(),
            "The @external directive collides with other directives in some situations.".to_owned(),
            Some(ErrorCodeMetadata {
                added_in: "2.1.0",
                replaces: &[],
            }),
        )
    });

static EXTERNAL_ARGUMENT_MISSING: LazyLock<ErrorCodeDefinition> = LazyLock::new(|| {
    ErrorCodeDefinition::new(
        "EXTERNAL_ARGUMENT_MISSING".to_owned(),
        "An `@external` field is missing some arguments present in the declaration(s) of that field in other subgraphs.".to_owned(),
        None,
    )
});

static EXTERNAL_ARGUMENT_TYPE_MISMATCH: LazyLock<ErrorCodeDefinition> = LazyLock::new(|| {
    ErrorCodeDefinition::new(
        "EXTERNAL_ARGUMENT_TYPE_MISMATCH".to_owned(),
        "An `@external` field declares an argument with a type that is incompatible with the corresponding argument in the declaration(s) of that field in other subgraphs.".to_owned(),
        None,
    )
});

static EXTERNAL_ARGUMENT_DEFAULT_MISMATCH: LazyLock<ErrorCodeDefinition> = LazyLock::new(|| {
    ErrorCodeDefinition::new(
        "EXTERNAL_ARGUMENT_DEFAULT_MISMATCH".to_owned(),
        "An `@external` field declares an argument with a default that is incompatible with the corresponding argument in the declaration(s) of that field in other subgraphs.".to_owned(),
        None,
    )
});

static EXTERNAL_ON_INTERFACE: LazyLock<ErrorCodeDefinition> = LazyLock::new(|| {
    ErrorCodeDefinition::new(
        "EXTERNAL_ON_INTERFACE".to_owned(),
        "The field of an interface type is marked with `@external`: as external is about marking field not resolved by the subgraph and as interface field are not resolved (only implementations of those fields are), an \"external\" interface field is nonsensical".to_owned(),
        None,
    )
});

static MERGED_DIRECTIVE_APPLICATION_ON_EXTERNAL: LazyLock<ErrorCodeDefinition> = LazyLock::new(
    || {
        ErrorCodeDefinition::new(
        "MERGED_DIRECTIVE_APPLICATION_ON_EXTERNAL".to_owned(),
        "In a subgraph, a field is both marked @external and has a merged directive applied to it".to_owned(),
        None,
    )
    },
);

static FIELD_TYPE_MISMATCH: LazyLock<ErrorCodeDefinition> = LazyLock::new(|| {
    ErrorCodeDefinition::new(
        "FIELD_TYPE_MISMATCH".to_owned(),
        "A field has a type that is incompatible with other declarations of that field in other subgraphs.".to_owned(),
        Some(ErrorCodeMetadata {
            replaces: &["VALUE_TYPE_FIELD_TYPE_MISMATCH"],
            ..DEFAULT_METADATA.clone()
        }),
    )
});

static FIELD_ARGUMENT_TYPE_MISMATCH: LazyLock<ErrorCodeDefinition> = LazyLock::new(|| {
    ErrorCodeDefinition::new(
        "FIELD_ARGUMENT_TYPE_MISMATCH".to_owned(),
        "An argument (of a field/directive) has a type that is incompatible with that of other declarations of that same argument in other subgraphs.".to_owned(),
        Some(ErrorCodeMetadata {
            replaces: &["VALUE_TYPE_INPUT_VALUE_MISMATCH"],
            ..DEFAULT_METADATA.clone()
        }),
    )
});

static INPUT_FIELD_DEFAULT_MISMATCH: LazyLock<ErrorCodeDefinition> = LazyLock::new(|| {
    ErrorCodeDefinition::new(
        "INPUT_FIELD_DEFAULT_MISMATCH".to_owned(),
        "An input field has a default value that is incompatible with other declarations of that field in other subgraphs.".to_owned(),
        None,
    )
});

static FIELD_ARGUMENT_DEFAULT_MISMATCH: LazyLock<ErrorCodeDefinition> = LazyLock::new(|| {
    ErrorCodeDefinition::new(
        "FIELD_ARGUMENT_DEFAULT_MISMATCH".to_owned(),
        "An argument (of a field/directive) has a default value that is incompatible with that of other declarations of that same argument in other subgraphs.".to_owned(),
        None,
    )
});

static EXTENSION_WITH_NO_BASE: LazyLock<ErrorCodeDefinition> = LazyLock::new(|| {
    ErrorCodeDefinition::new(
        "EXTENSION_WITH_NO_BASE".to_owned(),
        "A subgraph is attempting to `extend` a type that is not originally defined in any known subgraph.".to_owned(),
        Some(ErrorCodeMetadata {
            added_in: FED1_CODE,
            replaces: &[],
        }),
    )
});

static EXTERNAL_MISSING_ON_BASE: LazyLock<ErrorCodeDefinition> = LazyLock::new(|| {
    ErrorCodeDefinition::new(
        "EXTERNAL_MISSING_ON_BASE".to_owned(),
        "A field is marked as `@external` in a subgraph but with no non-external declaration in any other subgraph.".to_owned(),
        Some(ErrorCodeMetadata {
            added_in: FED1_CODE,
            replaces: &[],
        }),
    )
});

static INVALID_FIELD_SHARING: LazyLock<ErrorCodeDefinition> = LazyLock::new(|| {
    ErrorCodeDefinition::new(
        "INVALID_FIELD_SHARING".to_owned(),
        "A field that is non-shareable in at least one subgraph is resolved by multiple subgraphs."
            .to_owned(),
        None,
    )
});

static INVALID_SHAREABLE_USAGE: LazyLock<ErrorCodeDefinition> = LazyLock::new(|| {
    ErrorCodeDefinition::new(
        "INVALID_SHAREABLE_USAGE".to_owned(),
        "The `@shareable` federation directive is used in an invalid way.".to_owned(),
        Some(ErrorCodeMetadata {
            added_in: "2.1.2",
            replaces: &[],
        }),
    )
});

static INVALID_LINK_DIRECTIVE_USAGE: LazyLock<ErrorCodeDefinition> = LazyLock::new(|| {
    ErrorCodeDefinition::new(
        "INVALID_LINK_DIRECTIVE_USAGE".to_owned(),
        "An application of the @link directive is invalid/does not respect the specification."
            .to_owned(),
        None,
    )
});

static INVALID_LINK_IDENTIFIER: LazyLock<ErrorCodeDefinition> = LazyLock::new(|| {
    ErrorCodeDefinition::new(
        "INVALID_LINK_IDENTIFIER".to_owned(),
        "A url/version for a @link feature is invalid/does not respect the specification."
            .to_owned(),
        Some(ErrorCodeMetadata {
            added_in: "2.1.0",
            replaces: &[],
        }),
    )
});

static LINK_IMPORT_NAME_MISMATCH: LazyLock<ErrorCodeDefinition> = LazyLock::new(|| {
    ErrorCodeDefinition::new(
        "LINK_IMPORT_NAME_MISMATCH".to_owned(),
        "The import name for a merged directive (as declared by the relevant `@link(import:)` argument) is inconsistent between subgraphs.".to_owned(),
        None,
    )
});

static REFERENCED_INACCESSIBLE: LazyLock<ErrorCodeDefinition> = LazyLock::new(|| {
    ErrorCodeDefinition::new(
        "REFERENCED_INACCESSIBLE".to_owned(),
        "An element is marked as @inaccessible but is referenced by an element visible in the API schema.".to_owned(),
        None,
    )
});

static DEFAULT_VALUE_USES_INACCESSIBLE: LazyLock<ErrorCodeDefinition> = LazyLock::new(|| {
    ErrorCodeDefinition::new(
        "DEFAULT_VALUE_USES_INACCESSIBLE".to_owned(),
        "An element is marked as @inaccessible but is used in the default value of an element visible in the API schema.".to_owned(),
        None,
    )
});

static QUERY_ROOT_TYPE_INACCESSIBLE: LazyLock<ErrorCodeDefinition> = LazyLock::new(|| {
    ErrorCodeDefinition::new(
        "QUERY_ROOT_TYPE_INACCESSIBLE".to_owned(),
        "An element is marked as @inaccessible but is the query root type, which must be visible in the API schema.".to_owned(),
        None,
    )
});

static REQUIRED_INACCESSIBLE: LazyLock<ErrorCodeDefinition> = LazyLock::new(|| {
    ErrorCodeDefinition::new(
        "REQUIRED_INACCESSIBLE".to_owned(),
        "An element is marked as @inaccessible but is required by an element visible in the API schema.".to_owned(),
        None,
    )
});

static IMPLEMENTED_BY_INACCESSIBLE: LazyLock<ErrorCodeDefinition> = LazyLock::new(|| {
    ErrorCodeDefinition::new(
        "IMPLEMENTED_BY_INACCESSIBLE".to_owned(),
        "An element is marked as @inaccessible but implements an element visible in the API schema.".to_owned(),
        None,
    )
});

static DISALLOWED_INACCESSIBLE: LazyLock<ErrorCodeDefinition> = LazyLock::new(|| {
    ErrorCodeDefinition::new(
        "DISALLOWED_INACCESSIBLE".to_owned(),
        "An element is marked as @inaccessible that is not allowed to be @inaccessible.".to_owned(),
        None,
    )
});

static ONLY_INACCESSIBLE_CHILDREN: LazyLock<ErrorCodeDefinition> = LazyLock::new(|| {
    ErrorCodeDefinition::new(
        "ONLY_INACCESSIBLE_CHILDREN".to_owned(),
        "A type visible in the API schema has only @inaccessible children.".to_owned(),
        None,
    )
});

static REQUIRED_INPUT_FIELD_MISSING_IN_SOME_SUBGRAPH: LazyLock<ErrorCodeDefinition> = LazyLock::new(
    || {
        ErrorCodeDefinition::new(
        "REQUIRED_INPUT_FIELD_MISSING_IN_SOME_SUBGRAPH".to_owned(),
        "A field of an input object type is mandatory in some subgraphs, but the field is not defined in all the subgraphs that define the input object type.".to_owned(),
        None,
    )
    },
);

static REQUIRED_ARGUMENT_MISSING_IN_SOME_SUBGRAPH: LazyLock<ErrorCodeDefinition> = LazyLock::new(
    || {
        ErrorCodeDefinition::new(
        "REQUIRED_ARGUMENT_MISSING_IN_SOME_SUBGRAPH".to_owned(),
        "An argument of a field or directive definition is mandatory in some subgraphs, but the argument is not defined in all the subgraphs that define the field or directive definition.".to_owned(),
        None,
    )
    },
);

static EMPTY_MERGED_INPUT_TYPE: LazyLock<ErrorCodeDefinition> = LazyLock::new(|| {
    ErrorCodeDefinition::new(
        "EMPTY_MERGED_INPUT_TYPE".to_owned(),
        "An input object type has no field common to all the subgraphs that define the type. Merging that type would result in an invalid empty input object type.".to_owned(),
        None,
    )
});

static INPUT_FIELD_MERGE_FAILED: LazyLock<ErrorCodeDefinition> = LazyLock::new(|| {
    ErrorCodeDefinition::new(
        "INPUT_FIELD_MERGE_FAILED".to_owned(),
        "Failed to merge an input object field due to incompatible definitions across subgraphs."
            .to_owned(),
        None,
    )
});

static ENUM_VALUE_MISMATCH: LazyLock<ErrorCodeDefinition> = LazyLock::new(|| {
    ErrorCodeDefinition::new(
        "ENUM_VALUE_MISMATCH".to_owned(),
        "An enum type that is used as both an input and output type has a value that is not defined in all the subgraphs that define the enum type.".to_owned(),
        None,
    )
});

static EMPTY_MERGED_ENUM_TYPE: LazyLock<ErrorCodeDefinition> = LazyLock::new(|| {
    ErrorCodeDefinition::new(
        "EMPTY_MERGED_ENUM_TYPE".to_owned(),
        "An enum type has no value common to all the subgraphs that define the type. Merging that type would result in an invalid empty enum type.".to_owned(),
        None,
    )
});

static SHAREABLE_HAS_MISMATCHED_RUNTIME_TYPES: LazyLock<ErrorCodeDefinition> = LazyLock::new(
    || {
        ErrorCodeDefinition::new(
        "SHAREABLE_HAS_MISMATCHED_RUNTIME_TYPES".to_owned(),
        "A shareable field return type has mismatched possible runtime types in the subgraphs in which the field is declared. As shared fields must resolve the same way in all subgraphs, this is almost surely a mistake.".to_owned(),
        None,
    )
    },
);

static SATISFIABILITY_ERROR: LazyLock<ErrorCodeDefinition> = LazyLock::new(|| {
    ErrorCodeDefinition::new(
        "SATISFIABILITY_ERROR".to_owned(),
        "Subgraphs can be merged, but the resulting supergraph API would have queries that cannot be satisfied by those subgraphs.".to_owned(),
        None,
    )
});

static MAX_VALIDATION_SUBGRAPH_PATHS_EXCEEDED: LazyLock<ErrorCodeDefinition> =
    LazyLock::new(|| {
        ErrorCodeDefinition::new(
            "MAX_VALIDATION_SUBGRAPH_PATHS_EXCEEDED".to_owned(),
            "The maximum number of validation subgraph paths has been exceeded.".to_owned(),
            Some(ErrorCodeMetadata {
                added_in: "2.8.0",
                replaces: &[],
            }),
        )
    });

static OVERRIDE_FROM_SELF_ERROR: LazyLock<ErrorCodeDefinition> = LazyLock::new(|| {
    ErrorCodeDefinition::new(
        "OVERRIDE_FROM_SELF_ERROR".to_owned(),
        "Field with `@override` directive has \"from\" location that references its own subgraph."
            .to_owned(),
        None,
    )
});

static OVERRIDE_SOURCE_HAS_OVERRIDE: LazyLock<ErrorCodeDefinition> = LazyLock::new(|| {
    ErrorCodeDefinition::new(
        "OVERRIDE_SOURCE_HAS_OVERRIDE".to_owned(),
        "Field which is overridden to another subgraph is also marked @override.".to_owned(),
        None,
    )
});

static OVERRIDE_COLLISION_WITH_ANOTHER_DIRECTIVE: LazyLock<ErrorCodeDefinition> = LazyLock::new(
    || {
        ErrorCodeDefinition::new(
        "OVERRIDE_COLLISION_WITH_ANOTHER_DIRECTIVE".to_owned(),
        "The @override directive cannot be used on external fields, nor to override fields with either @external, @provides, or @requires.".to_owned(),
        None,
    )
    },
);

static OVERRIDE_ON_INTERFACE: LazyLock<ErrorCodeDefinition> = LazyLock::new(|| {
    ErrorCodeDefinition::new(
        "OVERRIDE_ON_INTERFACE".to_owned(),
        "The @override directive cannot be used on the fields of an interface type.".to_owned(),
        Some(ErrorCodeMetadata {
            added_in: "2.3.0",
            replaces: &[],
        }),
    )
});

static OVERRIDE_LABEL_INVALID: LazyLock<ErrorCodeDefinition> = LazyLock::new(|| {
    ErrorCodeDefinition::new(
        "OVERRIDE_LABEL_INVALID".to_owned(),
        r#"The @override directive `label` argument must match the pattern /^[a-zA-Z][a-zA-Z0-9_\-:./]*$/ or /^percent\((\d{1,2}(\.\d{1,8})?|100)\)$/"#.to_owned(),
        Some(ErrorCodeMetadata {
            added_in: "2.7.0",
            replaces: &[],
        }),
    )
});

static UNSUPPORTED_FEATURE: LazyLock<ErrorCodeDefinition> = LazyLock::new(|| {
    ErrorCodeDefinition::new(
        "UNSUPPORTED_FEATURE".to_owned(),
        "Indicates an error due to feature currently unsupported by federation.".to_owned(),
        Some(ErrorCodeMetadata {
            added_in: "2.1.0",
            replaces: &[],
        }),
    )
});

static INVALID_FEDERATION_SUPERGRAPH: LazyLock<ErrorCodeDefinition> = LazyLock::new(|| {
    ErrorCodeDefinition::new(
        "INVALID_FEDERATION_SUPERGRAPH".to_owned(),
        "Indicates that a schema provided for an Apollo Federation supergraph is not a valid supergraph schema.".to_owned(),
        Some(ErrorCodeMetadata {
            added_in: "2.1.0",
            replaces: &[],
        }),
    )
});

static DOWNSTREAM_SERVICE_ERROR: LazyLock<ErrorCodeDefinition> = LazyLock::new(|| {
    ErrorCodeDefinition::new(
        "DOWNSTREAM_SERVICE_ERROR".to_owned(),
        "Indicates an error in a subgraph service query during query execution in a federated service.".to_owned(),
        Some(ErrorCodeMetadata {
            added_in: FED1_CODE,
            replaces: &[],
        }),
    )
});

static DIRECTIVE_COMPOSITION_ERROR: LazyLock<ErrorCodeDefinition> = LazyLock::new(|| {
    ErrorCodeDefinition::new(
        "DIRECTIVE_COMPOSITION_ERROR".to_owned(),
        "Error when composing custom directives.".to_owned(),
        Some(ErrorCodeMetadata {
            added_in: "2.1.0",
            replaces: &[],
        }),
    )
});

static INTERFACE_OBJECT_USAGE_ERROR: LazyLock<ErrorCodeDefinition> = LazyLock::new(|| {
    ErrorCodeDefinition::new(
        "INTERFACE_OBJECT_USAGE_ERROR".to_owned(),
        "Error in the usage of the @interfaceObject directive.".to_owned(),
        Some(ErrorCodeMetadata {
            added_in: "2.3.0",
            replaces: &[],
        }),
    )
});

static INTERFACE_KEY_NOT_ON_IMPLEMENTATION: LazyLock<ErrorCodeDefinition> = LazyLock::new(|| {
    ErrorCodeDefinition::new(
        "INTERFACE_KEY_NOT_ON_IMPLEMENTATION".to_owned(),
        "A `@key` is defined on an interface type, but is not defined (or is not resolvable) on at least one of the interface implementations".to_owned(),
        Some(ErrorCodeMetadata {
            added_in: "2.3.0",
            replaces: &[],
        }),
    )
});

static INTERFACE_KEY_MISSING_IMPLEMENTATION_TYPE: LazyLock<ErrorCodeDefinition> = LazyLock::new(
    || {
        ErrorCodeDefinition::new(
        "INTERFACE_KEY_MISSING_IMPLEMENTATION_TYPE".to_owned(),
        "A subgraph has a `@key` on an interface type, but that subgraph does not define an implementation (in the supergraph) of that interface".to_owned(),
        Some(ErrorCodeMetadata {
            added_in: "2.3.0",
            replaces: &[],
        }),
    )
    },
);

static INTERNAL: LazyLock<ErrorCodeDefinition> = LazyLock::new(|| {
    ErrorCodeDefinition::new(
        "INTERNAL".to_owned(),
        "An internal federation error occured.".to_owned(),
        None,
    )
});

static ERROR_CODE_MISSING: LazyLock<ErrorCodeDefinition> = LazyLock::new(|| {
    ErrorCodeDefinition::new(
        "ERROR_CODE_MISSING".to_owned(),
        "An internal federation error occurred when translating a federation error into an error code".to_owned(),
        None,
    )
});

static UNSUPPORTED_FEDERATION_VERSION: LazyLock<ErrorCodeDefinition> = LazyLock::new(|| {
    ErrorCodeDefinition::new(
        "UNSUPPORTED_FEDERATION_VERSION".to_owned(),
        "Supergraphs composed with federation version 1 are not supported. Please recompose your supergraph with federation version 2 or greater".to_owned(),
        None,
    )
});

static UNSUPPORTED_FEDERATION_DIRECTIVE: LazyLock<ErrorCodeDefinition> = LazyLock::new(|| {
    ErrorCodeDefinition::new(
        "UNSUPPORTED_FEDERATION_DIRECTIVE".to_owned(),
        "Indicates that the specified specification version is outside of supported range"
            .to_owned(),
        None,
    )
});

static QUERY_PLAN_COMPLEXITY_EXCEEDED: LazyLock<ErrorCodeDefinition> = LazyLock::new(|| {
    ErrorCodeDefinition::new(
        "QUERY_PLAN_COMPLEXITY_EXCEEDED".to_owned(),
        "Indicates that provided query has too many possible ways to generate a plan and cannot be planned in a reasonable amount of time"
            .to_owned(),
        None,
    )
});

static NO_PLAN_FOUND_WITH_DISABLED_SUBGRAPHS: LazyLock<ErrorCodeDefinition> = LazyLock::new(|| {
    ErrorCodeDefinition::new(
        "NO_PLAN_FOUND_WITH_DISABLED_SUBGRAPHS".to_owned(),
        "Indicates that the provided query could not be query planned due to subgraphs being disabled"
            .to_owned(),
        None,
    )
});

static COST_APPLIED_TO_INTERFACE_FIELD: LazyLock<ErrorCodeDefinition> = LazyLock::new(|| {
    ErrorCodeDefinition::new(
        "COST_APPLIED_TO_INTERFACE_FIELD".to_owned(),
        "The `@cost` directive must be applied to concrete types".to_owned(),
        Some(ErrorCodeMetadata {
            added_in: "2.9.2",
            replaces: &[],
        }),
    )
});

static LIST_SIZE_APPLIED_TO_NON_LIST: LazyLock<ErrorCodeDefinition> = LazyLock::new(|| {
    ErrorCodeDefinition::new(
        "LIST_SIZE_APPLIED_TO_NON_LIST".to_owned(),
        "The `@listSize` directive must be applied to list types".to_owned(),
        Some(ErrorCodeMetadata {
            added_in: "2.9.2",
            replaces: &[],
        }),
    )
});

static LIST_SIZE_INVALID_ASSUMED_SIZE: LazyLock<ErrorCodeDefinition> = LazyLock::new(|| {
    ErrorCodeDefinition::new(
        "LIST_SIZE_INVALID_ASSUMED_SIZE".to_owned(),
        "The `@listSize` directive assumed size cannot be negative".to_owned(),
        Some(ErrorCodeMetadata {
            added_in: "2.9.2",
            replaces: &[],
        }),
    )
});

static LIST_SIZE_INVALID_SLICING_ARGUMENT: LazyLock<ErrorCodeDefinition> = LazyLock::new(|| {
    ErrorCodeDefinition::new(
        "LIST_SIZE_INVALID_SLICING_ARGUMENT".to_owned(),
        "The `@listSize` directive must have existing integer slicing arguments".to_owned(),
        Some(ErrorCodeMetadata {
            added_in: "2.9.2",
            replaces: &[],
        }),
    )
});

static LIST_SIZE_INVALID_SIZED_FIELD: LazyLock<ErrorCodeDefinition> = LazyLock::new(|| {
    ErrorCodeDefinition::new(
        "LIST_SIZE_INVALID_SIZED_FIELD".to_owned(),
        "The `@listSize` directive must reference existing list fields as sized fields".to_owned(),
        Some(ErrorCodeMetadata {
            added_in: "2.9.2",
            replaces: &[],
        }),
    )
});

static CONTEXT_NAME_CONTAINS_UNDERSCORE: LazyLock<ErrorCodeDefinition> = LazyLock::new(|| {
    ErrorCodeDefinition::new(
        "CONTEXT_NAME_CONTAINS_UNDERSCORE".to_owned(),
        "Context name is invalid.".to_owned(),
        Some(ErrorCodeMetadata {
            added_in: "2.8.0",
            replaces: &[],
        }),
    )
});

static CONTEXT_NAME_INVALID: LazyLock<ErrorCodeDefinition> = LazyLock::new(|| {
    ErrorCodeDefinition::new(
        "CONTEXT_NAME_INVALID".to_owned(),
        "Context name is invalid.".to_owned(),
        Some(ErrorCodeMetadata {
            added_in: "2.8.0",
            replaces: &[],
        }),
    )
});

static CONTEXT_NOT_SET: LazyLock<ErrorCodeDefinition> = LazyLock::new(|| {
    ErrorCodeDefinition::new(
        "CONTEXT_NOT_SET".to_owned(),
        "Context is never set for context trying to be used".to_owned(),
        Some(ErrorCodeMetadata {
            added_in: "2.8.0",
            replaces: &[],
        }),
    )
});

static NO_CONTEXT_REFERENCED: LazyLock<ErrorCodeDefinition> = LazyLock::new(|| {
    ErrorCodeDefinition::new(
        "NO_CONTEXT_REFERENCED".to_owned(),
        "Selection in @fromContext field argument does not reference a context".to_owned(),
        Some(ErrorCodeMetadata {
            added_in: "2.8.0",
            replaces: &[],
        }),
    )
});

static NO_SELECTION_FOR_CONTEXT: LazyLock<ErrorCodeDefinition> = LazyLock::new(|| {
    ErrorCodeDefinition::new(
        "NO_SELECTION_FOR_CONTEXT".to_owned(),
        "field parameter in @fromContext must contain a selection set".to_owned(),
        Some(ErrorCodeMetadata {
            added_in: "2.8.0",
            replaces: &[],
        }),
    )
});

static CONTEXT_NO_RESOLVABLE_KEY: LazyLock<ErrorCodeDefinition> = LazyLock::new(|| {
    ErrorCodeDefinition::new(
        "CONTEXT_NO_RESOLVABLE_KEY".to_owned(),
        "If an ObjectType uses a @fromContext, at least one of its keys must be resolvable"
            .to_owned(),
        Some(ErrorCodeMetadata {
            added_in: "2.8.0",
            replaces: &[],
        }),
    )
});

static CONTEXT_SELECTION_INVALID: LazyLock<ErrorCodeDefinition> = LazyLock::new(|| {
    ErrorCodeDefinition::new(
        "CONTEXT_SELECTION_INVALID".to_owned(),
        "The selection set is invalid".to_owned(),
        Some(ErrorCodeMetadata {
            added_in: "2.8.0",
            replaces: &[],
        }),
    )
});

static INVALID_TAG_NAME: LazyLock<ErrorCodeDefinition> = LazyLock::new(|| {
    ErrorCodeDefinition::new(
        "INVALID_TAG_NAME".to_owned(),
        "Invalid value for argument \"name\" in application of @tag.".to_owned(),
        Some(ErrorCodeMetadata {
            added_in: "2.0.0",
            replaces: &[],
        }),
    )
});

static CONTEXTUAL_ARGUMENT_NOT_CONTEXTUAL_IN_ALL_SUBGRAPHS: LazyLock<ErrorCodeDefinition> =
    LazyLock::new(|| {
        ErrorCodeDefinition::new(
            "CONTEXTUAL_ARGUMENT_NOT_CONTEXTUAL_IN_ALL_SUBGRAPHS".to_owned(),
            "Argument on field is marked contextual in only some subgraphs".to_owned(),
            Some(ErrorCodeMetadata {
                added_in: "2.7.0",
                replaces: &[],
            }),
        )
    });

static QUERY_ROOT_MISSING: LazyLock<ErrorCodeDefinition> = LazyLock::new(|| {
    ErrorCodeDefinition::new(
        "QUERY_ROOT_MISSING".to_owned(),
        "The schema has no query root type.".to_owned(),
        Some(ErrorCodeMetadata {
            added_in: "2.0.0",
            replaces: &[],
        }),
    )
});

#[derive(Debug, PartialEq, strum_macros::EnumIter)]
pub enum ErrorCode {
    ErrorCodeMissing,
    Internal,
    ExtensionWithNoBase,
    InvalidGraphQL,
    DirectiveDefinitionInvalid,
    TypeDefinitionInvalid,
    UnsupportedLinkedFeature,
    UnknownFederationLinkVersion,
    UnknownLinkVersion,
    KeyFieldsHasArgs,
    ProvidesFieldsHasArgs,
    ProvidesFieldsMissingExternal,
    RequiresFieldsMissingExternal,
    KeyUnsupportedOnInterface,
    ProvidesUnsupportedOnInterface,
    RequiresUnsupportedOnInterface,
    KeyDirectiveInFieldsArgs,
    ProvidesDirectiveInFieldsArgs,
    RequiresDirectiveInFieldsArgs,
    ExternalUnused,
    TypeWithOnlyUnusedExternal,
    ProvidesOnNonObjectField,
    KeyInvalidFieldsType,
    ProvidesInvalidFieldsType,
    RequiresInvalidFieldsType,
    KeyInvalidFields,
    ProvidesInvalidFields,
    RequiresInvalidFields,
    KeyFieldsSelectInvalidType,
    RootQueryUsed,
    RootMutationUsed,
    RootSubscriptionUsed,
    InvalidSubgraphName,
    NoQueries,
    InterfaceFieldNoImplem,
    TypeKindMismatch,
    ExternalTypeMismatch,
    ExternalCollisionWithAnotherDirective,
    ExternalArgumentMissing,
    ExternalArgumentTypeMismatch,
    ExternalArgumentDefaultMismatch,
    ExternalOnInterface,
    MergedDirectiveApplicationOnExternal,
    FieldTypeMismatch,
    FieldArgumentTypeMismatch,
    InputFieldDefaultMismatch,
    FieldArgumentDefaultMismatch,
    ExternalMissingOnBase,
    InvalidFieldSharing,
    InvalidShareableUsage,
    InvalidLinkDirectiveUsage,
    InvalidLinkIdentifier,
    LinkImportNameMismatch,
    ReferencedInaccessible,
    DefaultValueUsesInaccessible,
    QueryRootTypeInaccessible,
    RequiredInaccessible,
    ImplementedByInaccessible,
    DisallowedInaccessible,
    OnlyInaccessibleChildren,
    RequiredInputFieldMissingInSomeSubgraph,
    RequiredArgumentMissingInSomeSubgraph,
    EmptyMergedInputType,
    InputFieldMergeFailed,
    EnumValueMismatch,
    EmptyMergedEnumType,
    ShareableHasMismatchedRuntimeTypes,
    SatisfiabilityError,
    MaxValidationSubgraphPathsExceeded,
    OverrideFromSelfError,
    OverrideSourceHasOverride,
    OverrideCollisionWithAnotherDirective,
    OverrideOnInterface,
    UnsupportedFeature,
    InvalidFederationSupergraph,
    DownstreamServiceError,
    DirectiveCompositionError,
    InterfaceObjectUsageError,
    InterfaceKeyNotOnImplementation,
    InterfaceKeyMissingImplementationType,
    UnsupportedFederationVersion,
    UnsupportedFederationDirective,
    QueryPlanComplexityExceededError,
    NoPlanFoundWithDisabledSubgraphs,
    CostAppliedToInterfaceField,
    ListSizeAppliedToNonList,
    ListSizeInvalidAssumedSize,
    ListSizeInvalidSlicingArgument,
    ListSizeInvalidSizedField,
    ContextNameInvalid,
    ContextNameContainsUnderscore,
    ContextNotSet,
    NoContextReferenced,
    NoSelectionForContext,
    ContextNoResolvableKey,
    ContextSelectionInvalid,
    InvalidTagName,
    OverrideLabelInvalid,
    ContextualArgumentNotContextualInAllSubgraphs,
    QueryRootMissing,
}

impl ErrorCode {
    pub fn definition(&self) -> &'static ErrorCodeDefinition {
        match self {
            ErrorCode::Internal => &INTERNAL,
            ErrorCode::ExtensionWithNoBase => &EXTENSION_WITH_NO_BASE,
            ErrorCode::InvalidGraphQL => &INVALID_GRAPHQL,
            ErrorCode::DirectiveDefinitionInvalid => &DIRECTIVE_DEFINITION_INVALID,
            ErrorCode::TypeDefinitionInvalid => &TYPE_DEFINITION_INVALID,
            ErrorCode::UnsupportedLinkedFeature => &UNSUPPORTED_LINKED_FEATURE,
            ErrorCode::UnknownFederationLinkVersion => &UNKNOWN_FEDERATION_LINK_VERSION,
            ErrorCode::UnknownLinkVersion => &UNKNOWN_LINK_VERSION,
            ErrorCode::KeyFieldsHasArgs => &KEY_FIELDS_HAS_ARGS,
            ErrorCode::ProvidesFieldsHasArgs => &PROVIDES_FIELDS_HAS_ARGS,
            ErrorCode::ProvidesFieldsMissingExternal => &PROVIDES_FIELDS_MISSING_EXTERNAL,
            ErrorCode::RequiresFieldsMissingExternal => &REQUIRES_FIELDS_MISSING_EXTERNAL,
            ErrorCode::KeyUnsupportedOnInterface => &KEY_UNSUPPORTED_ON_INTERFACE,
            ErrorCode::ProvidesUnsupportedOnInterface => &PROVIDES_UNSUPPORTED_ON_INTERFACE,
            ErrorCode::RequiresUnsupportedOnInterface => &REQUIRES_UNSUPPORTED_ON_INTERFACE,
            ErrorCode::KeyDirectiveInFieldsArgs => &KEY_DIRECTIVE_IN_FIELDS_ARGS,
            ErrorCode::ProvidesDirectiveInFieldsArgs => &PROVIDES_DIRECTIVE_IN_FIELDS_ARGS,
            ErrorCode::RequiresDirectiveInFieldsArgs => &REQUIRES_DIRECTIVE_IN_FIELDS_ARGS,
            ErrorCode::ExternalUnused => &EXTERNAL_UNUSED,
            ErrorCode::ExternalCollisionWithAnotherDirective => {
                &EXTERNAL_COLLISION_WITH_ANOTHER_DIRECTIVE
            }
            ErrorCode::TypeWithOnlyUnusedExternal => &TYPE_WITH_ONLY_UNUSED_EXTERNAL,
            ErrorCode::ProvidesOnNonObjectField => &PROVIDES_ON_NON_OBJECT_FIELD,
            ErrorCode::KeyInvalidFieldsType => &KEY_INVALID_FIELDS_TYPE,
            ErrorCode::ProvidesInvalidFieldsType => &PROVIDES_INVALID_FIELDS_TYPE,
            ErrorCode::RequiresInvalidFieldsType => &REQUIRES_INVALID_FIELDS_TYPE,
            ErrorCode::KeyInvalidFields => &KEY_INVALID_FIELDS,
            ErrorCode::ProvidesInvalidFields => &PROVIDES_INVALID_FIELDS,
            ErrorCode::RequiresInvalidFields => &REQUIRES_INVALID_FIELDS,
            ErrorCode::KeyFieldsSelectInvalidType => &KEY_FIELDS_SELECT_INVALID_TYPE,
            ErrorCode::RootQueryUsed => &ROOT_QUERY_USED,
            ErrorCode::RootMutationUsed => &ROOT_MUTATION_USED,
            ErrorCode::RootSubscriptionUsed => &ROOT_SUBSCRIPTION_USED,
            ErrorCode::InvalidSubgraphName => &INVALID_SUBGRAPH_NAME,
            ErrorCode::NoQueries => &NO_QUERIES,
            ErrorCode::InterfaceFieldNoImplem => &INTERFACE_FIELD_NO_IMPLEM,
            ErrorCode::TypeKindMismatch => &TYPE_KIND_MISMATCH,
            ErrorCode::ExternalTypeMismatch => &EXTERNAL_TYPE_MISMATCH,
            ErrorCode::ExternalArgumentMissing => &EXTERNAL_ARGUMENT_MISSING,
            ErrorCode::ExternalArgumentTypeMismatch => &EXTERNAL_ARGUMENT_TYPE_MISMATCH,
            ErrorCode::ExternalArgumentDefaultMismatch => &EXTERNAL_ARGUMENT_DEFAULT_MISMATCH,
            ErrorCode::ExternalOnInterface => &EXTERNAL_ON_INTERFACE,
            ErrorCode::MergedDirectiveApplicationOnExternal => {
                &MERGED_DIRECTIVE_APPLICATION_ON_EXTERNAL
            }
            ErrorCode::FieldTypeMismatch => &FIELD_TYPE_MISMATCH,
            ErrorCode::FieldArgumentTypeMismatch => &FIELD_ARGUMENT_TYPE_MISMATCH,
            ErrorCode::InputFieldDefaultMismatch => &INPUT_FIELD_DEFAULT_MISMATCH,
            ErrorCode::FieldArgumentDefaultMismatch => &FIELD_ARGUMENT_DEFAULT_MISMATCH,
            ErrorCode::ExternalMissingOnBase => &EXTERNAL_MISSING_ON_BASE,
            ErrorCode::InvalidFieldSharing => &INVALID_FIELD_SHARING,
            ErrorCode::InvalidShareableUsage => &INVALID_SHAREABLE_USAGE,
            ErrorCode::InvalidLinkDirectiveUsage => &INVALID_LINK_DIRECTIVE_USAGE,
            ErrorCode::InvalidLinkIdentifier => &INVALID_LINK_IDENTIFIER,
            ErrorCode::LinkImportNameMismatch => &LINK_IMPORT_NAME_MISMATCH,
            ErrorCode::ReferencedInaccessible => &REFERENCED_INACCESSIBLE,
            ErrorCode::DefaultValueUsesInaccessible => &DEFAULT_VALUE_USES_INACCESSIBLE,
            ErrorCode::QueryRootTypeInaccessible => &QUERY_ROOT_TYPE_INACCESSIBLE,
            ErrorCode::RequiredInaccessible => &REQUIRED_INACCESSIBLE,
            ErrorCode::ImplementedByInaccessible => &IMPLEMENTED_BY_INACCESSIBLE,
            ErrorCode::DisallowedInaccessible => &DISALLOWED_INACCESSIBLE,
            ErrorCode::OnlyInaccessibleChildren => &ONLY_INACCESSIBLE_CHILDREN,
            ErrorCode::RequiredInputFieldMissingInSomeSubgraph => {
                &REQUIRED_INPUT_FIELD_MISSING_IN_SOME_SUBGRAPH
            }
            ErrorCode::RequiredArgumentMissingInSomeSubgraph => {
                &REQUIRED_ARGUMENT_MISSING_IN_SOME_SUBGRAPH
            }
            ErrorCode::EmptyMergedInputType => &EMPTY_MERGED_INPUT_TYPE,
            ErrorCode::InputFieldMergeFailed => &INPUT_FIELD_MERGE_FAILED,
            ErrorCode::EnumValueMismatch => &ENUM_VALUE_MISMATCH,
            ErrorCode::EmptyMergedEnumType => &EMPTY_MERGED_ENUM_TYPE,
            ErrorCode::ShareableHasMismatchedRuntimeTypes => {
                &SHAREABLE_HAS_MISMATCHED_RUNTIME_TYPES
            }
            ErrorCode::SatisfiabilityError => &SATISFIABILITY_ERROR,
            ErrorCode::MaxValidationSubgraphPathsExceeded => {
                &MAX_VALIDATION_SUBGRAPH_PATHS_EXCEEDED
            }
            ErrorCode::OverrideFromSelfError => &OVERRIDE_FROM_SELF_ERROR,
            ErrorCode::OverrideSourceHasOverride => &OVERRIDE_SOURCE_HAS_OVERRIDE,
            ErrorCode::OverrideCollisionWithAnotherDirective => {
                &OVERRIDE_COLLISION_WITH_ANOTHER_DIRECTIVE
            }
            ErrorCode::OverrideOnInterface => &OVERRIDE_ON_INTERFACE,
            ErrorCode::UnsupportedFeature => &UNSUPPORTED_FEATURE,
            ErrorCode::InvalidFederationSupergraph => &INVALID_FEDERATION_SUPERGRAPH,
            ErrorCode::DownstreamServiceError => &DOWNSTREAM_SERVICE_ERROR,
            ErrorCode::DirectiveCompositionError => &DIRECTIVE_COMPOSITION_ERROR,
            ErrorCode::InterfaceObjectUsageError => &INTERFACE_OBJECT_USAGE_ERROR,
            ErrorCode::InterfaceKeyNotOnImplementation => &INTERFACE_KEY_NOT_ON_IMPLEMENTATION,
            ErrorCode::InterfaceKeyMissingImplementationType => {
                &INTERFACE_KEY_MISSING_IMPLEMENTATION_TYPE
            }
            ErrorCode::UnsupportedFederationVersion => &UNSUPPORTED_FEDERATION_VERSION,
            ErrorCode::UnsupportedFederationDirective => &UNSUPPORTED_FEDERATION_DIRECTIVE,
            ErrorCode::QueryPlanComplexityExceededError => &QUERY_PLAN_COMPLEXITY_EXCEEDED,
            ErrorCode::NoPlanFoundWithDisabledSubgraphs => &NO_PLAN_FOUND_WITH_DISABLED_SUBGRAPHS,
            ErrorCode::CostAppliedToInterfaceField => &COST_APPLIED_TO_INTERFACE_FIELD,
            ErrorCode::ListSizeAppliedToNonList => &LIST_SIZE_APPLIED_TO_NON_LIST,
            ErrorCode::ListSizeInvalidAssumedSize => &LIST_SIZE_INVALID_ASSUMED_SIZE,
            ErrorCode::ListSizeInvalidSlicingArgument => &LIST_SIZE_INVALID_SLICING_ARGUMENT,
            ErrorCode::ListSizeInvalidSizedField => &LIST_SIZE_INVALID_SIZED_FIELD,
            ErrorCode::ContextNameContainsUnderscore => &CONTEXT_NAME_CONTAINS_UNDERSCORE,
            ErrorCode::ContextNameInvalid => &CONTEXT_NAME_INVALID,
            ErrorCode::ContextNotSet => &CONTEXT_NOT_SET,
            ErrorCode::NoContextReferenced => &NO_CONTEXT_REFERENCED,
            ErrorCode::NoSelectionForContext => &NO_SELECTION_FOR_CONTEXT,
            ErrorCode::ContextNoResolvableKey => &CONTEXT_NO_RESOLVABLE_KEY,
            ErrorCode::ContextSelectionInvalid => &CONTEXT_SELECTION_INVALID,
            ErrorCode::InvalidTagName => &INVALID_TAG_NAME,
            ErrorCode::ErrorCodeMissing => &ERROR_CODE_MISSING,
            ErrorCode::OverrideLabelInvalid => &OVERRIDE_LABEL_INVALID,
            ErrorCode::ContextualArgumentNotContextualInAllSubgraphs => {
                &CONTEXTUAL_ARGUMENT_NOT_CONTEXTUAL_IN_ALL_SUBGRAPHS
            }
            ErrorCode::QueryRootMissing => &QUERY_ROOT_MISSING,
        }
    }
}
