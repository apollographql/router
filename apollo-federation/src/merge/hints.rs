#[allow(dead_code)]
use std::sync::LazyLock;

use crate::supergraph::CompositionHint;

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
        }
    }
}

#[allow(dead_code)]
pub(crate) struct HintCodeDefinition {
    pub(crate) code: String,
    pub(crate) level: HintLevel,
    pub(crate) description: String,
}

#[allow(dead_code)]
impl HintCodeDefinition {
    pub(crate) fn new(code: impl Into<String>, level: HintLevel, description: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            level,
            description: description.into(),
        }
    }
}

#[allow(dead_code)]
pub(crate) static INCONSISTENT_BUT_COMPATIBLE_FIELD_TYPE: LazyLock<HintCodeDefinition> = LazyLock::new(|| {
    HintCodeDefinition::new(
        "INCONSISTENT_BUT_COMPATIBLE_FIELD_TYPE",
        HintLevel::Warn,
        "Field has inconsistent but compatible type across subgraphs"
    )
});

#[allow(dead_code)]
pub(crate) static INCONSISTENT_BUT_COMPATIBLE_ARGUMENT_TYPE: LazyLock<HintCodeDefinition> = LazyLock::new(|| {
    HintCodeDefinition::new(
        "INCONSISTENT_BUT_COMPATIBLE_ARGUMENT_TYPE",
        HintLevel::Warn,
        "Argument has inconsistent but compatible type across subgraphs"
    )
});

#[allow(dead_code)]
pub(crate) static INCONSISTENT_DEFAULT_VALUE_PRESENCE: LazyLock<HintCodeDefinition> = LazyLock::new(|| {
    HintCodeDefinition::new(
        "INCONSISTENT_DEFAULT_VALUE_PRESENCE",
        HintLevel::Warn,
        "Default value presence is inconsistent across subgraphs"
    )
});

#[allow(dead_code)]
pub(crate) static INCONSISTENT_ENTITY: LazyLock<HintCodeDefinition> = LazyLock::new(|| {
    HintCodeDefinition::new(
        "INCONSISTENT_ENTITY",
        HintLevel::Warn,
        "Entity definition is inconsistent across subgraphs"
    )
});

#[allow(dead_code)]
pub(crate) static INCONSISTENT_OBJECT_VALUE_TYPE_FIELD: LazyLock<HintCodeDefinition> = LazyLock::new(|| {
    HintCodeDefinition::new(
        "INCONSISTENT_OBJECT_VALUE_TYPE_FIELD",
        HintLevel::Warn,
        "Object value type field is inconsistent across subgraphs"
    )
});

#[allow(dead_code)]
pub(crate) static INCONSISTENT_INTERFACE_VALUE_TYPE_FIELD: LazyLock<HintCodeDefinition> = LazyLock::new(|| {
    HintCodeDefinition::new(
        "INCONSISTENT_INTERFACE_VALUE_TYPE_FIELD",
        HintLevel::Warn,
        "Interface value type field is inconsistent across subgraphs"
    )
});

#[allow(dead_code)]
pub(crate) static INCONSISTENT_INPUT_OBJECT_FIELD: LazyLock<HintCodeDefinition> = LazyLock::new(|| {
    HintCodeDefinition::new(
        "INCONSISTENT_INPUT_OBJECT_FIELD",
        HintLevel::Warn,
        "Input object field is inconsistent across subgraphs"
    )
});

#[allow(dead_code)]
pub(crate) static INCONSISTENT_UNION_MEMBER: LazyLock<HintCodeDefinition> = LazyLock::new(|| {
    HintCodeDefinition::new(
        "INCONSISTENT_UNION_MEMBER",
        HintLevel::Warn,
        "Union member is inconsistent across subgraphs"
    )
});

#[allow(dead_code)]
pub(crate) static INCONSISTENT_ENUM_VALUE_FOR_INPUT_ENUM: LazyLock<HintCodeDefinition> = LazyLock::new(|| {
    HintCodeDefinition::new(
        "INCONSISTENT_ENUM_VALUE_FOR_INPUT_ENUM",
        HintLevel::Warn,
        "Enum value for input enum is inconsistent across subgraphs"
    )
});

#[allow(dead_code)]
pub(crate) static INCONSISTENT_ENUM_VALUE_FOR_OUTPUT_ENUM: LazyLock<HintCodeDefinition> = LazyLock::new(|| {
    HintCodeDefinition::new(
        "INCONSISTENT_ENUM_VALUE_FOR_OUTPUT_ENUM",
        HintLevel::Warn,
        "Enum value for output enum is inconsistent across subgraphs"
    )
});

#[allow(dead_code)]
pub(crate) static INCONSISTENT_TYPE_SYSTEM_DIRECTIVE_REPEATABLE: LazyLock<HintCodeDefinition> = LazyLock::new(|| {
    HintCodeDefinition::new(
        "INCONSISTENT_TYPE_SYSTEM_DIRECTIVE_REPEATABLE",
        HintLevel::Warn,
        "Type system directive repeatable property is inconsistent across subgraphs"
    )
});

#[allow(dead_code)]
pub(crate) static INCONSISTENT_TYPE_SYSTEM_DIRECTIVE_LOCATIONS: LazyLock<HintCodeDefinition> = LazyLock::new(|| {
    HintCodeDefinition::new(
        "INCONSISTENT_TYPE_SYSTEM_DIRECTIVE_LOCATIONS",
        HintLevel::Warn,
        "Type system directive locations are inconsistent across subgraphs"
    )
});

#[allow(dead_code)]
pub(crate) static INCONSISTENT_EXECUTABLE_DIRECTIVE_PRESENCE: LazyLock<HintCodeDefinition> = LazyLock::new(|| {
    HintCodeDefinition::new(
        "INCONSISTENT_EXECUTABLE_DIRECTIVE_PRESENCE",
        HintLevel::Warn,
        "Executable directive presence is inconsistent across subgraphs"
    )
});

#[allow(dead_code)]
pub(crate) static NO_EXECUTABLE_DIRECTIVE_LOCATIONS_INTERSECTION: LazyLock<HintCodeDefinition> = LazyLock::new(|| {
    HintCodeDefinition::new(
        "NO_EXECUTABLE_DIRECTIVE_LOCATIONS_INTERSECTION",
        HintLevel::Warn,
        "No intersection between executable directive locations across subgraphs"
    )
});

#[allow(dead_code)]
pub(crate) static INCONSISTENT_EXECUTABLE_DIRECTIVE_REPEATABLE: LazyLock<HintCodeDefinition> = LazyLock::new(|| {
    HintCodeDefinition::new(
        "INCONSISTENT_EXECUTABLE_DIRECTIVE_REPEATABLE",
        HintLevel::Warn,
        "Executable directive repeatable property is inconsistent across subgraphs"
    )
});

#[allow(dead_code)]
pub(crate) static INCONSISTENT_EXECUTABLE_DIRECTIVE_LOCATIONS: LazyLock<HintCodeDefinition> = LazyLock::new(|| {
    HintCodeDefinition::new(
        "INCONSISTENT_EXECUTABLE_DIRECTIVE_LOCATIONS",
        HintLevel::Warn,
        "Executable directive locations are inconsistent across subgraphs"
    )
});

#[allow(dead_code)]
pub(crate) static INCONSISTENT_DESCRIPTION: LazyLock<HintCodeDefinition> = LazyLock::new(|| {
    HintCodeDefinition::new(
        "INCONSISTENT_DESCRIPTION",
        HintLevel::Warn,
        "Description is inconsistent across subgraphs"
    )
});

#[allow(dead_code)]
pub(crate) static INCONSISTENT_ARGUMENT_PRESENCE: LazyLock<HintCodeDefinition> = LazyLock::new(|| {
    HintCodeDefinition::new(
        "INCONSISTENT_ARGUMENT_PRESENCE",
        HintLevel::Warn,
        "Argument presence is inconsistent across subgraphs"
    )
});

#[allow(dead_code)]
pub(crate) static FROM_SUBGRAPH_DOES_NOT_EXIST: LazyLock<HintCodeDefinition> = LazyLock::new(|| {
    HintCodeDefinition::new(
        "FROM_SUBGRAPH_DOES_NOT_EXIST",
        HintLevel::Warn,
        "From subgraph does not exist"
    )
});

#[allow(dead_code)]
pub(crate) static OVERRIDDEN_FIELD_CAN_BE_REMOVED: LazyLock<HintCodeDefinition> = LazyLock::new(|| {
    HintCodeDefinition::new(
        "OVERRIDDEN_FIELD_CAN_BE_REMOVED",
        HintLevel::Info,
        "Overridden field can be removed"
    )
});

#[allow(dead_code)]
pub(crate) static OVERRIDE_DIRECTIVE_CAN_BE_REMOVED: LazyLock<HintCodeDefinition> = LazyLock::new(|| {
    HintCodeDefinition::new(
        "OVERRIDE_DIRECTIVE_CAN_BE_REMOVED",
        HintLevel::Info,
        "Override directive can be removed"
    )
});

#[allow(dead_code)]
pub(crate) static OVERRIDE_MIGRATION_IN_PROGRESS: LazyLock<HintCodeDefinition> = LazyLock::new(|| {
    HintCodeDefinition::new(
        "OVERRIDE_MIGRATION_IN_PROGRESS",
        HintLevel::Info,
        "Override migration is in progress"
    )
});

#[allow(dead_code)]
pub(crate) static UNUSED_ENUM_TYPE: LazyLock<HintCodeDefinition> = LazyLock::new(|| {
    HintCodeDefinition::new(
        "UNUSED_ENUM_TYPE",
        HintLevel::Warn,
        "Enum type is unused"
    )
});

#[allow(dead_code)]
pub(crate) static INCONSISTENT_NON_REPEATABLE_DIRECTIVE_ARGUMENTS: LazyLock<HintCodeDefinition> = LazyLock::new(|| {
    HintCodeDefinition::new(
        "INCONSISTENT_NON_REPEATABLE_DIRECTIVE_ARGUMENTS",
        HintLevel::Warn,
        "Non-repeatable directive arguments are inconsistent across subgraphs"
    )
});

#[allow(dead_code)]
pub(crate) static MERGED_NON_REPEATABLE_DIRECTIVE_ARGUMENTS: LazyLock<HintCodeDefinition> = LazyLock::new(|| {
    HintCodeDefinition::new(
        "MERGED_NON_REPEATABLE_DIRECTIVE_ARGUMENTS",
        HintLevel::Info,
        "Non-repeatable directive arguments have been merged"
    )
});

#[allow(dead_code)]
pub(crate) static DIRECTIVE_COMPOSITION_INFO: LazyLock<HintCodeDefinition> = LazyLock::new(|| {
    HintCodeDefinition::new(
        "DIRECTIVE_COMPOSITION_INFO",
        HintLevel::Info,
        "Directive composition information"
    )
});

#[allow(dead_code)]
pub(crate) static DIRECTIVE_COMPOSITION_WARN: LazyLock<HintCodeDefinition> = LazyLock::new(|| {
    HintCodeDefinition::new(
        "DIRECTIVE_COMPOSITION_WARN",
        HintLevel::Warn,
        "Directive composition warning"
    )
});

#[allow(dead_code)]
pub(crate) static INCONSISTENT_RUNTIME_TYPES_FOR_SHAREABLE_RETURN: LazyLock<HintCodeDefinition> = LazyLock::new(|| {
    HintCodeDefinition::new(
        "INCONSISTENT_RUNTIME_TYPES_FOR_SHAREABLE_RETURN",
        HintLevel::Warn,
        "Runtime types for shareable return are inconsistent across subgraphs"
    )
});

#[allow(dead_code)]
pub(crate) static IMPLICITLY_UPGRADED_FEDERATION_VERSION: LazyLock<HintCodeDefinition> = LazyLock::new(|| {
    HintCodeDefinition::new(
        "IMPLICITLY_UPGRADED_FEDERATION_VERSION",
        HintLevel::Info,
        "Federation version has been implicitly upgraded"
    )
});

#[allow(dead_code)]
pub(crate) static CONTEXTUAL_ARGUMENT_NOT_CONTEXTUAL_IN_ALL_SUBGRAPHS: LazyLock<HintCodeDefinition> = LazyLock::new(|| {
    HintCodeDefinition::new(
        "CONTEXTUAL_ARGUMENT_NOT_CONTEXTUAL_IN_ALL_SUBGRAPHS",
        HintLevel::Warn,
        "Contextual argument is not contextual in all subgraphs"
    )
});

// Helper functions for creating hints

#[allow(dead_code)]
pub(crate) fn create_hint(_definition: &HintCodeDefinition, message: impl Into<String>) -> CompositionHint {
    CompositionHint::new(message)
}

#[allow(dead_code)]
pub(crate) fn create_inconsistent_but_compatible_field_type_hint(
    message: impl Into<String>,
) -> CompositionHint {
    create_hint(&INCONSISTENT_BUT_COMPATIBLE_FIELD_TYPE, message)
}

#[allow(dead_code)]
pub(crate) fn create_inconsistent_but_compatible_argument_type_hint(
    message: impl Into<String>,
) -> CompositionHint {
    create_hint(&INCONSISTENT_BUT_COMPATIBLE_ARGUMENT_TYPE, message)
}

#[allow(dead_code)]
pub(crate) fn create_inconsistent_default_value_presence_hint(
    message: impl Into<String>,
) -> CompositionHint {
    create_hint(&INCONSISTENT_DEFAULT_VALUE_PRESENCE, message)
}

#[allow(dead_code)]
pub(crate) fn create_inconsistent_entity_hint(message: impl Into<String>) -> CompositionHint {
    create_hint(&INCONSISTENT_ENTITY, message)
}

#[allow(dead_code)]
pub(crate) fn create_inconsistent_object_value_type_field_hint(
    message: impl Into<String>,
) -> CompositionHint {
    create_hint(&INCONSISTENT_OBJECT_VALUE_TYPE_FIELD, message)
}

#[allow(dead_code)]
pub(crate) fn create_inconsistent_interface_value_type_field_hint(
    message: impl Into<String>,
) -> CompositionHint {
    create_hint(&INCONSISTENT_INTERFACE_VALUE_TYPE_FIELD, message)
}

#[allow(dead_code)]
pub(crate) fn create_inconsistent_input_object_field_hint(message: impl Into<String>) -> CompositionHint {
    create_hint(&INCONSISTENT_INPUT_OBJECT_FIELD, message)
}

#[allow(dead_code)]
pub(crate) fn create_inconsistent_union_member_hint(message: impl Into<String>) -> CompositionHint {
    create_hint(&INCONSISTENT_UNION_MEMBER, message)
}

#[allow(dead_code)]
pub(crate) fn create_inconsistent_enum_value_for_input_enum_hint(
    message: impl Into<String>,
) -> CompositionHint {
    create_hint(&INCONSISTENT_ENUM_VALUE_FOR_INPUT_ENUM, message)
}

#[allow(dead_code)]
pub(crate) fn create_inconsistent_enum_value_for_output_enum_hint(
    message: impl Into<String>,
) -> CompositionHint {
    create_hint(&INCONSISTENT_ENUM_VALUE_FOR_OUTPUT_ENUM, message)
}

#[allow(dead_code)]
pub(crate) fn create_from_subgraph_does_not_exist_hint(message: impl Into<String>) -> CompositionHint {
    create_hint(&FROM_SUBGRAPH_DOES_NOT_EXIST, message)
}

#[allow(dead_code)]
pub(crate) fn create_overridden_field_can_be_removed_hint(message: impl Into<String>) -> CompositionHint {
    create_hint(&OVERRIDDEN_FIELD_CAN_BE_REMOVED, message)
}

#[allow(dead_code)]
pub(crate) fn create_override_directive_can_be_removed_hint(
    message: impl Into<String>,
) -> CompositionHint {
    create_hint(&OVERRIDE_DIRECTIVE_CAN_BE_REMOVED, message)
}

#[allow(dead_code)]
pub(crate) fn create_override_migration_in_progress_hint(message: impl Into<String>) -> CompositionHint {
    create_hint(&OVERRIDE_MIGRATION_IN_PROGRESS, message)
}