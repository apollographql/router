use crate::error::FederationError;

/// Pipeline stage an error originates from, reported as the build message `step`.
///
/// The strings are the JS implementation's `FailedStep` values, so build messages keep the
/// same `step` whichever implementation produced them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FilterStep {
    Configuration,
    ParseSchema,
    AddDirectiveDefinitions,
    TagInheriting,
    TagMatching,
    EmptyEnumMasking,
    EmptyInputObjectMasking,
    EmptyObjectAndInterfaceFieldMasking,
    PartialInterfaceMasking,
    EmptyObjectAndInterfaceMasking,
    EmptyUnionMasking,
    UnreachableTypeMasking,
    /// Re-checking the filtered supergraph. JS reports this as `PARSING` too, telling it
    /// apart from parsing the input only by the message.
    ValidateFilteredSchema,
}

impl FilterStep {
    fn as_str(self) -> &'static str {
        match self {
            Self::Configuration => "INPUT_VALIDATION",
            Self::ParseSchema | Self::ValidateFilteredSchema => "PARSING",
            Self::AddDirectiveDefinitions => "ADD_DIRECTIVE_DEFINITIONS_IF_NOT_PRESENT",
            Self::TagInheriting => "TAG_INHERITING",
            Self::TagMatching => "TAG_MATCHING",
            Self::EmptyEnumMasking => "EMPTY_ENUM_MASKING",
            Self::EmptyInputObjectMasking => "EMPTY_INPUT_OBJECT_MASKING",
            Self::EmptyObjectAndInterfaceFieldMasking => "EMPTY_OBJECT_AND_INTERFACE_FIELD_MASKING",
            Self::PartialInterfaceMasking => "PARTIAL_INTERFACE_MASKING",
            Self::EmptyObjectAndInterfaceMasking => "EMPTY_OBJECT_AND_INTERFACE_MASKING",
            Self::EmptyUnionMasking => "EMPTY_UNION_MASKING",
            Self::UnreachableTypeMasking => "UNREACHABLE_TYPE_MASKING",
        }
    }
}

impl std::fmt::Display for FilterStep {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A `@tag`/`@inaccessible` directive definition that the schema declares but the
/// filtering pipeline cannot work with.
#[derive(Debug, thiserror::Error)]
pub enum DirectiveError {
    #[error("Unsupported @{spec} spec version \"{version}\"")]
    UnsupportedVersion { spec: &'static str, version: String },
    #[error("Schema does not have {spec} spec")]
    MissingSpec { spec: &'static str },
    #[error(transparent)]
    Federation(#[from] FederationError),
}

/// Anything that stops a supergraph schema from being transformed into a contract variant.
#[derive(Debug, thiserror::Error)]
pub enum TransformError {
    #[error("Include and exclude filters cannot overlap: {}", .0.join(", "))]
    OverlappingFilters(Vec<String>),

    #[error("Provided supergraph schema is invalid: {0}")]
    InvalidSupergraph(String),

    #[error("{step}: {source}")]
    DirectiveSetup {
        step: FilterStep,
        #[source]
        source: DirectiveError,
    },

    #[error("Filtered supergraph schema is invalid: {0}")]
    InvalidFilteredSupergraph(String),

    /// A step found the schema in a state it should not be able to reach.
    #[error("{step}: {source}")]
    Internal {
        step: FilterStep,
        #[source]
        source: FederationError,
    },
}

impl TransformError {
    pub fn directive_setup(step: FilterStep, source: DirectiveError) -> Self {
        Self::DirectiveSetup { step, source }
    }

    /// Wrap an internal error raised while running `step`.
    pub(crate) fn internal(step: FilterStep) -> impl FnOnce(FederationError) -> Self {
        move |source| Self::Internal { step, source }
    }

    /// The stage the error was raised in.
    pub fn step(&self) -> FilterStep {
        match self {
            Self::OverlappingFilters(_) => FilterStep::Configuration,
            Self::InvalidSupergraph(_) => FilterStep::ParseSchema,
            Self::DirectiveSetup { step, .. } => *step,
            Self::InvalidFilteredSupergraph(_) => FilterStep::ValidateFilteredSchema,
            Self::Internal { step, .. } => *step,
        }
    }

    /// Machine readable classification, reported as the build message `code`.
    pub fn code(&self) -> &'static str {
        match self {
            Self::OverlappingFilters(_) => "INVALID_FILTER_CONFIGURATION",
            Self::InvalidSupergraph(_) => "INVALID_SUPERGRAPH",
            Self::DirectiveSetup { .. } => "INVALID_DIRECTIVE_DEFINITION",
            Self::InvalidFilteredSupergraph(_) => "INVALID_FILTERED_SUPERGRAPH",
            Self::Internal { .. } => "INTERNAL",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::FilterStep;

    /// The `step` strings are the JS `FailedStep` values; changing one changes what build
    /// messages report.
    #[test]
    fn steps_use_the_js_failed_step_names() {
        let steps = [
            (FilterStep::Configuration, "INPUT_VALIDATION"),
            (FilterStep::ParseSchema, "PARSING"),
            (
                FilterStep::AddDirectiveDefinitions,
                "ADD_DIRECTIVE_DEFINITIONS_IF_NOT_PRESENT",
            ),
            (FilterStep::TagInheriting, "TAG_INHERITING"),
            (FilterStep::TagMatching, "TAG_MATCHING"),
            (FilterStep::EmptyEnumMasking, "EMPTY_ENUM_MASKING"),
            (
                FilterStep::EmptyInputObjectMasking,
                "EMPTY_INPUT_OBJECT_MASKING",
            ),
            (
                FilterStep::EmptyObjectAndInterfaceFieldMasking,
                "EMPTY_OBJECT_AND_INTERFACE_FIELD_MASKING",
            ),
            (
                FilterStep::PartialInterfaceMasking,
                "PARTIAL_INTERFACE_MASKING",
            ),
            (
                FilterStep::EmptyObjectAndInterfaceMasking,
                "EMPTY_OBJECT_AND_INTERFACE_MASKING",
            ),
            (FilterStep::EmptyUnionMasking, "EMPTY_UNION_MASKING"),
            (
                FilterStep::UnreachableTypeMasking,
                "UNREACHABLE_TYPE_MASKING",
            ),
            (FilterStep::ValidateFilteredSchema, "PARSING"),
        ];
        for (step, expected) in steps {
            assert_eq!(step.to_string(), expected, "{step:?}");
        }
    }
}
