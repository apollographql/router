use apollo_router_error_derive::Error;
pub enum BasicError {
    #[error("Basic error occurred")]
    #[diagnostic(code(apollo_router::service::basic_error))]
    BasicError,
    #[error("Config error: {message}")]
    #[diagnostic(
        code(apollo_router::service::config_error),
        help("Check your configuration")
    )]
    ConfigError { message: String },
}
#[automatically_derived]
impl ::core::fmt::Debug for BasicError {
    #[inline]
    fn fmt(&self, f: &mut ::core::fmt::Formatter) -> ::core::fmt::Result {
        match self {
            BasicError::BasicError => ::core::fmt::Formatter::write_str(f, "BasicError"),
            BasicError::ConfigError { message: __self_0 } => {
                ::core::fmt::Formatter::debug_struct_field1_finish(
                    f,
                    "ConfigError",
                    "message",
                    &__self_0,
                )
            }
        }
    }
}
#[allow(unused_qualifications)]
#[automatically_derived]
impl ::thiserror::__private::Error for BasicError {}
#[allow(unused_qualifications)]
#[automatically_derived]
impl ::core::fmt::Display for BasicError {
    fn fmt(&self, __formatter: &mut ::core::fmt::Formatter) -> ::core::fmt::Result {
        use ::thiserror::__private::AsDisplay as _;
        #[allow(unused_variables, deprecated, clippy::used_underscore_binding)]
        match self {
            BasicError::BasicError {} => __formatter.write_str("Basic error occurred"),
            BasicError::ConfigError { message } => {
                match (message.as_display(),) {
                    (__display_message,) => {
                        __formatter
                            .write_fmt(
                                format_args!("Config error: {0}", __display_message),
                            )
                    }
                }
            }
        }
    }
}
impl miette::Diagnostic for BasicError {
    fn code(&self) -> std::option::Option<std::boxed::Box<dyn std::fmt::Display + '_>> {
        #[allow(unused_variables, deprecated)]
        match self {
            Self::BasicError => {
                std::option::Option::Some(
                    std::boxed::Box::new("apollo_router::service::basic_error"),
                )
            }
            Self::ConfigError { .. } => {
                std::option::Option::Some(
                    std::boxed::Box::new("apollo_router::service::config_error"),
                )
            }
            _ => std::option::Option::None,
        }
    }
    fn help(&self) -> std::option::Option<std::boxed::Box<dyn std::fmt::Display + '_>> {
        #[allow(unused_variables, deprecated)]
        match self {
            Self::ConfigError { message } => {
                std::option::Option::Some(
                    std::boxed::Box::new(
                        ::alloc::__export::must_use({
                            let res = ::alloc::fmt::format(
                                format_args!("Check your configuration"),
                            );
                            res
                        }),
                    ),
                )
            }
            _ => std::option::Option::None,
        }
    }
}
const _: fn() = || {
    fn assert_error<T: std::error::Error>() {}
    fn assert_diagnostic<T: miette::Diagnostic>() {}
    fn assert_debug<T: std::fmt::Debug>() {}
    assert_error::<BasicError>();
    assert_diagnostic::<BasicError>();
    assert_debug::<BasicError>();
};
impl apollo_router_error::Error for BasicError {
    fn error_code(&self) -> &'static str {
        match self {
            Self::BasicError => "apollo_router::service::basic_error",
            Self::ConfigError { .. } => "apollo_router::service::config_error",
        }
    }
    fn populate_graphql_extensions(
        &self,
        extensions_map: &mut std::collections::BTreeMap<String, serde_json::Value>,
    ) {
        match self {
            Self::BasicError => {
                extensions_map
                    .insert(
                        "errorType".to_string(),
                        serde_json::Value::String("BASIC_ERROR".to_string()),
                    );
            }
            Self::ConfigError { .. } => {
                extensions_map
                    .insert(
                        "errorType".to_string(),
                        serde_json::Value::String("CONFIG_ERROR".to_string()),
                    );
            }
        }
    }
}
