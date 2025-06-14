//! # Apollo Router Error Derive Macro
//!
//! This crate provides a derive macro that automatically:
//! - Implements the `Error` trait from apollo-router-error
//! - Registers errors with the global error registry using `linkme`
//! - Generates GraphQL extensions population code
//! - Extracts error codes from `#[diagnostic(code(...))]` attributes
//!
//! ## Usage
//!
//! ```rust
//! use apollo_router_error_derive::Error;
//!
//! #[derive(Debug, thiserror::Error, miette::Diagnostic, Error)]
//! pub enum MyError {
//!     #[error("Something went wrong: {message}")]
//!     #[diagnostic(
//!         code(apollo_router::my_service::something_wrong),
//!         help("Try doing something else")
//!     )]
//!     SomethingWrong {
//!         #[extension("errorMessage")]
//!         message: String,
//!         #[extension("timestamp")]  
//!         when: String,
//!         #[source]
//!         cause: Option<Box<dyn std::error::Error + Send + Sync>>,
//!     },
//!     
//!     #[error("Another error occurred")]
//!     #[diagnostic(code(apollo_router::my_service::another_error))]
//!     AnotherError,
//! }
//! ```
//!
//! ## Extension Fields
//!
//! Fields that should be included in GraphQL error extensions must be explicitly marked
//! with the `#[extension("extensionFieldName")]` attribute. This ensures only intended
//! fields are exposed and allows custom naming of extension fields.
//!
//! ## Required Derives
//!
//! The `Error` derive macro requires that your error enum also derives:
//! - `Debug` - For debugging support
//! - `thiserror::Error` - For standard error trait implementation
//! - `miette::Diagnostic` - For rich error diagnostics
//!
//! If any of these derives are missing, the compiler will provide helpful error messages
//! indicating which traits need to be implemented.

use inflector::Inflector;
use proc_macro::TokenStream;
use quote::format_ident;
use quote::quote;
use syn::Attribute;
use syn::Data;
use syn::DataEnum;
use syn::DeriveInput;
use syn::Fields;
use syn::Variant;
use syn::parse_macro_input;

/// Derive macro for automatically implementing Error and registering with the error registry
#[proc_macro_derive(Error, attributes(extension))]
pub fn derive_error(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);

    match generate_error_impl(input) {
        Ok(tokens) => tokens.into(),
        Err(err) => err.to_compile_error().into(),
    }
}

fn generate_error_impl(input: DeriveInput) -> syn::Result<proc_macro2::TokenStream> {
    let enum_name = &input.ident;
    let enum_data = match &input.data {
        Data::Enum(data) => data,
        _ => {
            return Err(syn::Error::new_spanned(
                &input,
                "Error can only be derived for enums",
            ));
        }
    };

    // Since proc macros can't see their own derive attribute, we'll generate
    // trait bounds that will give helpful compile errors if the required traits
    // are not implemented. This is a more robust approach than trying to detect derives.

    // Parse all variants and extract error information
    let variant_info = parse_error_variants(enum_data)?;

    // Generate the error_code implementation
    let error_code_arms = generate_error_code_match_arms(&variant_info);

    // Generate GraphQL extensions population implementation
    let graphql_extensions_arms = generate_graphql_extensions_arms(&variant_info);

    // Generate registry entry
    let registry_entry = generate_registry_entry(enum_name, &variant_info)?;

    // Generate GraphQL error handler registration
    let graphql_handler_entry = generate_graphql_handler_entry(enum_name)?;

    let expanded = quote! {
        // Compile-time check that required traits are implemented
        // This will produce helpful error messages if derives are missing
        const _: fn() = || {
            fn assert_error<T: std::error::Error>() {}
            fn assert_diagnostic<T: miette::Diagnostic>() {}
            fn assert_debug<T: std::fmt::Debug>() {}

            assert_error::<#enum_name>();
            assert_diagnostic::<#enum_name>();
            assert_debug::<#enum_name>();
        };

        impl apollo_router_error::Error for #enum_name {
            fn error_code(&self) -> &'static str {
                match self {
                    #(#error_code_arms)*
                }
            }

            fn populate_graphql_extensions(&self, extensions_map: &mut std::collections::BTreeMap<String, serde_json::Value>) {
                match self {
                    #(#graphql_extensions_arms)*
                }
            }
        }

        #registry_entry

        #graphql_handler_entry
    };

    Ok(expanded)
}

#[derive(Debug, Clone)]
struct ErrorVariantInfo {
    variant_name: syn::Ident,
    error_code: String,
    help_text: Option<String>,
    fields: Vec<ErrorFieldInfo>,
    is_tuple_variant: bool,
}

#[derive(Debug, Clone)]
struct ErrorFieldInfo {
    name: syn::Ident,
    extension_name: Option<String>,
}

fn parse_error_variants(enum_data: &DataEnum) -> syn::Result<Vec<ErrorVariantInfo>> {
    let mut variants = Vec::new();

    for variant in &enum_data.variants {
        let variant_info = parse_single_variant(variant)?;
        variants.push(variant_info);
    }

    Ok(variants)
}

fn parse_single_variant(variant: &Variant) -> syn::Result<ErrorVariantInfo> {
    let variant_name = variant.ident.clone();

    // Extract error code and help text from diagnostic attribute
    let (error_code, help_text) = extract_diagnostic_info(&variant.attrs)?;

    // Determine if this is a tuple variant
    let is_tuple_variant = matches!(variant.fields, Fields::Unnamed(_));

    // Parse fields for GraphQL extensions generation
    let fields = parse_variant_fields(&variant.fields)?;

    Ok(ErrorVariantInfo {
        variant_name,
        error_code,
        help_text,
        fields,
        is_tuple_variant,
    })
}

fn extract_diagnostic_info(attrs: &[Attribute]) -> syn::Result<(String, Option<String>)> {
    for attr in attrs {
        if attr.path().is_ident("diagnostic") {
            return parse_diagnostic_attribute(attr);
        }
    }

    Err(syn::Error::new_spanned(
        attrs.first(),
        "Missing #[diagnostic(code(...))] attribute. All error variants must have diagnostic codes.",
    ))
}

fn parse_diagnostic_attribute(attr: &Attribute) -> syn::Result<(String, Option<String>)> {
    let mut error_code = None;
    let mut help_text = None;

    // Parse attribute tokens
    attr.parse_nested_meta(|meta| {
        if meta.path.is_ident("code") {
            // Parse code(path::to::error)
            let content;
            syn::parenthesized!(content in meta.input);
            let path: syn::Path = content.parse()?;
            error_code = Some(path_to_string(&path));
            Ok(())
        } else if meta.path.is_ident("help") {
            // Parse help("text") or help = "text"
            if meta.input.peek(syn::Token![=]) {
                // Parse help = "text" format
                let value = meta.value()?;
                let lit_str = value.parse::<syn::LitStr>()?;
                help_text = Some(lit_str.value())
            } else {
                // Parse help("text") format
                let content;
                syn::parenthesized!(content in meta.input);
                let lit: syn::LitStr = content.parse()?;
                help_text = Some(lit.value());
            }
            Ok(())
        } else {
            // Skip other attributes like url(docsrs)
            Ok(())
        }
    })?;

    let error_code = error_code.ok_or_else(|| {
        syn::Error::new_spanned(attr, "Missing code(...) in diagnostic attribute")
    })?;

    Ok((error_code, help_text))
}

fn path_to_string(path: &syn::Path) -> String {
    path.segments
        .iter()
        .map(|seg| seg.ident.to_string())
        .collect::<Vec<_>>()
        .join("::")
}

fn parse_variant_fields(fields: &Fields) -> syn::Result<Vec<ErrorFieldInfo>> {
    let mut field_info = Vec::new();

    match fields {
        Fields::Named(named_fields) => {
            for field in &named_fields.named {
                let field_name = field.ident.as_ref().unwrap().clone();

                // Check for special attributes
                let mut extension_name = extract_extension_name(&field.attrs)?;

                // If extension_name is the special marker, replace with camelCase field name
                if extension_name == Some("__FIELD_NAME__".to_string()) {
                    extension_name = Some(to_camel_case(&field_name.to_string()));
                }

                field_info.push(ErrorFieldInfo {
                    name: field_name,
                    extension_name,
                });
            }
        }
        Fields::Unnamed(_) => {
            // For unnamed fields, we'll generate generic field names
            for (i, field) in fields.iter().enumerate() {
                let field_name = format_ident!("field_{}", i);

                let mut extension_name = extract_extension_name(&field.attrs)?;

                // If extension_name is the special marker, replace with camelCase field name
                if extension_name == Some("__FIELD_NAME__".to_string()) {
                    extension_name = Some(to_camel_case(&field_name.to_string()));
                }

                field_info.push(ErrorFieldInfo {
                    name: field_name,
                    extension_name,
                });
            }
        }
        Fields::Unit => {
            // No fields for unit variants
        }
    }

    Ok(field_info)
}

fn to_camel_case(s: &str) -> String {
    let mut result = String::new();
    let mut capitalize_next = false;

    for c in s.chars() {
        if c == '_' {
            capitalize_next = true;
        } else if capitalize_next {
            result.push(c.to_ascii_uppercase());
            capitalize_next = false;
        } else {
            result.push(c);
        }
    }

    result
}

fn extract_extension_name(attrs: &[Attribute]) -> syn::Result<Option<String>> {
    for attr in attrs {
        if attr.path().is_ident("extension") {
            match &attr.meta {
                syn::Meta::List(list) => {
                    // Parse #[extension("name")] - explicit name
                    let lit: syn::LitStr = syn::parse2(list.tokens.clone())?;
                    return Ok(Some(lit.value()));
                }
                syn::Meta::Path(_) => {
                    // Parse #[extension] - use field name in camelCase
                    // Return a special marker that we'll replace with the actual field name later
                    return Ok(Some("__FIELD_NAME__".to_string()));
                }
                _ => {
                    return Err(syn::Error::new_spanned(
                        attr,
                        "Expected #[extension] or #[extension(\"name\")] format",
                    ));
                }
            }
        }
    }
    Ok(None)
}

fn generate_error_code_match_arms(variants: &[ErrorVariantInfo]) -> Vec<proc_macro2::TokenStream> {
    variants
        .iter()
        .map(|variant| {
            let variant_name = &variant.variant_name;
            let error_code = &variant.error_code;

            if variant.fields.is_empty() {
                quote! {
                    Self::#variant_name => #error_code,
                }
            } else if variant.is_tuple_variant {
                quote! {
                    Self::#variant_name(..) => #error_code,
                }
            } else {
                quote! {
                    Self::#variant_name { .. } => #error_code,
                }
            }
        })
        .collect()
}

fn generate_graphql_extensions_arms(
    variants: &[ErrorVariantInfo],
) -> Vec<proc_macro2::TokenStream> {
    variants
        .iter()
        .map(generate_graphql_extensions_for_variant)
        .collect()
}

fn generate_graphql_extensions_for_variant(variant: &ErrorVariantInfo) -> proc_macro2::TokenStream {
    let variant_name = &variant.variant_name;
    let error_type = variant_name.to_string().to_screaming_snake_case();

    if variant.fields.is_empty() {
        return quote! {
            Self::#variant_name => {
                extensions_map.insert("errorType".to_string(), serde_json::Value::String(#error_type.to_string()));
            },
        };
    }

    // Only generate field bindings for fields that have extensions
    let fields_with_extensions: Vec<_> = variant
        .fields
        .iter()
        .filter(|field| field.extension_name.is_some())
        .collect();

    let field_insertions: Vec<_> = fields_with_extensions
        .iter()
        .filter_map(|field| generate_field_insertion(field))
        .collect();

    if fields_with_extensions.is_empty() {
        // No extension fields, use wildcard pattern
        if variant.is_tuple_variant {
            quote! {
                Self::#variant_name(..) => {
                    extensions_map.insert("errorType".to_string(), serde_json::Value::String(#error_type.to_string()));
                },
            }
        } else {
            quote! {
                Self::#variant_name { .. } => {
                    extensions_map.insert("errorType".to_string(), serde_json::Value::String(#error_type.to_string()));
                },
            }
        }
    } else {
        // Generate specific field bindings only for extension fields
        if variant.is_tuple_variant {
            // For tuple variants, we need to match all fields but only bind extension ones
            let field_patterns: Vec<_> = variant
                .fields
                .iter()
                .map(|field| {
                    if field.extension_name.is_some() {
                        let field_name = &field.name;
                        quote! { #field_name }
                    } else {
                        quote! { _ }
                    }
                })
                .collect();

            quote! {
                Self::#variant_name(#(#field_patterns),*) => {
                    extensions_map.insert("errorType".to_string(), serde_json::Value::String(#error_type.to_string()));
                    #(#field_insertions)*
                },
            }
        } else {
            // For named fields, only bind the extension fields
            let field_bindings: Vec<_> = fields_with_extensions
                .iter()
                .map(|field| {
                    let field_name = &field.name;
                    quote! { #field_name }
                })
                .collect();

            quote! {
                Self::#variant_name { #(#field_bindings),*, .. } => {
                    extensions_map.insert("errorType".to_string(), serde_json::Value::String(#error_type.to_string()));
                    #(#field_insertions)*
                },
            }
        }
    }
}

fn generate_field_insertion(field: &ErrorFieldInfo) -> Option<proc_macro2::TokenStream> {
    let field_name = &field.name;

    // Only include fields that have an explicit extension name
    let extension_field_name = field.extension_name.as_ref()?;

    // Generate appropriate insertion based on field type
    Some(quote! {
        extensions_map.insert(#extension_field_name.to_string(), serde_json::to_value(#field_name).unwrap_or(serde_json::Value::Null));
    })
}

fn generate_registry_entry(
    enum_name: &syn::Ident,
    variants: &[ErrorVariantInfo],
) -> syn::Result<proc_macro2::TokenStream> {
    let enum_name_str = enum_name.to_string();

    // Extract component and category from the first variant's error code
    let first_variant = variants.first().ok_or_else(|| {
        syn::Error::new_spanned(enum_name, "Error enum must have at least one variant")
    })?;

    let (component, category) = extract_component_and_category(&first_variant.error_code);
    let primary_error_code = &first_variant.error_code;

    // Generate variant info for registry
    let variant_entries: Vec<_> = variants
        .iter()
        .map(|variant| {
            let variant_name = variant.variant_name.to_string();
            let error_code = &variant.error_code;
            let help = variant.help_text.as_deref();

            let graphql_fields: Vec<_> = variant
                .fields
                .iter()
                .filter_map(|f| f.extension_name.as_ref())
                .cloned()
                .collect();

            let help_value = match help {
                Some(h) => quote! { Some(#h) },
                None => quote! { None },
            };

            quote! {
                apollo_router_error::ErrorVariantInfo {
                    name: #variant_name,
                    code: #error_code,
                    help: #help_value,
                    graphql_fields: &[#(#graphql_fields),*],
                }
            }
        })
        .collect();

    // Generate a unique identifier for the registry entry to avoid name collisions
    let registry_entry_name =
        format_ident!("__{}_REGISTRY_ENTRY", enum_name.to_string().to_uppercase());

    Ok(quote! {
        #[cfg(feature = "registry")]
        apollo_router_error::register_error! {
            registry_name: #registry_entry_name,
            type_name: #enum_name_str,
            error_code: #primary_error_code,
            category: #category,
            component: #component,
            variants: [#(#variant_entries),*]
        }
    })
}

fn extract_component_and_category(error_code: &str) -> (String, String) {
    let parts: Vec<&str> = error_code.split("::").collect();

    if parts.len() >= 3 {
        // Format: apollo_router::component::category::specific_error
        let component = parts[1].to_string();
        let category = parts[2].to_string();
        (component, category)
    } else {
        ("unknown".to_string(), "unknown".to_string())
    }
}

fn generate_graphql_handler_entry(enum_name: &syn::Ident) -> syn::Result<proc_macro2::TokenStream> {
    let enum_name_str = enum_name.to_string();

    // Generate a unique identifier for the handler function to avoid name collisions
    let handler_function_name = format_ident!(
        "__{}_graphql_error_handler",
        enum_name.to_string().to_uppercase()
    );

    // Generate a unique static name to avoid collisions
    let handler_static_name = format_ident!(
        "__{}_GRAPHQL_ERROR_HANDLER",
        enum_name.to_string().to_uppercase()
    );

    Ok(quote! {
        #[cfg(feature = "registry")]
        apollo_router_error::register_graphql_error_handler! {
            handler_name: #handler_function_name,
            static_name: #handler_static_name,
            type_name: #enum_name_str,
            error_type: #enum_name
        }
    })
}

#[cfg(test)]
mod tests {
    use syn::DeriveInput;
    use syn::parse_quote;

    use super::*;

    #[test]
    fn test_derive_macro_basic() {
        let input: DeriveInput = parse_quote! {
            #[derive(Debug, thiserror::Error, miette::Diagnostic, Error)]
            pub enum TestError {
                #[error("Config error: {message}")]
                #[diagnostic(code(apollo_router::test::config_error), help("Check config"))]
                ConfigError {
                    #[extension("errorMessage")]
                    message: String
                },

                #[error("Network error")]
                #[diagnostic(code(apollo_router::test::network_error))]
                NetworkError,
            }
        };

        let result = generate_error_impl(input);
        assert!(
            result.is_ok(),
            "Derive macro should succeed: {:?}",
            result.err()
        );

        let generated = result.unwrap();
        let generated_str = generated.to_string();

        // Check that Error trait is implemented
        assert!(generated_str.contains("impl apollo_router_error :: Error for TestError"));
        assert!(generated_str.contains("fn error_code"));
        assert!(generated_str.contains("fn populate_graphql_extensions"));

        // Check error codes are generated correctly
        assert!(generated_str.contains("apollo_router::test::config_error"));
        assert!(generated_str.contains("apollo_router::test::network_error"));

        // Check that extension field is included
        assert!(generated_str.contains("errorMessage"));
    }

    #[test]
    fn test_error_code_extraction() {
        let attrs = vec![parse_quote! {
            #[diagnostic(code(apollo_router::service::my_error), help("Help text"))]
        }];

        let result = extract_diagnostic_info(&attrs);
        assert!(result.is_ok());

        let (code, help) = result.unwrap();
        assert_eq!(code, "apollo_router::service::my_error");
        assert_eq!(help, Some("Help text".to_string()));
    }

    #[test]
    fn test_component_category_extraction() {
        let (component, category) =
            extract_component_and_category("apollo_router::query_parse::syntax_error");
        assert_eq!(component, "query_parse");
        assert_eq!(category, "syntax_error");

        let (component, category) =
            extract_component_and_category("apollo_router::layers::conversion");
        assert_eq!(component, "layers");
        assert_eq!(category, "conversion");
    }

    // Removed test_graphql_error_type_inference as the function no longer exists

    #[test]
    fn test_field_parsing() {
        let fields: syn::FieldsNamed = parse_quote! {
            {
                #[extension("errorMessage")]
                message: String,
                #[source]
                source_error: std::io::Error,
                #[source_code]
                source_text: Option<String>,
                regular_field: String,
            }
        };

        let result = parse_variant_fields(&Fields::Named(fields));
        assert!(result.is_ok());

        let field_info = result.unwrap();
        assert_eq!(field_info.len(), 4);

        // Regular field with extension
        assert_eq!(field_info[0].name.to_string(), "message");
        assert_eq!(
            field_info[0].extension_name,
            Some("errorMessage".to_string())
        );

        // Source field (no extension)
        assert_eq!(field_info[1].name.to_string(), "source_error");
        assert_eq!(field_info[1].extension_name, None);

        // Source code field (no extension)
        assert_eq!(field_info[2].name.to_string(), "source_text");
        assert_eq!(field_info[2].extension_name, None);

        // Regular field (no extension)
        assert_eq!(field_info[3].name.to_string(), "regular_field");
        assert_eq!(field_info[3].extension_name, None);
    }

    #[test]
    fn test_missing_diagnostic_attribute() {
        let attrs = vec![parse_quote! {
            #[error("Some error")]
        }];

        let result = extract_diagnostic_info(&attrs);
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("Missing #[diagnostic(code(...))] attribute")
        );
    }

    #[test]
    fn test_camel_case_conversion() {
        assert_eq!(to_camel_case("message"), "message");
        assert_eq!(to_camel_case("error_code"), "errorCode");
        assert_eq!(to_camel_case("my_field_name"), "myFieldName");
        assert_eq!(to_camel_case("single"), "single");
        assert_eq!(to_camel_case("a_b_c"), "aBC");
    }

    #[test]
    fn test_extension_attribute_parsing() {
        // Test explicit name
        let attrs = vec![parse_quote! {
            #[extension("customFieldName")]
        }];

        let result = extract_extension_name(&attrs);
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), Some("customFieldName".to_string()));

        // Test marker for field name
        let attrs = vec![parse_quote! {
            #[extension]
        }];

        let result = extract_extension_name(&attrs);
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), Some("__FIELD_NAME__".to_string()));
    }

    #[test]
    fn test_extension_field_generation() {
        let input: DeriveInput = parse_quote! {
            #[derive(Debug, thiserror::Error, miette::Diagnostic, Error)]
            pub enum TestError {
                #[error("Error with extensions: {message}")]
                #[diagnostic(code(apollo_router::test::extension_error))]
                WithExtensions {
                    #[extension("userMessage")]
                    message: String,
                    #[extension("errorCode")]
                    code: i32,
                    #[source]
                    cause: Option<Box<dyn std::error::Error + Send + Sync>>,
                    non_extension_field: String,
                },
            }
        };

        let result = generate_error_impl(input);
        assert!(
            result.is_ok(),
            "Derive macro should succeed: {:?}",
            result.err()
        );

        let generated = result.unwrap();
        let generated_str = generated.to_string();

        // Check that extension fields are included
        assert!(generated_str.contains("userMessage"));
        assert!(generated_str.contains("errorCode"));

        // Check that non-extension field is not included
        assert!(!generated_str.contains("non_extension_field"));

        // Check that source field is not included
        assert!(!generated_str.contains("cause"));
    }

    #[test]
    fn test_invalid_enum_input() {
        let input: DeriveInput = parse_quote! {
            pub struct NotAnEnum {
                field: String,
            }
        };

        let result = generate_error_impl(input);
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("Error can only be derived for enums")
        );
    }
}
