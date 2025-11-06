use serde_json_bytes::Value as JSON;
use shape::Shape;

use super::ApplyToError;
use super::MethodArgs;
use super::VarsWithPathsMap;
use super::immutable::InputPath;
use super::location::WithRange;
use crate::connectors::json_selection::ShapeContext;
use crate::connectors::spec::ConnectSpec;

mod common;

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
    As,
    Echo,
    Map,
    Match,
    First,
    Last,
    Slice,
    Size,
    Entries,
    JsonStringify,
    JoinNotNull,
    Filter,
    Find,
    Gte,
    Lte,
    Eq,
    Ne,
    Or,
    And,
    Gt,
    Lt,
    Not,
    In,
    Contains,
    Get,
    ToString,
    ParseInt,
    Add,
    Sub,
    Mul,
    Div,
    Mod,

    // Future methods:
    TypeOf,
    MatchIf,
    Has,
    Keys,
    Values,
}

#[macro_export]
macro_rules! impl_arrow_method {
    ($struct_name:ident, $impl_fn_name:ident, $shape_fn_name:ident) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        pub(crate) struct $struct_name;
        impl $crate::connectors::json_selection::methods::ArrowMethodImpl for $struct_name {
            fn apply(
                &self,
                method_name: &WithRange<String>,
                method_args: Option<&MethodArgs>,
                data: &JSON,
                vars: &VarsWithPathsMap,
                input_path: &InputPath<JSON>,
                spec: $crate::connectors::spec::ConnectSpec,
            ) -> (Option<JSON>, Vec<ApplyToError>) {
                $impl_fn_name(method_name, method_args, data, vars, input_path, spec)
            }

            fn shape(
                &self,
                context: &$crate::connectors::json_selection::apply_to::ShapeContext,
                method_name: &WithRange<String>,
                method_args: Option<&MethodArgs>,
                input_shape: Shape,
                dollar_shape: Shape,
            ) -> Shape {
                $shape_fn_name(context, method_name, method_args, input_shape, dollar_shape)
            }
        }
    };
}

#[allow(dead_code)] // method type-checking disabled until we add name resolution
pub(super) trait ArrowMethodImpl {
    fn apply(
        &self,
        method_name: &WithRange<String>,
        method_args: Option<&MethodArgs>,
        data: &JSON,
        vars: &VarsWithPathsMap,
        input_path: &InputPath<JSON>,
        spec: ConnectSpec,
    ) -> (Option<JSON>, Vec<ApplyToError>);

    fn shape(
        &self,
        context: &ShapeContext,
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
    ) -> Shape;
}

// This Deref implementation allows us to call .apply(...) directly on the
// ArrowMethod enum.
impl std::ops::Deref for ArrowMethod {
    type Target = dyn ArrowMethodImpl;

    fn deref(&self) -> &Self::Target {
        match self {
            // Public methods:
            Self::As => &public::AsMethod,
            Self::Echo => &public::EchoMethod,
            Self::Map => &public::MapMethod,
            Self::Match => &public::MatchMethod,
            Self::First => &public::FirstMethod,
            Self::Last => &public::LastMethod,
            Self::Slice => &public::SliceMethod,
            Self::Size => &public::SizeMethod,
            Self::Entries => &public::EntriesMethod,
            Self::JsonStringify => &public::JsonStringifyMethod,
            Self::JoinNotNull => &public::JoinNotNullMethod,
            Self::Filter => &public::FilterMethod,
            Self::Find => &public::FindMethod,
            Self::Gte => &public::GteMethod,
            Self::Lte => &public::LteMethod,
            Self::Eq => &public::EqMethod,
            Self::Ne => &public::NeMethod,
            Self::Or => &public::OrMethod,
            Self::And => &public::AndMethod,
            Self::Gt => &public::GtMethod,
            Self::Lt => &public::LtMethod,
            Self::Not => &public::NotMethod,
            Self::In => &public::InMethod,
            Self::Contains => &public::ContainsMethod,
            Self::Get => &public::GetMethod,
            Self::ToString => &public::ToStringMethod,
            Self::ParseInt => &public::ParseIntMethod,
            Self::Add => &public::AddMethod,
            Self::Sub => &public::SubMethod,
            Self::Mul => &public::MulMethod,
            Self::Div => &public::DivMethod,
            Self::Mod => &public::ModMethod,

            // Future methods:
            Self::TypeOf => &future::TypeOfMethod,
            Self::MatchIf => &future::MatchIfMethod,
            Self::Has => &future::HasMethod,
            Self::Keys => &future::KeysMethod,
            Self::Values => &future::ValuesMethod,
        }
    }
}

impl ArrowMethod {
    // This method is currently used at runtime to look up methods by &str name,
    // but it could be hoisted parsing time, and then we'd store an ArrowMethod
    // instead of a String for the method name in the AST.
    pub(super) fn lookup(name: &str) -> Option<Self> {
        let method_opt = match name {
            "as" => Some(Self::As),
            "echo" => Some(Self::Echo),
            "map" => Some(Self::Map),
            "eq" => Some(Self::Eq),
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
            "jsonStringify" => Some(Self::JsonStringify),
            "joinNotNull" => Some(Self::JoinNotNull),
            "filter" => Some(Self::Filter),
            "find" => Some(Self::Find),
            "gte" => Some(Self::Gte),
            "lte" => Some(Self::Lte),
            "ne" => Some(Self::Ne),
            "gt" => Some(Self::Gt),
            "lt" => Some(Self::Lt),
            "in" => Some(Self::In),
            "contains" => Some(Self::Contains),
            "toString" => Some(Self::ToString),
            "parseInt" => Some(Self::ParseInt),
            _ => None,
        };

        match method_opt {
            Some(method) if cfg!(test) || method.is_public() => Some(method),
            _ => None,
        }
    }

    pub(super) const fn is_public(&self) -> bool {
        // This set controls which ->methods are exposed for use in connector
        // schemas. Non-public methods are still implemented and tested, but
        // will not be returned from lookup_arrow_method outside of tests.
        matches!(
            self,
            Self::As
                | Self::Echo
                | Self::Map
                | Self::Match
                | Self::First
                | Self::Last
                | Self::Slice
                | Self::Size
                | Self::Entries
                | Self::JsonStringify
                | Self::JoinNotNull
                | Self::Filter
                | Self::Find
                | Self::Gte
                | Self::Lte
                | Self::Eq
                | Self::Ne
                | Self::Or
                | Self::And
                | Self::Gt
                | Self::Lt
                | Self::Not
                | Self::In
                | Self::Contains
                | Self::Get
                | Self::ToString
                | Self::ParseInt
                | Self::Add
                | Self::Sub
                | Self::Mul
                | Self::Div
                | Self::Mod
        )
    }
}
