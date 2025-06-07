use apollo_router_error_derive::Error;
pub enum InferenceError {
    #[error("Syntax error")]
    #[diagnostic(code(apollo_router::service::syntax_error))]
    SyntaxError,
    #[error("Config error")]
    #[diagnostic(code(apollo_router::service::config_error))]
    ConfigError,
    #[error("Timeout error")]
    #[diagnostic(code(apollo_router::service::timeout))]
    TimeoutError,
    #[error("Network error")]
    #[diagnostic(code(apollo_router::service::network_error))]
    NetworkError,
    #[error("Conversion error")]
    #[diagnostic(code(apollo_router::service::conversion_error))]
    ConversionError,
    #[error("JSON error")]
    #[diagnostic(code(apollo_router::service::json_error))]
    JsonError,
}
#[automatically_derived]
impl ::core::fmt::Debug for InferenceError {
    #[inline]
    fn fmt(&self, f: &mut ::core::fmt::Formatter) -> ::core::fmt::Result {
        ::core::fmt::Formatter::write_str(
            f,
            match self {
                InferenceError::SyntaxError => "SyntaxError",
                InferenceError::ConfigError => "ConfigError",
                InferenceError::TimeoutError => "TimeoutError",
                InferenceError::NetworkError => "NetworkError",
                InferenceError::ConversionError => "ConversionError",
                InferenceError::JsonError => "JsonError",
            },
        )
    }
}
#[allow(unused_qualifications)]
#[automatically_derived]
impl ::thiserror::__private::Error for InferenceError {}
#[allow(unused_qualifications)]
#[automatically_derived]
impl ::core::fmt::Display for InferenceError {
    fn fmt(&self, __formatter: &mut ::core::fmt::Formatter) -> ::core::fmt::Result {
        #[allow(unused_variables, deprecated, clippy::used_underscore_binding)]
        match self {
            InferenceError::SyntaxError {} => __formatter.write_str("Syntax error"),
            InferenceError::ConfigError {} => __formatter.write_str("Config error"),
            InferenceError::TimeoutError {} => __formatter.write_str("Timeout error"),
            InferenceError::NetworkError {} => __formatter.write_str("Network error"),
            InferenceError::ConversionError {} => {
                __formatter.write_str("Conversion error")
            }
            InferenceError::JsonError {} => __formatter.write_str("JSON error"),
        }
    }
}
impl miette::Diagnostic for InferenceError {
    fn code(&self) -> std::option::Option<std::boxed::Box<dyn std::fmt::Display + '_>> {
        #[allow(unused_variables, deprecated)]
        match self {
            Self::SyntaxError => {
                std::option::Option::Some(
                    std::boxed::Box::new("apollo_router::service::syntax_error"),
                )
            }
            Self::ConfigError => {
                std::option::Option::Some(
                    std::boxed::Box::new("apollo_router::service::config_error"),
                )
            }
            Self::TimeoutError => {
                std::option::Option::Some(
                    std::boxed::Box::new("apollo_router::service::timeout"),
                )
            }
            Self::NetworkError => {
                std::option::Option::Some(
                    std::boxed::Box::new("apollo_router::service::network_error"),
                )
            }
            Self::ConversionError => {
                std::option::Option::Some(
                    std::boxed::Box::new("apollo_router::service::conversion_error"),
                )
            }
            Self::JsonError => {
                std::option::Option::Some(
                    std::boxed::Box::new("apollo_router::service::json_error"),
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
    assert_error::<InferenceError>();
    assert_diagnostic::<InferenceError>();
    assert_debug::<InferenceError>();
};
impl apollo_router_error::Error for InferenceError {
    fn error_code(&self) -> &'static str {
        match self {
            Self::SyntaxError => "apollo_router::service::syntax_error",
            Self::ConfigError => "apollo_router::service::config_error",
            Self::TimeoutError => "apollo_router::service::timeout",
            Self::NetworkError => "apollo_router::service::network_error",
            Self::ConversionError => "apollo_router::service::conversion_error",
            Self::JsonError => "apollo_router::service::json_error",
        }
    }
    fn populate_graphql_extensions(
        &self,
        details: &mut std::collections::HashMap<String, serde_json::Value>,
    ) {
        match self {
            Self::SyntaxError => {
                details
                    .insert(
                        "errorType".to_string(),
                        serde_json::Value::String("syntax".to_string()),
                    );
            }
            Self::ConfigError => {
                details
                    .insert(
                        "errorType".to_string(),
                        serde_json::Value::String("config".to_string()),
                    );
            }
            Self::TimeoutError => {
                details
                    .insert(
                        "errorType".to_string(),
                        serde_json::Value::String("timeout".to_string()),
                    );
            }
            Self::NetworkError => {
                details
                    .insert(
                        "errorType".to_string(),
                        serde_json::Value::String("network".to_string()),
                    );
            }
            Self::ConversionError => {
                details
                    .insert(
                        "errorType".to_string(),
                        serde_json::Value::String("conversion".to_string()),
                    );
            }
            Self::JsonError => {
                details
                    .insert(
                        "errorType".to_string(),
                        serde_json::Value::String("json".to_string()),
                    );
            }
        }
    }
}
