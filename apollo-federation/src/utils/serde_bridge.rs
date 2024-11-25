use apollo_compiler::{executable, validation::Valid, ExecutableDocument, Node};
use serde::{ser::SerializeSeq, Serializer};

/// This module contains functions used to bridge the apollo compiler serialization methods with
/// serialization with serde.

pub(crate) fn serialize_exe_field<S: Serializer>(
    field: &executable::Field,
    ser: S,
) -> Result<S::Ok, S::Error> {
    ser.serialize_str(&field.serialize().no_indent().to_string())
}

pub(crate) fn serialize_exe_inline_fragment<S: Serializer>(
    fragment: &executable::InlineFragment,
    ser: S,
) -> Result<S::Ok, S::Error> {
    ser.serialize_str(&fragment.serialize().no_indent().to_string())
}

pub(crate) fn serialize_optional_exe_selection_set<S: Serializer>(
    set: &Option<executable::SelectionSet>,
    ser: S,
) -> Result<S::Ok, S::Error> {
    match set {
        Some(set) => ser.serialize_str(&set.serialize().no_indent().to_string()),
        None => ser.serialize_none(),
    }
}

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
    let _ser = ser.serialize_seq(Some(args.len()))?;
    // FIXME: Arg doesn't have the serialize method like the other types:
    // args.iter().try_for_each(|arg| ser.serialize_element(&arg.))?;
    // ser.end()
    todo!();
}

pub(crate) fn serialize_exe_directive_list<S: Serializer>(
    list: &executable::DirectiveList,
    ser: S,
) -> Result<S::Ok, S::Error> {
    ser.serialize_str(&list.serialize().no_indent().to_string())
}

pub(crate) fn serialize_optional_vec_of_exe_selection<S: Serializer>(
    selection: &Option<Vec<executable::Selection>>,
    ser: S,
) -> Result<S::Ok, S::Error> {
    let Some(selections) = selection else {
        return ser.serialize_none();
    };
    let mut ser = ser.serialize_seq(Some(selections.len()))?;
    selections.iter().try_for_each(|selection| {
        ser.serialize_element(&selection.serialize().no_indent().to_string())
    })?;
    ser.end()
}

pub(crate) fn serialize_valid_executable_document<S: Serializer>(
    doc: &Valid<ExecutableDocument>,
    ser: S,
) -> Result<S::Ok, S::Error> {
    ser.serialize_str(&doc.serialize().no_indent().to_string())
}

pub(crate) fn serialize_exe_operation_type<S: Serializer>(
    ty: &executable::OperationType,
    ser: S,
) -> Result<S::Ok, S::Error> {
    ser.serialize_str(&ty.to_string())
}
