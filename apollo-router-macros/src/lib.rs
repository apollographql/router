extern crate proc_macro;

use convert_case::Case;
use convert_case::Casing;
use proc_macro::TokenStream;
use quote::{quote, ToTokens};
use syn::__private::TokenStream2;
use syn::{ItemImpl, Type};
macro_rules! matches_enum {
    ($t:ident::$v:ident($p:ident)) => {
        |f| match (&f) {
            $t::$v($p) => Some($p),
            _ => None,
        }
    };

    ($e:expr, $t:ident::$v:ident($p:ident)) => {
        match (&$e) {
            $t::$v($p) => Some($p),
            _ => None,
        }
    };
}

#[proc_macro_attribute]
pub fn router_plugin(_attr: TokenStream, item: TokenStream) -> TokenStream {
    let ast: ItemImpl = syn::parse(item).unwrap();
    let impl_stream = create_plugin(&ast).map(TokenStream::from);

    TokenStream::from_iter(
        vec![Some(ast.into_token_stream().into()), impl_stream]
            .into_iter()
            .flatten(),
    )
}

fn create_plugin(ast: &ItemImpl) -> Option<TokenStream2> {
    let type_name = matches_enum!(*ast.self_ty, Type::Path(a))?
        .path
        .get_ident()?;
    let type_name_snake = type_name.to_string().to_case(Case::Snake);

    let expanded = quote! {
            startup::on_startup! {
                // Register the plugin factory function
                apollo_router_core::plugins_mut().insert(#type_name_snake.to_string(), || Box::new(#type_name::default()));
            }
            impl apollo_router_core::DynPlugin for #type_name where
                Self: Plugin + 'static,
            {

                fn configure(&mut self, configuration: &serde_json::Value) -> Result<(), tower::BoxError> {
                    let conf = serde_json::from_value(configuration.clone())?;
                    apollo_router_core::Plugin::configure(self, conf)
                }

                fn startup<'a>(&'a mut self) -> futures::future::BoxFuture<'a, Result<(), tower::BoxError>> {
                    Box::pin(apollo_router_core::Plugin::startup(self))
                }

                fn shutdown<'a>(&'a mut self) -> futures::future::BoxFuture<'a, Result<(), tower::BoxError>> {
                    Box::pin(apollo_router_core::Plugin::shutdown(self))
                }

                fn router_service(
                    &mut self,
                    service: tower::util::BoxService<apollo_router_core::RouterRequest, apollo_router_core::RouterResponse, tower::BoxError>,
                ) -> tower::util::BoxService<apollo_router_core::RouterRequest, apollo_router_core::RouterResponse, tower::BoxError> {
                    apollo_router_core::Plugin::router_service(self, service)
                }

                fn query_planning_service(
                    &mut self,
                    service: tower::util::BoxService<apollo_router_core::RouterRequest, apollo_router_core::PlannedRequest, tower::BoxError>,
                ) -> tower::util::BoxService<apollo_router_core::RouterRequest, apollo_router_core::PlannedRequest, tower::BoxError> {
                    Plugin::query_planning_service(self, service)
                }

                fn execution_service(
                    &mut self,
                    service: tower::util::BoxService<apollo_router_core::PlannedRequest, apollo_router_core::RouterResponse, tower::BoxError>,
                ) -> tower::util::BoxService<apollo_router_core::PlannedRequest, apollo_router_core::RouterResponse, tower::BoxError> {
                    Plugin::execution_service(self, service)
                }

                fn subgraph_service(
                    &mut self,
                    name: &str,
                    service: tower::util::BoxService<apollo_router_core::SubgraphRequest, apollo_router_core::RouterResponse, tower::BoxError>,
                ) -> tower::util::BoxService<apollo_router_core::SubgraphRequest, apollo_router_core::RouterResponse, tower::BoxError> {
                    apollo_router_core::Plugin::subgraph_service(self, name, service)
                }
            }
    };

    // Hand the output tokens back to the compiler
    Some(expanded)
}

#[cfg(test)]
mod test {
    use std::str::FromStr;

    use syn::ItemImpl;

    use crate::create_plugin;

    #[test]
    fn check_success() {
        // struct sample
        let s = "impl Plugin for MyPlugin {
    type Config = Conf;

    fn configure(&mut self, configuration: Self::Config) -> Result<(), BoxError> {}
}
";

        let tokens = proc_macro2::TokenStream::from_str(s).unwrap();
        let ast: ItemImpl = syn::parse2(tokens).unwrap();

        assert!(create_plugin(&ast).is_some());
    }
}
