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

pub(crate) fn serialize_exe_operation_type<S: Serializer>(
    ty: &executable::OperationType,
    ser: S,
) -> Result<S::Ok, S::Error> {
    ser.serialize_str(&ty.to_string())
}
