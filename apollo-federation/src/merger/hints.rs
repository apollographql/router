#[allow(dead_code)]
use std::sync::LazyLock;

#[derive(Debug)]
#[allow(dead_code)]
pub(crate) enum HintLevel {
    Warn,
    Info,
    Debug,
}

#[allow(dead_code)]
impl HintLevel {
    pub(crate) fn name(&self) -> &'static str {
        match self {
            HintLevel::Warn => "WARN",
            HintLevel::Info => "INFO",
            HintLevel::Debug => "DEBUG",
        }
    }
}

#[derive(Debug)]
pub(crate) struct HintCodeDefinition {
    code: String,
    level: HintLevel,
    description: String,
}

#[allow(dead_code)]
impl HintCodeDefinition {
    pub(crate) fn new(
        code: impl Into<String>,
        level: HintLevel,
        description: impl Into<String>,
    ) -> Self {
        Self {
            code: code.into(),
            level,
            description: description.into(),
        }
    }

    pub(crate) fn code(&self) -> &str {
        &self.code
    }

    pub(crate) fn level(&self) -> &HintLevel {
        &self.level
    }

    pub(crate) fn description(&self) -> &str {
        &self.description
    }
}

#[derive(Clone, Debug)]
#[allow(dead_code)]
pub(crate) enum HintCode {
    InconsistentButCompatibleFieldType,
    InconsistentButCompatibleArgumentType,
    InconsistentDefaultValuePresence,
    InconsistentEntity,
    InconsistentObjectValueTypeField,
    InconsistentInterfaceValueTypeField,
    InconsistentInputObjectField,
    InconsistentUnionMember,
    InconsistentEnumValueForInputEnum,
    InconsistentEnumValueForOutputEnum,
    InconsistentTypeSystemDirectiveRepeatable,
    InconsistentTypeSystemDirectiveLocations,
    InconsistentExecutableDirectivePresence,
    NoExecutableDirectiveLocationsIntersection,
    InconsistentExecutableDirectiveRepeatable,
    InconsistentExecutableDirectiveLocations,
    InconsistentDescription,
    InconsistentArgumentPresence,
    FromSubgraphDoesNotExist,
    OverriddenFieldCanBeRemoved,
    OverrideDirectiveCanBeRemoved,
    OverrideMigrationInProgress,
    UnusedEnumType,
    InconsistentNonRepeatableDirectiveArguments,
    MergedNonRepeatableDirectiveArguments,
    DirectiveCompositionInfo,
    DirectiveCompositionWarn,
    InconsistentRuntimeTypesForShareableReturn,
    ImplicitlyUpgradedFederationVersion,
    ContextualArgumentNotContextualInAllSubgraphs,
    InterfaceKeyMissingImplementationType,
}

#[allow(dead_code)]
impl HintCode {
    pub(crate) fn definition(&self) -> &'static HintCodeDefinition {
        match self {
            HintCode::InconsistentButCompatibleFieldType => &INCONSISTENT_BUT_COMPATIBLE_FIELD_TYPE,
            HintCode::InconsistentButCompatibleArgumentType => {
                &INCONSISTENT_BUT_COMPATIBLE_ARGUMENT_TYPE
            }
            HintCode::InconsistentDefaultValuePresence => &INCONSISTENT_DEFAULT_VALUE_PRESENCE,
            HintCode::InconsistentEntity => &INCONSISTENT_ENTITY,
            HintCode::InconsistentObjectValueTypeField => &INCONSISTENT_OBJECT_VALUE_TYPE_FIELD,
            HintCode::InconsistentInterfaceValueTypeField => {
                &INCONSISTENT_INTERFACE_VALUE_TYPE_FIELD
            }
            HintCode::InconsistentInputObjectField => &INCONSISTENT_INPUT_OBJECT_FIELD,
            HintCode::InconsistentUnionMember => &INCONSISTENT_UNION_MEMBER,
            HintCode::InconsistentEnumValueForInputEnum => &INCONSISTENT_ENUM_VALUE_FOR_INPUT_ENUM,
            HintCode::InconsistentEnumValueForOutputEnum => {
                &INCONSISTENT_ENUM_VALUE_FOR_OUTPUT_ENUM
            }
            HintCode::InconsistentTypeSystemDirectiveRepeatable => {
                &INCONSISTENT_TYPE_SYSTEM_DIRECTIVE_REPEATABLE
            }
            HintCode::InconsistentTypeSystemDirectiveLocations => {
                &INCONSISTENT_TYPE_SYSTEM_DIRECTIVE_LOCATIONS
            }
            HintCode::InconsistentExecutableDirectivePresence => {
                &INCONSISTENT_EXECUTABLE_DIRECTIVE_PRESENCE
            }
            HintCode::NoExecutableDirectiveLocationsIntersection => {
                &NO_EXECUTABLE_DIRECTIVE_LOCATIONS_INTERSECTION
            }
            HintCode::InconsistentExecutableDirectiveRepeatable => {
                &INCONSISTENT_EXECUTABLE_DIRECTIVE_REPEATABLE
            }
            HintCode::InconsistentExecutableDirectiveLocations => {
                &INCONSISTENT_EXECUTABLE_DIRECTIVE_LOCATIONS
            }
            HintCode::InconsistentDescription => &INCONSISTENT_DESCRIPTION,
            HintCode::InconsistentArgumentPresence => &INCONSISTENT_ARGUMENT_PRESENCE,
            HintCode::FromSubgraphDoesNotExist => &FROM_SUBGRAPH_DOES_NOT_EXIST,
            HintCode::OverriddenFieldCanBeRemoved => &OVERRIDDEN_FIELD_CAN_BE_REMOVED,
            HintCode::OverrideDirectiveCanBeRemoved => &OVERRIDE_DIRECTIVE_CAN_BE_REMOVED,
            HintCode::OverrideMigrationInProgress => &OVERRIDE_MIGRATION_IN_PROGRESS,
            HintCode::UnusedEnumType => &UNUSED_ENUM_TYPE,
            HintCode::InconsistentNonRepeatableDirectiveArguments => {
                &INCONSISTENT_NON_REPEATABLE_DIRECTIVE_ARGUMENTS
            }
            HintCode::MergedNonRepeatableDirectiveArguments => {
                &MERGED_NON_REPEATABLE_DIRECTIVE_ARGUMENTS
            }
            HintCode::DirectiveCompositionInfo => &DIRECTIVE_COMPOSITION_INFO,
            HintCode::DirectiveCompositionWarn => &DIRECTIVE_COMPOSITION_WARN,
            HintCode::InconsistentRuntimeTypesForShareableReturn => {
                &INCONSISTENT_RUNTIME_TYPES_FOR_SHAREABLE_RETURN
            }
            HintCode::ImplicitlyUpgradedFederationVersion => {
                &IMPLICITLY_UPGRADED_FEDERATION_VERSION
            }
            HintCode::ContextualArgumentNotContextualInAllSubgraphs => {
                &CONTEXTUAL_ARGUMENT_NOT_CONTEXTUAL_IN_ALL_SUBGRAPHS
            }
            HintCode::InterfaceKeyMissingImplementationType => {
                &INTERFACE_KEY_MISSING_IMPLEMENTATION_TYPE
            }
        }
    }

    pub(crate) fn code(&self) -> &str {
        self.definition().code()
    }
}

#[allow(dead_code)]
pub(crate) static INCONSISTENT_BUT_COMPATIBLE_FIELD_TYPE: LazyLock<HintCodeDefinition> =
    LazyLock::new(|| {
        HintCodeDefinition::new(
            "INCONSISTENT_BUT_COMPATIBLE_FIELD_TYPE",
            HintLevel::Warn,
            "Field has inconsistent but compatible type across subgraphs",
        )
    });

#[allow(dead_code)]
pub(crate) static INCONSISTENT_BUT_COMPATIBLE_ARGUMENT_TYPE: LazyLock<HintCodeDefinition> =
    LazyLock::new(|| {
        HintCodeDefinition::new(
            "INCONSISTENT_BUT_COMPATIBLE_ARGUMENT_TYPE",
            HintLevel::Warn,
            "Argument has inconsistent but compatible type across subgraphs",
        )
    });

#[allow(dead_code)]
pub(crate) static INCONSISTENT_DEFAULT_VALUE_PRESENCE: LazyLock<HintCodeDefinition> =
    LazyLock::new(|| {
        HintCodeDefinition::new(
            "INCONSISTENT_DEFAULT_VALUE_PRESENCE",
            HintLevel::Warn,
            "Default value presence is inconsistent across subgraphs",
        )
    });

#[allow(dead_code)]
pub(crate) static INCONSISTENT_ENTITY: LazyLock<HintCodeDefinition> = LazyLock::new(|| {
    HintCodeDefinition::new(
        "INCONSISTENT_ENTITY",
        HintLevel::Warn,
        "Entity definition is inconsistent across subgraphs",
    )
});

#[allow(dead_code)]
pub(crate) static INCONSISTENT_OBJECT_VALUE_TYPE_FIELD: LazyLock<HintCodeDefinition> =
    LazyLock::new(|| {
        HintCodeDefinition::new(
            "INCONSISTENT_OBJECT_VALUE_TYPE_FIELD",
            HintLevel::Warn,
            "Object value type field is inconsistent across subgraphs",
        )
    });

#[allow(dead_code)]
pub(crate) static INCONSISTENT_INTERFACE_VALUE_TYPE_FIELD: LazyLock<HintCodeDefinition> =
    LazyLock::new(|| {
        HintCodeDefinition::new(
            "INCONSISTENT_INTERFACE_VALUE_TYPE_FIELD",
            HintLevel::Warn,
            "Interface value type field is inconsistent across subgraphs",
        )
    });

#[allow(dead_code)]
pub(crate) static INCONSISTENT_INPUT_OBJECT_FIELD: LazyLock<HintCodeDefinition> =
    LazyLock::new(|| {
        HintCodeDefinition::new(
            "INCONSISTENT_INPUT_OBJECT_FIELD",
            HintLevel::Warn,
            "Input object field is inconsistent across subgraphs",
        )
    });

#[allow(dead_code)]
pub(crate) static INCONSISTENT_UNION_MEMBER: LazyLock<HintCodeDefinition> = LazyLock::new(|| {
    HintCodeDefinition::new(
        "INCONSISTENT_UNION_MEMBER",
        HintLevel::Warn,
        "Union member is inconsistent across subgraphs",
    )
});

#[allow(dead_code)]
pub(crate) static INCONSISTENT_ENUM_VALUE_FOR_INPUT_ENUM: LazyLock<HintCodeDefinition> =
    LazyLock::new(|| {
        HintCodeDefinition::new(
            "INCONSISTENT_ENUM_VALUE_FOR_INPUT_ENUM",
            HintLevel::Warn,
            "Enum value for input enum is inconsistent across subgraphs",
        )
    });

#[allow(dead_code)]
pub(crate) static INCONSISTENT_ENUM_VALUE_FOR_OUTPUT_ENUM: LazyLock<HintCodeDefinition> =
    LazyLock::new(|| {
        HintCodeDefinition::new(
            "INCONSISTENT_ENUM_VALUE_FOR_OUTPUT_ENUM",
            HintLevel::Warn,
            "Enum value for output enum is inconsistent across subgraphs",
        )
    });

#[allow(dead_code)]
pub(crate) static INCONSISTENT_TYPE_SYSTEM_DIRECTIVE_REPEATABLE: LazyLock<HintCodeDefinition> =
    LazyLock::new(|| {
        HintCodeDefinition::new(
            "INCONSISTENT_TYPE_SYSTEM_DIRECTIVE_REPEATABLE",
            HintLevel::Warn,
            "Type system directive repeatable property is inconsistent across subgraphs",
        )
    });

#[allow(dead_code)]
pub(crate) static INCONSISTENT_TYPE_SYSTEM_DIRECTIVE_LOCATIONS: LazyLock<HintCodeDefinition> =
    LazyLock::new(|| {
        HintCodeDefinition::new(
            "INCONSISTENT_TYPE_SYSTEM_DIRECTIVE_LOCATIONS",
            HintLevel::Warn,
            "Type system directive locations are inconsistent across subgraphs",
        )
    });

#[allow(dead_code)]
pub(crate) static INCONSISTENT_EXECUTABLE_DIRECTIVE_PRESENCE: LazyLock<HintCodeDefinition> =
    LazyLock::new(|| {
        HintCodeDefinition::new(
            "INCONSISTENT_EXECUTABLE_DIRECTIVE_PRESENCE",
            HintLevel::Warn,
            "Executable directive presence is inconsistent across subgraphs",
        )
    });

#[allow(dead_code)]
pub(crate) static NO_EXECUTABLE_DIRECTIVE_LOCATIONS_INTERSECTION: LazyLock<HintCodeDefinition> =
    LazyLock::new(|| {
        HintCodeDefinition::new(
            "NO_EXECUTABLE_DIRECTIVE_LOCATIONS_INTERSECTION",
            HintLevel::Warn,
            "No intersection between executable directive locations across subgraphs",
        )
    });

#[allow(dead_code)]
pub(crate) static INCONSISTENT_EXECUTABLE_DIRECTIVE_REPEATABLE: LazyLock<HintCodeDefinition> =
    LazyLock::new(|| {
        HintCodeDefinition::new(
            "INCONSISTENT_EXECUTABLE_DIRECTIVE_REPEATABLE",
            HintLevel::Warn,
            "Executable directive repeatable property is inconsistent across subgraphs",
        )
    });

#[allow(dead_code)]
pub(crate) static INCONSISTENT_EXECUTABLE_DIRECTIVE_LOCATIONS: LazyLock<HintCodeDefinition> =
    LazyLock::new(|| {
        HintCodeDefinition::new(
            "INCONSISTENT_EXECUTABLE_DIRECTIVE_LOCATIONS",
            HintLevel::Warn,
            "Executable directive locations are inconsistent across subgraphs",
        )
    });

#[allow(dead_code)]
pub(crate) static INCONSISTENT_DESCRIPTION: LazyLock<HintCodeDefinition> = LazyLock::new(|| {
    HintCodeDefinition::new(
        "INCONSISTENT_DESCRIPTION",
        HintLevel::Warn,
        "Description is inconsistent across subgraphs",
    )
});

#[allow(dead_code)]
pub(crate) static INCONSISTENT_ARGUMENT_PRESENCE: LazyLock<HintCodeDefinition> =
    LazyLock::new(|| {
        HintCodeDefinition::new(
            "INCONSISTENT_ARGUMENT_PRESENCE",
            HintLevel::Warn,
            "Argument presence is inconsistent across subgraphs",
        )
    });

#[allow(dead_code)]
pub(crate) static FROM_SUBGRAPH_DOES_NOT_EXIST: LazyLock<HintCodeDefinition> =
    LazyLock::new(|| {
        HintCodeDefinition::new(
            "FROM_SUBGRAPH_DOES_NOT_EXIST",
            HintLevel::Warn,
            "From subgraph does not exist",
        )
    });

#[allow(dead_code)]
pub(crate) static OVERRIDDEN_FIELD_CAN_BE_REMOVED: LazyLock<HintCodeDefinition> =
    LazyLock::new(|| {
        HintCodeDefinition::new(
            "OVERRIDDEN_FIELD_CAN_BE_REMOVED",
            HintLevel::Info,
            "Overridden field can be removed",
        )
    });

#[allow(dead_code)]
pub(crate) static OVERRIDE_DIRECTIVE_CAN_BE_REMOVED: LazyLock<HintCodeDefinition> =
    LazyLock::new(|| {
        HintCodeDefinition::new(
            "OVERRIDE_DIRECTIVE_CAN_BE_REMOVED",
            HintLevel::Info,
            "Override directive can be removed",
        )
    });

#[allow(dead_code)]
pub(crate) static OVERRIDE_MIGRATION_IN_PROGRESS: LazyLock<HintCodeDefinition> =
    LazyLock::new(|| {
        HintCodeDefinition::new(
            "OVERRIDE_MIGRATION_IN_PROGRESS",
            HintLevel::Info,
            "Override migration is in progress",
        )
    });

#[allow(dead_code)]
pub(crate) static UNUSED_ENUM_TYPE: LazyLock<HintCodeDefinition> = LazyLock::new(|| {
    HintCodeDefinition::new("UNUSED_ENUM_TYPE", HintLevel::Warn, "Enum type is unused")
});

#[allow(dead_code)]
pub(crate) static INCONSISTENT_NON_REPEATABLE_DIRECTIVE_ARGUMENTS: LazyLock<HintCodeDefinition> =
    LazyLock::new(|| {
        HintCodeDefinition::new(
            "INCONSISTENT_NON_REPEATABLE_DIRECTIVE_ARGUMENTS",
            HintLevel::Warn,
            "Non-repeatable directive arguments are inconsistent across subgraphs",
        )
    });

#[allow(dead_code)]
pub(crate) static MERGED_NON_REPEATABLE_DIRECTIVE_ARGUMENTS: LazyLock<HintCodeDefinition> =
    LazyLock::new(|| {
        HintCodeDefinition::new(
            "MERGED_NON_REPEATABLE_DIRECTIVE_ARGUMENTS",
            HintLevel::Info,
            "Non-repeatable directive arguments have been merged",
        )
    });

#[allow(dead_code)]
pub(crate) static DIRECTIVE_COMPOSITION_INFO: LazyLock<HintCodeDefinition> = LazyLock::new(|| {
    HintCodeDefinition::new(
        "DIRECTIVE_COMPOSITION_INFO",
        HintLevel::Info,
        "Directive composition information",
    )
});

#[allow(dead_code)]
pub(crate) static DIRECTIVE_COMPOSITION_WARN: LazyLock<HintCodeDefinition> = LazyLock::new(|| {
    HintCodeDefinition::new(
        "DIRECTIVE_COMPOSITION_WARN",
        HintLevel::Warn,
        "Directive composition warning",
    )
});

#[allow(dead_code)]
pub(crate) static INCONSISTENT_RUNTIME_TYPES_FOR_SHAREABLE_RETURN: LazyLock<HintCodeDefinition> =
    LazyLock::new(|| {
        HintCodeDefinition::new(
            "INCONSISTENT_RUNTIME_TYPES_FOR_SHAREABLE_RETURN",
            HintLevel::Warn,
            "Runtime types for shareable return are inconsistent across subgraphs",
        )
    });

#[allow(dead_code)]
pub(crate) static IMPLICITLY_UPGRADED_FEDERATION_VERSION: LazyLock<HintCodeDefinition> =
    LazyLock::new(|| {
        HintCodeDefinition::new(
            "IMPLICITLY_UPGRADED_FEDERATION_VERSION",
            HintLevel::Info,
            "Federation version has been implicitly upgraded",
        )
    });

#[allow(dead_code)]
pub(crate) static CONTEXTUAL_ARGUMENT_NOT_CONTEXTUAL_IN_ALL_SUBGRAPHS: LazyLock<
    HintCodeDefinition,
> = LazyLock::new(|| {
    HintCodeDefinition::new(
        "CONTEXTUAL_ARGUMENT_NOT_CONTEXTUAL_IN_ALL_SUBGRAPHS",
        HintLevel::Warn,
        "Contextual argument is not contextual in all subgraphs",
    )
});

#[allow(dead_code)]
pub(crate) static INTERFACE_KEY_MISSING_IMPLEMENTATION_TYPE: LazyLock<HintCodeDefinition> =
    LazyLock::new(|| {
        HintCodeDefinition::new(
            "INTERFACE_KEY_MISSING_IMPLEMENTATION_TYPE",
            HintLevel::Warn,
            "Interface key missing implementation type",
        )
    });
