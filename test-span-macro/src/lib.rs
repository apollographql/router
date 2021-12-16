use proc_macro::TokenStream;
use proc_macro2::TokenStream as TokenStream2;
use quote::quote;

use syn::parse_macro_input;
use syn::AttributeArgs;
use syn::ItemFn;
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

    let subscriber_boilerplate = subscriber_boilerplate();

    quote! {
      #[#macro_attrs]
      #(#fn_attrs)*
      #maybe_async fn #test_name() #ret {
        #maybe_async fn inner_test(get_logs: impl Fn() -> Records, get_span: impl Fn() -> Span) #ret
          #body


        #subscriber_boilerplate

        #run_test #maybe_semicolon
      }
    }
    .into()
}

fn async_test() -> TokenStream2 {
    quote! {
        inner_test(get_logs, get_span)
            .with_subscriber(subscriber).await
    }
}

fn sync_test() -> TokenStream2 {
    quote! {
        subscriber.init();

        inner_test(get_logs, get_span)
    }
}
fn subscriber_boilerplate() -> TokenStream2 {
    quote! {
        let id_sequence = Default::default();
        let all_spans = Default::default();
        let logs = Default::default();

        let subscriber = tracing_subscriber::registry().with(Layer::new(
            Arc::clone(&id_sequence),
            Arc::clone(&all_spans),
            Arc::clone(&logs),
        ));

        let logs_clone = Arc::clone(&logs);
        let spans_clone = Arc::clone(&all_spans);
        let id_sequence_clone = Arc::clone(&id_sequence);

        #[allow(unused)]
        let get_logs = move || logs_clone.lock().unwrap().contents();

        #[allow(unused)]
        let get_span = move || {
            let all_spans = spans_clone.lock().unwrap().clone();
            let id_sequence = id_sequence_clone.read().unwrap().clone();
            Span::from_records(id_sequence, all_spans)
        };
    }
}
