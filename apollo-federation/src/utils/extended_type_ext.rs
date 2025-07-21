use apollo_compiler::schema::ExtendedType;

pub(crate) trait ExtendedTypeExt {
    fn has_extension_elements(&self) -> bool;
    fn has_non_extension_elements(&self) -> bool;
}

fn has_non_extension_inner_elements(extended_type: &ExtendedType) -> bool {
    match extended_type {
        ExtendedType::Scalar(_) => false,
        ExtendedType::Object(t) => {
            t.implements_interfaces
                .iter()
                .any(|itf| itf.origin.extension_id().is_none())
                || t.fields.values().any(|f| f.origin.extension_id().is_none())
        }
        ExtendedType::Interface(t) => {
            t.implements_interfaces
                .iter()
                .any(|itf| itf.origin.extension_id().is_none())
                || t.fields.values().any(|f| f.origin.extension_id().is_none())
        }
        ExtendedType::Union(t) => t.members.iter().any(|m| m.origin.extension_id().is_none()),
        ExtendedType::Enum(t) => t.values.values().any(|v| v.origin.extension_id().is_none()),
        ExtendedType::InputObject(t) => {
            t.fields.values().any(|f| f.origin.extension_id().is_none())
        }
    }
}

impl ExtendedTypeExt for ExtendedType {
    fn has_extension_elements(&self) -> bool {
        match self {
            ExtendedType::Scalar(scalar) => !scalar.extensions().is_empty(),
            ExtendedType::Object(object) => !object.extensions().is_empty(),
            ExtendedType::Interface(interface) => !interface.extensions().is_empty(),
            ExtendedType::Union(union) => !union.extensions().is_empty(),
            ExtendedType::Enum(enum_type) => !enum_type.extensions().is_empty(),
            ExtendedType::InputObject(input_object) => !input_object.extensions().is_empty(),
        }
    }

    fn has_non_extension_elements(&self) -> bool {
        self.directives()
            .iter()
            .any(|d| d.origin.extension_id().is_none())
            || has_non_extension_inner_elements(self)
    }
}
