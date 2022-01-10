use proc_macro::TokenStream;
use proc_macro2::TokenStream as TokenStream2;
use quote::quote;

use syn::parse_macro_input;
use syn::AttributeArgs;
use syn::ItemFn;
use syn::Path;
use syn::ReturnType;

#[proc_macro_attribute]
pub fn test_span(attr: TokenStream, item: TokenStream) -> TokenStream {
    let attrs = parse_macro_input!(attr as AttributeArgs);
    let test_fn = parse_macro_input!(item as ItemFn);

    let macro_attrs = if attrs.as_slice().is_empty() {
        quote! { test }
    } else {
        quote! {#(#attrs)*}
    };

    let fn_attrs = &test_fn.attrs;

    let mut tracing_level = quote!(::tracing::Level::DEBUG);

    // Get tracing level from #[level(tracing::Level::INFO)]
    let fn_attrs = fn_attrs
        .iter()
        .filter(|attr| {
            let path = &attr.path;
            if quote!(#path).to_string().as_str() == "level" {
                let value: Path = attr.parse_args().expect(
                    "wrong level attribute synthax. Example: #[level(tracing::Level::INFO)]",
                );
                tracing_level = quote!(#value);
                false
            } else {
                true
            }
        })
        .collect::<Vec<_>>();

    let maybe_async = &test_fn.sig.asyncness;

    let body = &test_fn.block;
    let test_name = &test_fn.sig.ident;
    let output_type = &test_fn.sig.output;

    let maybe_semicolon = if let ReturnType::Default = output_type {
        quote! {;}
    } else {
        quote! {}
    };

    let run_test = if maybe_async.is_some() {
        async_test()
    } else {
        sync_test()
    };

    let ret = quote! {#output_type};

    let subscriber_boilerplate = subscriber_boilerplate(tracing_level);

    quote! {
      #[#macro_attrs]
      #(#fn_attrs)*
      #maybe_async fn #test_name() #ret {
        #maybe_async fn inner_test(get_telemetry: impl Fn() -> (Span, Records), get_logs: impl Fn() -> Records, get_spans: impl Fn() -> Span) #ret
          #body


        #subscriber_boilerplate

        #run_test #maybe_semicolon
      }
    }
    .into()
}

fn async_test() -> TokenStream2 {
    quote! {
        inner_test(get_telemetry, get_logs, get_spans)
            .instrument(root_span).await
    }
}

fn sync_test() -> TokenStream2 {
    quote! {
        root_span.in_scope(|| {
            inner_test(get_telemetry, get_logs, get_spans)
        });
    }
}
fn subscriber_boilerplate(level: TokenStream2) -> TokenStream2 {
    quote! {
        test_span::init();

        let level = &#level;

        let root_span = test_span::reexports::tracing::span!(#level, "root");

        let root_id = root_span.id().clone().expect("couldn't get root span id; this cannot happen.");

        #[allow(unused)]
        let get_telemetry = || test_span::get_telemetry_for_root(&root_id, level);

        #[allow(unused)]
        let get_logs = || test_span::get_logs_for_root(&root_id, level);


        #[allow(unused)]
        let get_spans = || test_span::get_spans_for_root(&root_id, level);
    }
}
