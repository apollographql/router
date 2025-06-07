use apollo_router_error_derive::Error;
pub enum ComplexError {
    #[error("Parse error: {reason}")]
    #[diagnostic(code(apollo_router::service::parse_error), help("Check syntax"))]
    ParseError {
        reason: String,
        line: u32,
        column: u32,
        #[source_code]
        source_text: Option<String>,
        #[label("Error here")]
        span: Option<String>,
    },
    #[error("Network error for {endpoint}")]
    #[diagnostic(code(apollo_router::service::network_error))]
    NetworkError { endpoint: String, #[source] io_error: String },
    #[error("JSON error: {0}")]
    #[diagnostic(code(apollo_router::service::json_error))]
    JsonError(String),
}
#[automatically_derived]
impl ::core::fmt::Debug for ComplexError {
    #[inline]
    fn fmt(&self, f: &mut ::core::fmt::Formatter) -> ::core::fmt::Result {
        match self {
            ComplexError::ParseError {
                reason: __self_0,
                line: __self_1,
                column: __self_2,
                source_text: __self_3,
                span: __self_4,
            } => {
                ::core::fmt::Formatter::debug_struct_field5_finish(
                    f,
                    "ParseError",
                    "reason",
                    __self_0,
                    "line",
                    __self_1,
                    "column",
                    __self_2,
                    "source_text",
                    __self_3,
                    "span",
                    &__self_4,
                )
            }
            ComplexError::NetworkError { endpoint: __self_0, io_error: __self_1 } => {
                ::core::fmt::Formatter::debug_struct_field2_finish(
                    f,
                    "NetworkError",
                    "endpoint",
                    __self_0,
                    "io_error",
                    &__self_1,
                )
            }
            ComplexError::JsonError(__self_0) => {
                ::core::fmt::Formatter::debug_tuple_field1_finish(
                    f,
                    "JsonError",
                    &__self_0,
                )
            }
        }
    }
}
#[allow(unused_qualifications)]
#[automatically_derived]
impl ::thiserror::__private::Error for ComplexError {
    fn source(
        &self,
    ) -> ::core::option::Option<&(dyn ::thiserror::__private::Error + 'static)> {
        use ::thiserror::__private::AsDynError as _;
        #[allow(deprecated)]
        match self {
            ComplexError::ParseError { .. } => ::core::option::Option::None,
            ComplexError::NetworkError { io_error: source, .. } => {
                ::core::option::Option::Some(source.as_dyn_error())
            }
            ComplexError::JsonError { .. } => ::core::option::Option::None,
        }
    }
}
#[allow(unused_qualifications)]
#[automatically_derived]
impl ::core::fmt::Display for ComplexError {
    fn fmt(&self, __formatter: &mut ::core::fmt::Formatter) -> ::core::fmt::Result {
        use ::thiserror::__private::AsDisplay as _;
        #[allow(unused_variables, deprecated, clippy::used_underscore_binding)]
        match self {
            ComplexError::ParseError { reason, line, column, source_text, span } => {
                match (reason.as_display(),) {
                    (__display_reason,) => {
                        __formatter
                            .write_fmt(
                                format_args!("Parse error: {0}", __display_reason),
                            )
                    }
                }
            }
            ComplexError::NetworkError { endpoint, io_error } => {
                match (endpoint.as_display(),) {
                    (__display_endpoint,) => {
                        __formatter
                            .write_fmt(
                                format_args!("Network error for {0}", __display_endpoint),
                            )
                    }
                }
            }
            ComplexError::JsonError(_0) => {
                match (_0.as_display(),) {
                    (__display0,) => {
                        __formatter
                            .write_fmt(format_args!("JSON error: {0}", __display0))
                    }
                }
            }
        }
    }
}
impl miette::Diagnostic for ComplexError {
    fn code(&self) -> std::option::Option<std::boxed::Box<dyn std::fmt::Display + '_>> {
        #[allow(unused_variables, deprecated)]
        match self {
            Self::ParseError { .. } => {
                std::option::Option::Some(
                    std::boxed::Box::new("apollo_router::service::parse_error"),
                )
            }
            Self::NetworkError { .. } => {
                std::option::Option::Some(
                    std::boxed::Box::new("apollo_router::service::network_error"),
                )
            }
            Self::JsonError(..) => {
                std::option::Option::Some(
                    std::boxed::Box::new("apollo_router::service::json_error"),
                )
            }
            _ => std::option::Option::None,
        }
    }
    fn help(&self) -> std::option::Option<std::boxed::Box<dyn std::fmt::Display + '_>> {
        #[allow(unused_variables, deprecated)]
        match self {
            Self::ParseError { reason, line, column, source_text, span } => {
                std::option::Option::Some(
                    std::boxed::Box::new(
                        ::alloc::__export::must_use({
                            let res = ::alloc::fmt::format(format_args!("Check syntax"));
                            res
                        }),
                    ),
                )
            }
            _ => std::option::Option::None,
        }
    }
    fn labels(
        &self,
    ) -> std::option::Option<
        std::boxed::Box<dyn std::iter::Iterator<Item = miette::LabeledSpan> + '_>,
    > {
        #[allow(unused_variables, deprecated)]
        match self {
            Self::ParseError { reason, line, column, source_text, span } => {
                use miette::macro_helpers::ToOption;
                let labels_iter = <[_]>::into_vec(
                        ::alloc::boxed::box_new([
                            miette::macro_helpers::OptionalWrapper::<
                                Option<String>,
                            >::new()
                                .to_option(span)
                                .map(|__miette_internal_var| miette::LabeledSpan::new_with_span(
                                    std::option::Option::Some(
                                        ::alloc::__export::must_use({
                                            let res = ::alloc::fmt::format(format_args!("Error here"));
                                            res
                                        }),
                                    ),
                                    __miette_internal_var.clone(),
                                )),
                        ]),
                    )
                    .into_iter();
                std::option::Option::Some(
                    std::boxed::Box::new(
                        labels_iter.filter(Option::is_some).map(Option::unwrap),
                    ),
                )
            }
            _ => std::option::Option::None,
        }
    }
    fn source_code(&self) -> std::option::Option<&dyn miette::SourceCode> {
        #[allow(unused_variables, deprecated)]
        match self {
            Self::ParseError { reason, line, column, source_text, span } => {
                source_text.as_ref().map(|s| s as _)
            }
            _ => std::option::Option::None,
        }
    }
}
const _: fn() = || {
    fn assert_error<T: std::error::Error>() {}
    fn assert_diagnostic<T: miette::Diagnostic>() {}
    fn assert_debug<T: std::fmt::Debug>() {}
    assert_error::<ComplexError>();
    assert_diagnostic::<ComplexError>();
    assert_debug::<ComplexError>();
};
impl apollo_router_error::Error for ComplexError {
    fn error_code(&self) -> &'static str {
        match self {
            Self::ParseError { .. } => "apollo_router::service::parse_error",
            Self::NetworkError { .. } => "apollo_router::service::network_error",
            Self::JsonError(..) => "apollo_router::service::json_error",
        }
    }
    fn populate_graphql_extensions(
        &self,
        extensions_map: &mut std::collections::BTreeMap<String, serde_json::Value>,
    ) {
        match self {
            Self::ParseError { .. } => {
                extensions_map
                    .insert(
                        "errorType".to_string(),
                        serde_json::Value::String("PARSE_ERROR".to_string()),
                    );
            }
            Self::NetworkError { .. } => {
                extensions_map
                    .insert(
                        "errorType".to_string(),
                        serde_json::Value::String("NETWORK_ERROR".to_string()),
                    );
            }
            Self::JsonError(..) => {
                extensions_map
                    .insert(
                        "errorType".to_string(),
                        serde_json::Value::String("JSON_ERROR".to_string()),
                    );
            }
        }
    }
}
