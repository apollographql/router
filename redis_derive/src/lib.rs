use proc_macro::TokenStream;
use quote::quote;

#[proc_macro_derive(SerializableValue)]
pub fn derive_redis_value_type_for_serializable_type(input: TokenStream) -> TokenStream {
    // Construct a representation of Rust code as a syntax tree
    // that we can manipulate.
    let ast = syn::parse(input).expect("Unable to parse syntax for SerializableValue derivation");

    // Build the trait implementation.
    impl_redis_value_type_for_serializable_type_macro(&ast)
}

fn impl_redis_value_type_for_serializable_type_macro(ast: &syn::DeriveInput) -> TokenStream {
    let name = &ast.ident;
    let generated = quote! {
        impl crate::redis::ValueType for #name {
            fn try_from_redis_value(value: crate::redis::FredValue) -> crate::redis::FredResult<Self> {
                crate::redis::try_from_redis_value(value)
            }

            fn try_into_redis_value(self) -> crate::redis::FredResult<crate::redis::FredValue> {
                crate::redis::try_into_redis_value(self)
            }
        }
    };
    generated.into()
}
