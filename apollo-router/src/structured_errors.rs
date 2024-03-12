use std::collections::HashMap;
use std::collections::HashSet;
use std::fmt::Display;

use displaydoc::Display;
use heck::ToShoutySnakeCase;
use linkme::distributed_slice;
use rust_embed::RustEmbed;
use schemars::schema::Schema;
use serde::Deserialize;
use serde::Serialize;

use crate::json_ext::Object;
use crate::json_ext::Path;

#[derive(Debug, thiserror::Error, Display)]
pub enum Error {
    /// unable to serialize the error
    Serialization {
        /// the error code of the error that could not be serialized
        error_code: String,
        /// the serialization error
        source: serde_json::Error,
    },
    /// the serialized error was not a json object
    SerializedWasNotObject {
        /// the error code of the error
        error_code: String,
    },
}

#[derive(RustEmbed)]
#[folder = "resources/errors"]
struct Asset;

#[derive(Serialize, Deserialize, Display)]
#[serde(rename_all = "snake_case")]
pub enum Level {
    /// Info
    Info,
    /// Warn
    Warn,
    /// Error
    Error,
}

#[derive(Serialize, Deserialize, Display)]
#[serde(rename_all = "snake_case")]
pub enum ErrorType {
    /// This indicates a problem with the request. Retrying the same request is not likely to succeed. An example would be a query or argument that cannot be deserialized.	400 Bad Request
    BadRequest,
    /// The operation was rejected because the system is not in a state required for the operation’s execution. For example, the directory to be deleted is non-empty, an rmdir operation is applied to a non-directory, etc. Use UNAVAILABLE instead if the client can retry just the failing call without waiting for the system state to be explicitly fixed.	400 Bad Request, or 500 Internal Server Error
    FailedPrecondition,
    /// This indicates that an unexpected internal error was encountered: some invariants expected by the underlying system have been broken. This error code is reserved for serious errors.	500 Internal Server Error
    Internal,
    /// This could apply to a resource that has never existed (e.g. bad resource id), or a resource that no longer exists (e.g. cache expired). Note to server developers: if a request is denied for an entire class of users, such as gradual feature rollout or undocumented allowlist, NOT_FOUND may be used. If a request is denied for some users within a class of users, such as user-based access control, PERMISSION_DENIED must be used.	404 Not Found
    NotFound,
    /// This indicates that the requester does not have permission to execute the specified operation. PERMISSION_DENIED must not be used for rejections caused by exhausting some resource or quota. PERMISSION_DENIED must not be used if the caller cannot be identified (use UNAUTHENTICATED instead for those errors). This error does not imply that the request is valid or the requested entity exists or satisfies other pre-conditions.	403 Forbidden
    PermissionDenied,
    /// This indicates that the request does not have valid authentication credentials but the route requires authentication.	401 Unauthorized
    Unauthenticated,
    /// This indicates that the service is currently unavailable. This is most likely a transient condition, which can be corrected by retrying with a backoff.	503 Unavailable
    Unavailable,
    /// This error may be returned, for example, when an error code received from another address space belongs to an error space that is not known in this address space. Errors raised by APIs that do not return enough error information may also be converted to this error. If a client sees an errorType that is not known to it, it will be interpreted as UNKNOWN. Unknown errors must not trigger any special behavior. They may be treated by an implementation as being equivalent to INTERNAL.
    Unknown,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Attribute {
    #[serde(rename = "type")]
    ty: String,
    description: String,
}

/// The definition of a structured error.
/// It provides extra information that is not contained on the error enum itself.
/// This information is usually drawn from an external file to enable easy localization and documentation.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StructuredErrorDefinition {
    /// The severity of the error
    pub level: Level,
    /// The error code, This will be of the form ERROR_ENUM_NAME__VARIANT_NAME
    pub code: String,
    /// A course description of the error sufficient for client side branching logic.
    #[serde(rename="type")]
    pub ty: ErrorType,
    /// The origin that the error is related to, Free format, suggested values are mentioned in errors/schema.json
    pub origin: String,
    /// The optional details of the error, this is a fixed value that goes into more detail than the error message that should contain when this errors can occur.
    pub detail: Option<String>,
    /// The attributes associated with this error. These are the fields that are present on the error enum variant.
    pub attributes: HashMap<String, Attribute>,
    /// Action that the user may want to take.
    pub actions: Vec<String>,
}

pub trait StructuredError: Display + std::error::Error + 'static {
    /// Returns the definition for the error.
    fn definition(&self) -> &'static StructuredErrorDefinition;

    /// Returns the error code.
    fn code(&self) -> &'static str {
        &self.definition().code
    }

    /// Returns the error message. This is dynamic and can change based on the error attributes.
    fn message(&self) -> String {
        self.to_string()
    }

    /// Returns the serde json value of the error attributes.
    fn attributes(&self) -> Result<Object, Error>;
}

/// A utility method to validate the error definitions are in sync with the related yaml file.
#[cfg(test)]
pub(crate) fn validate_definitions(
    definitions: &'static [StructuredErrorDefinition],
    expected_error_codes: HashSet<&String>,
    mut schema: schemars::schema::RootSchema,
) {
    // Check for duplicates
    let mut error_codes_from_yaml = std::collections::HashSet::new();
    for definition in definitions {
        if !error_codes_from_yaml.insert(&definition.code) {
            panic!("duplicate error definition for code {}", definition.code);
        }
    }

    // Check for missing errors
    for name in &expected_error_codes {
        if !error_codes_from_yaml.contains(name) {
            panic!("missing error definition for code {}", name);
        }
    }

    // Check for extra errors in the yaml
    for definition in definitions {
        if !expected_error_codes.contains(&definition.code) {
            panic!("extra error definition for code {}", definition.code);
        }
    }

    let ty = schema
        .schema
        .metadata
        .as_ref()
        .and_then(|s| s.title.clone())
        .unwrap_or_default();
    for definition in definitions {
        if let Some(one_of) = &mut schema.schema.subschemas().one_of {
            for variant in one_of {
                if let Schema::Object(o) = variant {
                    if let Some(object) = &o.object {
                        if let Some(variant_name) = object.properties.get("__type") {
                            if let Schema::Object(schema) = variant_name {
                                if let Some(enum_values) = &schema.enum_values {
                                    if enum_values.iter().any(|v| {
                                        format!(
                                            "{}__{}",
                                            ty.to_shouty_snake_case(),
                                            v.as_str().unwrap_or_default().to_shouty_snake_case()
                                        ) == definition.code
                                    }) {
                                        for (attr, schema) in &object.properties {
                                            if attr != "__type" {
                                                if let Schema::Object(schema) = schema {
                                                    if let Some(description) = &schema
                                                        .metadata
                                                        .as_ref()
                                                        .and_then(|s| s.description.clone())
                                                    {
                                                        if let Some(attr_definition) =
                                                            definition.attributes.get(attr)
                                                        {
                                                            if let Some(description) = &schema
                                                                .metadata
                                                                .as_ref()
                                                                .and_then(|s| s.description.clone())
                                                            {
                                                                assert_eq!(&attr_definition.description, description, "description mismatch for attribute {} for error {}", attr, definition.code);
                                                                continue;
                                                            }
                                                        }
                                                        panic!("missing yaml doc for attribute {} for error {} with doc: {}", attr, definition.code, description);
                                                    }
                                                }
                                                panic!("missing rust doc for attribute {} for error {}", attr, definition.code);
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

// This contains a slice of StructuredErrorCast which allows casting from a dyn Error to a dyn StructuredError
#[distributed_slice]
pub static STRUCTURED_ERROR_CAST: [&'static dyn StructuredErrorCast];

/// An extension trait that allows casting from a dyn Error to a `dyn StructuredError` using `as_structured_error_ref`
pub trait AnyStructuredError<'a> {
    fn as_structured_error_ref(self) -> Option<&'a (dyn StructuredError)>;
}
impl<'a> AnyStructuredError<'a> for &'a (dyn std::error::Error + 'static) {
    fn as_structured_error_ref(self) -> Option<&'a (dyn StructuredError)> {
        for cast in STRUCTURED_ERROR_CAST {
            if let Some(e) = cast.cast_structured_error_ref(self) {
                return Some(e);
            }
        }
        None
    }
}

/// A trait that allows casting from a `dyn Error` to a `dyn StructuredError`
pub(crate) trait StructuredErrorCast: Sync + Send {
    fn cast_structured_error_ref<'a>(
        &'a self,
        error: &'a (dyn std::error::Error + 'static),
    ) -> Option<&'a (dyn StructuredError)>;
}

/// The default and only implementation of `StructuredErrorCast`
struct DefaultStructuredErrorCast<T>
where
    T: StructuredError + Send + Sync + std::error::Error + 'static,
{
    _phantom: std::marker::PhantomData<T>,
}

/// The default and only implementation of `StructuredErrorCast,` essentially does a `downcast_ref` to `T` and then returns it as a reference to `dyn StructuredError`
impl<T> StructuredErrorCast for DefaultStructuredErrorCast<T>
where
    T: StructuredError + Send + Sync + std::error::Error + 'static,
{
    fn cast_structured_error_ref<'a>(
        &'a self,
        error: &'a (dyn std::error::Error + 'static),
    ) -> Option<&'a (dyn StructuredError)> {
        if let Some(e) = error.downcast_ref::<T>() {
            return Some(e);
        }
        None
    }
}

/// Macro for creating errors that implement `StructuredError`. It automatically adds a bunch of required derives and provides the implementation of the `StructuredError` trait.
/// Usage:
/// ```
/// error_type!("Test error docs", TestError, {
///         /// Not found
///         NotFound {
///             /// Some attribute
///             attr: String,
///             #[serde(skip)]
///             source: Box<dyn std::error::Error + Send + Sync>,
///         },
///         /// Bad request
///         BadRequest,
///     });
/// ```
macro_rules! error_type {
    ($doc:literal, $name:ident, $definition:tt) => {
        paste::paste! {
            #[doc=$doc]
            #[derive(
                strum_macros::IntoStaticStr,
                strum_macros::EnumDiscriminants,
                thiserror::Error,
                displaydoc::Display,
                Debug,
                serde::Serialize,
                ordinalizer::Ordinal
            )]
            #[cfg_attr(test, derive(schemars::JsonSchema))]
            #[strum_discriminants(name([<$name Discriminant>]))]
            #[strum_discriminants(derive(
                ordinalizer::Ordinal,
                strum_macros::EnumIter,
                strum_macros::IntoStaticStr
            ))]
            #[serde(tag = "__type")]
            enum $name $definition

            impl $name {
                 fn definitions() -> &'static [crate::structured_errors::StructuredErrorDefinition] where Self: Sized{
                    static ALL_ERROR_DEFINITIONS: std::sync::OnceLock<Vec<crate::structured_errors::StructuredErrorDefinition>> = std::sync::OnceLock::new();
                    ALL_ERROR_DEFINITIONS.get_or_init(|| {
                        // This only happens once.
                        // We load all the error definitions, and sort them in the order that they appear in the enum so that we can look them up O(1).
                        // We check the definitions to ensure that there are:
                        // - No duplicate errors defined.
                        // - No missing errors.
                        // - No extra errors that do not appear in the error enum.
                        let discriminants = Self::discriminants();
                        let yaml_file_name = format!("{}.yaml", std::any::type_name::<$name>());
                        let embedded_file = crate::structured_errors::Asset::get(&yaml_file_name)
                            .expect(&format!("missing error definition file {}", yaml_file_name));
                        let all_error_definitions: Vec<crate::structured_errors::StructuredErrorDefinition> =
                            serde_yaml::from_slice(&embedded_file.data).expect(&format!(
                                "error parsing error definitions file {}",
                                yaml_file_name
                            ));

                        // Finally rearrange the definitions in the order that they appear in the enum.
                        let mut definitions = Vec::with_capacity(discriminants.len());
                        definitions.resize_with(discriminants.len(), Default::default);
                        for definition in all_error_definitions {
                            let ordinal = discriminants[&definition.code];
                            definitions[ordinal] = Some(definition);
                        }

                        definitions
                            .into_iter()
                            .map(|v| v.expect("error definition must have been found"))
                            .collect()
                    })
                }

                /// Returns the ordinal of the error by code, This is slow, but is only called when populating ALL_ERROR_DEFINITIONS so that O(1) lookups can be done later.
                fn discriminants() -> std::collections::HashMap<String, usize> {
                    use heck::ToShoutySnakeCase;
                    use strum::IntoEnumIterator;
                    [<$name Discriminant>]::iter()
                        .map(|v| {
                            let code: &'static str = v.into();
                            (format!("{}__{}", stringify!($name).to_shouty_snake_case(), code.to_shouty_snake_case()), v.ordinal())
                        })
                        .collect()
                }

            }

            impl crate::structured_errors::StructuredError for $name {
                /// Returns the error attributes as a serde_json::Value
                fn attributes(&self) -> Result<crate::json_ext::Object, crate::structured_errors::Error> {
                    let object = serde_json_bytes::to_value(self).map_err(|e|crate::structured_errors::Error::Serialization {
                        error_code: self.code().to_string(),
                        source: e,
                    })?;
                    match object {
                        serde_json_bytes::Value::Object(mut object) => {
                            object.remove("__type");
                            Ok(object)
                        },
                        serde_json_bytes::Value::Null => Ok(crate::json_ext::Object::new()),
                        _ => Err(crate::structured_errors::Error::SerializedWasNotObject {
                            error_code: self.code().to_string()
                        })
                    }
                }

                /// Returns the definition for the error.
                /// This is O(1) lookup because the error definitions are sorted in the order that they appear in the enum.
                fn definition(&self) -> &'static crate::structured_errors::StructuredErrorDefinition {
                    // O(1) lookup
                    &$name::definitions()[$name::ordinal(self)]
                }
            }

            #[linkme::distributed_slice(crate::structured_errors::STRUCTURED_ERROR_CAST)]
            static [<$name:snake:upper _CAST>] : &'static dyn crate::structured_errors::StructuredErrorCast = &crate::structured_errors::DefaultStructuredErrorCast::<$name>{_phantom: std::marker::PhantomData{}};

            #[cfg(test)]
            #[test]
            fn [<test_ $name:snake _definitions>]() {
                let definitions = $name::definitions();
                let descriminants = $name::discriminants();
                crate::structured_errors::validate_definitions(definitions, descriminants.keys().collect(), schemars::schema_for!($name));
            }
        }
    };
}

/// An error formatter that can be used to convert errors that implement `StructuredError` to graphql errors
pub trait ErrorFormatter {
    /// Convert the error into a graphql error.
    fn format_error<T: StructuredError + ?Sized>(
        &self,
        error: &T,
        locations: Vec<crate::graphql::Location>,
        path: Option<Path>,
    ) -> Result<crate::graphql::Error, Error>;
}

#[cfg(test)]
mod test {
    use displaydoc::Display;
    use insta::assert_yaml_snapshot;

    use crate::json_ext::Object;
    use crate::json_ext::Path;
    use crate::structured_errors::AnyStructuredError;
    use crate::structured_errors::ErrorFormatter;
    use crate::structured_errors::StructuredError;

    error_type!("Test error docs", TestNestedError, {
        /// Nested error
        Nested,
    });

    error_type!("Test error docs", TestError, {
        /// Not found
        NotFound {
            /// Some attribute
            attr: String,
            #[serde(skip)]
            source: Box<dyn std::error::Error + Send + Sync>,
        },
        /// Bad request
        BadRequest,
    });

    #[derive(Debug, thiserror::Error, Display)]
    enum NonStructuredError {
        /// Some unstructured error
        Unstructured {

            source: Box<dyn std::error::Error + Send + Sync>,
        },
    }

    struct SimpleErrorFormatter;
    impl ErrorFormatter for SimpleErrorFormatter {
        fn format_error<T: StructuredError + ?Sized>(
            &self,
            error: &T,
            locations: Vec<crate::graphql::Location>,
            path: Option<Path>,
        ) -> Result<crate::graphql::Error, super::Error> {
            let mut attributes = error.attributes()?;
            let mut cause = error.source();
            let mut trace = vec![];
            while let Some(err) = cause {
                if let Some(err) = err.as_structured_error_ref() {
                    let mut o = Object::new();
                    o.insert("code".to_string(), err.code().to_string().into());
                    o.insert("message".to_string(), err.message().into());
                    let attr = err.attributes()?;
                    if !attr.is_empty() {
                        o.insert("attributes".to_string(), err.attributes()?.into());
                    }
                    trace.push(o);
                } else {
                    // You may do some manual downcasting here
                    let mut o = Object::new();
                    o.insert("code".to_string(), "UNKNOWN".into());
                    o.insert("message".to_string(), err.to_string().into());
                    trace.push(o);
                }
                cause = err.source();
            }
            attributes.insert("code".to_string(), error.code().into());
            attributes.insert(
                "level".to_string(),
                error.definition().level.to_string().into(),
            );
            if !trace.is_empty() {
                attributes.insert("trace".to_string(), trace.into());
            }

            Ok(crate::graphql::Error {
                message: error.message(),
                extensions: attributes,
                locations,
                path,
            })
        }
    }

    #[test]
    fn test_error_definition() {
        let error = TestError::NotFound {
            attr: "test".to_string(),
            source: Box::new(TestNestedError::Nested),
        };
        assert_yaml_snapshot!(error.definition());
    }

    #[test]
    fn test_error_formatting() {
        let error = TestError::BadRequest;
        assert_eq!(error.code(), "TEST_ERROR__BAD_REQUEST");
        assert_eq!(error.message(), "Bad request");
        assert_eq!(
            error.definition().detail,
            Some("The request was invalid or cannot be otherwise served.".to_string())
        );
        assert_eq!(
            serde_json::to_string(&error.attributes().unwrap()).unwrap(),
            "{}".to_string()
        );
        assert_yaml_snapshot!(SimpleErrorFormatter
            .format_error(&error, Default::default(), None)
            .unwrap());
    }

    #[test]
    fn test_error_formatting_with_source_chain() {
        let error = TestError::NotFound {
            attr: "test".to_string(),
            source: Box::new(NonStructuredError::Unstructured { source: Box::new(TestNestedError::Nested)}),
        };
        assert_eq!(error.code(), "TEST_ERROR__NOT_FOUND");
        assert_eq!(error.message(), "Not found");
        assert_eq!(
            error.definition().detail,
            Some("The requested resource could not be found but may be available in the future.".to_string())
        );
        assert_eq!(
            serde_json::to_string(&error.attributes().unwrap()).unwrap(),
            "{\"attr\":\"test\"}".to_string()
        );
        assert_yaml_snapshot!(SimpleErrorFormatter
            .format_error(&error, Default::default(), None)
            .unwrap());
    }
}
