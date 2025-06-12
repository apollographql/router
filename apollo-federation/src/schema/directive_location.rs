use apollo_compiler::ast::DirectiveLocation;

pub(crate) trait DirectiveLocationExt {
    fn is_executable_location(&self) -> bool;
    #[allow(dead_code)]
    fn is_type_system_location(&self) -> bool;
}

impl DirectiveLocationExt for DirectiveLocation {
    fn is_executable_location(&self) -> bool {
        matches!(
            self,
            DirectiveLocation::Query
                | DirectiveLocation::Mutation
                | DirectiveLocation::Subscription
                | DirectiveLocation::Field
                | DirectiveLocation::FragmentDefinition
                | DirectiveLocation::FragmentSpread
                | DirectiveLocation::InlineFragment
                | DirectiveLocation::VariableDefinition
        )
    }

    fn is_type_system_location(&self) -> bool {
        matches!(
            self,
            DirectiveLocation::Schema
                | DirectiveLocation::Scalar
                | DirectiveLocation::Object
                | DirectiveLocation::FieldDefinition
                | DirectiveLocation::ArgumentDefinition
                | DirectiveLocation::Interface
                | DirectiveLocation::Union
                | DirectiveLocation::Enum
                | DirectiveLocation::EnumValue
                | DirectiveLocation::InputObject
                | DirectiveLocation::InputFieldDefinition,
        )
    }
}
