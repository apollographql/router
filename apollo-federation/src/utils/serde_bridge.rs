//! This module contains functions used to bridge the apollo compiler serialization methods with
//! serialization with serde.

use apollo_compiler::Node;
use apollo_compiler::executable;
use serde::Serializer;
use serde::ser::SerializeSeq;

pub(crate) fn serialize_optional_slice_of_exe_argument_nodes<
    S: Serializer,
    Args: AsRef<[Node<executable::Argument>]>,
>(
    args: &Option<Args>,
    ser: S,
) -> Result<S::Ok, S::Error> {
    let Some(args) = args else {
        return ser.serialize_none();
    };
    let args = args.as_ref();
    let mut ser = ser.serialize_seq(Some(args.len()))?;
    args.iter().try_for_each(|arg| {
        ser.serialize_element(&format!(
            "{}: {}",
            arg.name,
            arg.value.serialize().no_indent()
        ))
    })?;
    ser.end()
}

pub(crate) fn serialize_exe_directive_list<S: Serializer>(
    list: &executable::DirectiveList,
    ser: S,
) -> Result<S::Ok, S::Error> {
    ser.serialize_str(&list.serialize().no_indent().to_string())
}

pub(crate) mod operation_type {
    use apollo_compiler::executable;
    use serde::Deserialize;
    use serde::Deserializer;
    use serde::Serialize;
    use serde::Serializer;

    pub(crate) fn serialize<S: Serializer>(
        ty: &executable::OperationType,
        ser: S,
    ) -> Result<S::Ok, S::Error> {
        match ty {
            executable::OperationType::Query => OperationType::Query,
            executable::OperationType::Mutation => OperationType::Mutation,
            executable::OperationType::Subscription => OperationType::Subscription,
        }
        .serialize(ser)
    }

    pub(crate) fn deserialize<'de, D>(
        deserializier: D,
    ) -> Result<executable::OperationType, D::Error>
    where
        D: Deserializer<'de>,
    {
        Ok(match OperationType::deserialize(deserializier)? {
            OperationType::Query => executable::OperationType::Query,
            OperationType::Mutation => executable::OperationType::Mutation,
            OperationType::Subscription => executable::OperationType::Subscription,
        })
    }

    #[derive(Serialize, Deserialize)]
    #[serde(rename_all = "lowercase")]
    enum OperationType {
        Query,
        Mutation,
        Subscription,
    }
}
