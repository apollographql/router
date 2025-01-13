use apollo_compiler::collections::IndexMap;
use serde_json_bytes::Value as JSON;
use shape::Shape;

use super::immutable::InputPath;
use super::location::WithRange;
use super::ApplyToError;
use super::MethodArgs;
use super::PathList;
use super::VarsWithPathsMap;

// Two kinds of methods: public ones and not-yet-public ones. The future ones
// have proposed implementations and tests, and some are even used within the
// tests of other methods, but are not yet exposed for use in connector schemas.
// Graduating to public status requires updated documentation, careful review,
// and team discussion to make sure the method is one we want to support
// long-term. Once we have a better story for checking method type signatures
// and versioning any behavioral changes, we should be able to expand/improve
// the list of public::* methods more quickly/confidently.
mod future;
mod public;

#[cfg(test)]
mod tests;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ArrowMethod {
    // Public methods:
    Echo,
    Map,
    Match,
    First,
    Last,
    Slice,
    Size,
    Entries,

    // Future methods:
    TypeOf,
    Eq,
    Then,
    MatchIf,
    Add,
    Sub,
    Mul,
    Div,
    Mod,
    Has,
    Get,
    Keys,
    Values,
    Not,
    Or,
    And,
}

#[macro_export]
macro_rules! impl_arrow_method {
    ($struct_name:ident, $impl_fn_name:ident, $shape_fn_name:ident) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        pub(super) struct $struct_name;
        impl $crate::sources::connect::json_selection::methods::ArrowMethodImpl for $struct_name {
            fn apply(
                &self,
                method_name: &WithRange<String>,
                method_args: Option<&MethodArgs>,
                data: &JSON,
                vars: &VarsWithPathsMap,
                input_path: &InputPath<JSON>,
                tail: &WithRange<PathList>,
            ) -> (Option<JSON>, Vec<ApplyToError>) {
                $impl_fn_name(method_name, method_args, data, vars, input_path, tail)
            }

            fn shape(
                &self,
                method_name: &WithRange<String>,
                method_args: Option<&MethodArgs>,
                input_shape: Shape,
                dollar_shape: Shape,
                named_var_shapes: &IndexMap<&str, Shape>,
            ) -> Shape {
                // TODO
                $shape_fn_name(
                    method_name,
                    method_args,
                    input_shape,
                    dollar_shape,
                    named_var_shapes,
                )
            }
        }
    };
}

pub(super) trait ArrowMethodImpl {
    fn apply(
        &self,
        method_name: &WithRange<String>,
        method_args: Option<&MethodArgs>,
        data: &JSON,
        vars: &VarsWithPathsMap,
        input_path: &InputPath<JSON>,
        tail: &WithRange<PathList>,
    ) -> (Option<JSON>, Vec<ApplyToError>);

    fn shape(
        &self,
        // Shape processing errors for methods can benefit from knowing the name
        // of the method and its source range. Note that ArrowMethodImpl::shape
        // is invoked for every invocation of a method, with appropriately
        // different source ranges.
        method_name: &WithRange<String>,
        // Most methods implementing ArrowMethodImpl::shape will need to know
        // the shapes of their arguments, which can be computed from MethodArgs
        // using the compute_output_shape method.
        method_args: Option<&MethodArgs>,
        // The input_shape is the shape of the @ variable, or the value from the
        // left hand side of the -> token.
        input_shape: Shape,
        // The dollar_shape is the shape of the $ variable, or the input object
        // associated with the closest enclosing subselection.
        dollar_shape: Shape,
        // Other variable shapes may also be provided here, though in general
        // variables and their subproperties can be represented abstractly using
        // $var.nested.property ShapeCase::Name shapes.
        named_var_shapes: &IndexMap<&str, Shape>,
    ) -> Shape;
}

// This Deref implementation allows us to call .apply(...) directly on the
// ArrowMethod enum.
impl std::ops::Deref for ArrowMethod {
    type Target = dyn ArrowMethodImpl;

    fn deref(&self) -> &Self::Target {
        match self {
            // Public methods:
            Self::Echo => &public::EchoMethod,
            Self::Map => &public::MapMethod,
            Self::Match => &public::MatchMethod,
            Self::First => &public::FirstMethod,
            Self::Last => &public::LastMethod,
            Self::Slice => &public::SliceMethod,
            Self::Size => &public::SizeMethod,
            Self::Entries => &public::EntriesMethod,

            // Future methods:
            Self::TypeOf => &future::TypeOfMethod,
            Self::Eq => &future::EqMethod,
            Self::Then => &future::ThenMethod,
            Self::MatchIf => &future::MatchIfMethod,
            Self::Add => &future::AddMethod,
            Self::Sub => &future::SubMethod,
            Self::Mul => &future::MulMethod,
            Self::Div => &future::DivMethod,
            Self::Mod => &future::ModMethod,
            Self::Has => &future::HasMethod,
            Self::Get => &future::GetMethod,
            Self::Keys => &future::KeysMethod,
            Self::Values => &future::ValuesMethod,
            Self::Not => &future::NotMethod,
            Self::Or => &future::OrMethod,
            Self::And => &future::AndMethod,
        }
    }
}

impl ArrowMethod {
    // This method is currently used at runtime to look up methods by &str name,
    // but it could be hoisted parsing time, and then we'd store an ArrowMethod
    // instead of a String for the method name in the AST.
    pub(super) fn lookup(name: &str) -> Option<Self> {
        let method_opt = match name {
            "echo" => Some(Self::Echo),
            "map" => Some(Self::Map),
            "eq" => Some(Self::Eq),
            "then" => Some(Self::Then),
            "match" => Some(Self::Match),
            // As this case suggests, we can't necessarily provide a name()
            // method for ArrowMethod (the opposite of lookup), because method
            // implementations can be used under multiple names.
            "matchIf" | "match_if" => Some(Self::MatchIf),
            "typeof" => Some(Self::TypeOf),
            "add" => Some(Self::Add),
            "sub" => Some(Self::Sub),
            "mul" => Some(Self::Mul),
            "div" => Some(Self::Div),
            "mod" => Some(Self::Mod),
            "first" => Some(Self::First),
            "last" => Some(Self::Last),
            "slice" => Some(Self::Slice),
            "size" => Some(Self::Size),
            "has" => Some(Self::Has),
            "get" => Some(Self::Get),
            "keys" => Some(Self::Keys),
            "values" => Some(Self::Values),
            "entries" => Some(Self::Entries),
            "not" => Some(Self::Not),
            "or" => Some(Self::Or),
            "and" => Some(Self::And),
            _ => None,
        };

        match method_opt {
            Some(method) if cfg!(test) || method.is_public() => Some(method),
            _ => None,
        }
    }

    pub(super) fn is_public(&self) -> bool {
        // This set controls which ->methods are exposed for use in connector
        // schemas. Non-public methods are still implemented and tested, but
        // will not be returned from lookup_arrow_method outside of tests.
        matches!(
            self,
            Self::Echo
                | Self::Map
                | Self::Match
                | Self::First
                | Self::Last
                | Self::Slice
                | Self::Size
                | Self::Entries
        )
    }
}
