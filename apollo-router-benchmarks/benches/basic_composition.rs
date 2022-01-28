use apollo_router::ApolloRouter;
use apollo_router_core::{
    FetchError, Fetcher, PreparedQuery, Request, Response, Router, ServiceRegistry, Value,
};
use async_trait::async_trait;
use criterion::{criterion_group, criterion_main, Criterion};
use once_cell::sync::OnceCell;
use serde_json_bytes::ByteString;
use std::sync::Arc;

macro_rules! generate_registry {
    ($name:ident => $( $service_name:ident : $service_struct:ident , )+) => {
        #[derive(Debug)]
        struct $name {
            $(
            $service_name: $service_struct,
            )+
        }

        impl $name {
            fn new() -> Self {
                Self {
                    $(
                    $service_name: $service_struct::new(),
                    )+
                }
            }
        }

        impl ServiceRegistry for $name {
            fn get(&self, service: &str) -> Option<&dyn Fetcher> {
                match service {
                    $(
                    stringify!($service_name) => Some(&self.$service_name),
                    )+
                    _ => todo!("service not implemented: {}", service)
                }
            }

            fn has(&self, service: &str) -> bool {
                match service {
                    $(
                    stringify!($service_name) => true,
                    )+
                    _ => false,
                }
            }
        }
    };
}

generate_registry!(MockRegistry =>
    accounts: Accounts,
    reviews: Reviews,
    products: Products,
);

macro_rules! generate_service {
    ($name:ident => $( $id:ident => $query:literal : $res:literal , )+) => {
        #[derive(Debug)]
        struct $name;

        $(
        static $id: OnceCell<Response> = OnceCell::new();
        )+

        impl $name {
            fn new() -> Self {
                $(
                $id.set(serde_json::from_str::<Response>($res).unwrap())
                    .expect("cannot initialize twice");
                )+

                Self
            }
        }

        #[async_trait]
        impl Fetcher for $name {
            async fn stream(&self, request: Request) -> Result<Response, FetchError> {
                let res = match request.query.as_str() {
                    $(
                    $query => Ok($id.get().unwrap().clone()),
                    )+
                    other => todo!(
                        "query for service {:?} not implemented:\n{}\nvariables:\n{}\n",
                        self,
                        other,
                        serde_json::to_string(&request.variables).unwrap(),
                    ),
                };
                res
            }
        }
    };
}

generate_service!(Accounts =>
    ACCOUNTS_1 => "query($representations:[_Any!]!){_entities(representations:$representations){...on User{name}}}":
        r#"{"data":{"_entities":[{"name":"Ada Lovelace"},{"name":"Alan Turing"},{"name":"Ada Lovelace"},{"name":"Alan Turing"}]}}"#,
);

generate_service!(Reviews =>
    REVIEWS_1 => "query($representations:[_Any!]!){_entities(representations:$representations){...on Product{reviews{id product{__typename upc}author{id __typename}}}}}":
        r#"{"data":{"_entities":[{"reviews":[{"id":"1","product":{"__typename":"Product","upc":"1"},"author":{"id":"1","__typename":"User"}},{"id":"4","product":{"__typename":"Product","upc":"1"},"author":{"id":"2","__typename":"User"}}]},{"reviews":[{"id":"2","product":{"__typename":"Product","upc":"2"},"author":{"id":"1","__typename":"User"}}]},{"reviews":[{"id":"3","product":{"__typename":"Product","upc":"3"},"author":{"id":"2","__typename":"User"}}]}]}}"#,
    REVIEWS_2 => "query($representations:[_Any!]!){_entities(representations:$representations){...on Product{reviews{id product{__typename upc}author{__typename id}}}}}":
    r#"{"data":{"_entities":[{"reviews":[{"id":"1","product":{"__typename":"Product","upc":"1"},"author":{"id":"1","__typename":"User"}},{"id":"4","product":{"__typename":"Product","upc":"1"},"author":{"id":"2","__typename":"User"}}]},{"reviews":[{"id":"2","product":{"__typename":"Product","upc":"2"},"author":{"id":"1","__typename":"User"}}]},{"reviews":[{"id":"3","product":{"__typename":"Product","upc":"3"},"author":{"id":"2","__typename":"User"}}]}]}}"#,
);

generate_service!(Products =>
PRODUCTS_1 => "query($representations:[_Any!]!){_entities(representations:$representations){...on Product{name}}}":
    r#"{"data":{"_entities":[{"name":"Table"},{"name":"Table"},{"name":"Couch"},{"name":"Chair"}]}}"#,
PRODUCTS_2 => "{topProducts{upc name __typename}}":
    r#"{"data":{"topProducts":[{"upc":"1","name":"Table","__typename":"Product"},{"upc":"2","name":"Couch","__typename":"Product"},{"upc":"3","name":"Chair","__typename":"Product"}]}}"#,
PRODUCTS_3 => "{topProducts{__typename upc name}}":
    r#"{"data":{"topProducts":[{"name":"Table","__typename":"Product", "upc":"1"},{"name":"Couch","__typename":"Product", "upc":"2"},{"name":"Chair","__typename":"Product", "upc":"3"}]}}"#,
);

async fn basic_composition_benchmark(federated: &ApolloRouter) {
    let query = r#"{ topProducts { upc name reviews {id product { name } author { id name } } } }"#;
    let request = Request::builder()
        .query(query)
        .variables(Arc::new(
            vec![
                (
                    ByteString::from("topProductsFirst"),
                    Value::Number(2usize.into()),
                ),
                (
                    ByteString::from("reviewsForAuthorAuthorId"),
                    Value::Number(1usize.into()),
                ),
            ]
            .into_iter()
            .collect(),
        ))
        .build();
    let _result = match federated.prepare_query(&request).await {
        Ok(prepared_query) => prepared_query.execute(request).await,
        Err(_) => panic!("should have prepared a query"),
    };
    // expected: Response { label: None, data: Object({"topProducts": Array([Object({"upc": String("1"), "name": String("Table"), "__typename": String("Product"), "reviews": Array([Object({"id": String("1"), "product": Object({"__typename": String("Product"), "upc": String("1"), "name": String("Table")}), "author": Object({"id": String("1"), "__typename": String("User"), "name": String("Ada Lovelace")})}), Object({"id": String("4"), "product": Object({"__typename": String("Product"), "upc": String("1"), "name": String("Table")}), "author": Object({"id": String("2"), "__typename": String("User"), "name": String("Alan Turing")})})])}), Object({"upc": String("2"), "name": String("Couch"), "__typename": String("Product"), "reviews": Array([Object({"id": String("2"), "product": Object({"__typename": String("Product"), "upc": String("2"), "name": String("Couch")}), "author": Object({"id": String("1"), "__typename": String("User"), "name": String("Ada Lovelace")})})])}), Object({"upc": String("3"), "name": String("Chair"), "__typename": String("Product"), "reviews": Array([Object({"id": String("3"), "product": Object({"__typename": String("Product"), "upc": String("3"), "name": String("Chair")}), "author": Object({"id": String("2"), "__typename": String("User"), "name": String("Alan Turing")})})])})])}), path: None, has_next: None, errors: [], extensions: {} }
}

fn from_elem(c: &mut Criterion) {
    let registry = Arc::new(MockRegistry::new());

    c.bench_function("basic_composition_benchmark", move |b| {
        let runtime = tokio::runtime::Runtime::new().unwrap();

        let schema = Arc::new(include_str!("fixtures/supergraph.graphql").parse().unwrap());

        let federated = ApolloRouter::new(registry.clone(), schema, None);

        let federated = runtime.block_on(federated);

        b.to_async(runtime)
            .iter(|| basic_composition_benchmark(&federated));
    });
}

criterion_group!(benches, from_elem);
criterion_main!(benches);
