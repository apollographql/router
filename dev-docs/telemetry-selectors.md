# Telemetry selectors

The router has many [selectors](https://www.apollographql.com/docs/router/configuration/telemetry/instrumentation/selectors/) which are really helpful for users wishing to customize their telemetry. With selectors they're able to add custom attributes on [spans](https://www.apollographql.com/docs/router/configuration/telemetry/instrumentation/spans), or create custom [instruments](https://www.apollographql.com/docs/router/configuration/telemetry/instrumentation/instruments) which have custom metrics but also conditionally [log events](https://www.apollographql.com/docs/router/configuration/telemetry/instrumentation/events/).

Some useful existing selectors are `request_header` to fetch the value of a specific request header, `trace_id` to get the current trace id, `supergraph_query` to get the current query executed at the supergraph level.

## Goal

Selectors may be used as a value for an attribute or a metric value and also as a condition. What we would like to achieve in the future is to provide enough flexibility to users so that we can reduce the number of metrics provided by default in the router when adding new features. For example, let's talk about authentication features. We could provide a selector called `authenticated` which would return a boolean specifying that the user executing the query is authenticated (or not). This selector would provide the ability for our users to create their own metrics which specified, for example, how many authenticated metrics are executed. If you combine this with a selector giving the operation cost we could create an histogram indicating the distribution of operation cost for authenticated queries.

## How to add your own selector ?

Everything you need to know happens in [this file](https://github.com/apollographql/router/blob/dev/apollo-router/src/plugins/telemetry/config_new/selectors.rs). A selector implements [`Selector` trait](https://github.com/apollographql/router/blob/db741ad683508bb05b1da687e53d6ab00962bb18/apollo-router/src/plugins/telemetry/config_new/mod.rs#L35) which has 2 associated types, one for the `Request` and one for the `Response` type. A `Selector` can happen at different service levels, like `router`|`supergraph`|`subgraph` and so it will let you write the right logic into the methods provided by this trait. You have method `fn on_request(&self, request: &Self::Request) -> Option<opentelemetry::Value>` to fetch a value from the request itself and `fn on_response(&self, response: &Self::Response) -> Option<opentelemetry::Value>` to fetch a value from the response. For example if we look at the `request_header` selector it will happen [at the request level](https://github.com/apollographql/router/blob/db741ad683508bb05b1da687e53d6ab00962bb18/apollo-router/src/plugins/telemetry/config_new/selectors.rs#L482).

You won't have to create new types implementing this `Selector` trait as we already have 3 different kind of `Selector`, [`RouterSelector`](https://github.com/apollographql/router/blob/db741ad683508bb05b1da687e53d6ab00962bb18/apollo-router/src/plugins/telemetry/config_new/selectors.rs#L76), [`SupergraphSelector`](https://github.com/apollographql/router/blob/db741ad683508bb05b1da687e53d6ab00962bb18/apollo-router/src/plugins/telemetry/config_new/selectors.rs#L161) and [`SubgraphSelector`](https://github.com/apollographql/router/blob/db741ad683508bb05b1da687e53d6ab00962bb18/apollo-router/src/plugins/telemetry/config_new/selectors.rs#L276). Both of these types are enum and include different available selectors for each services.

If you want to define your own selector you just have to add a new variant to these enums and handle the logic properly in the implementation of the `Selector` trait on each enum. Example [here](https://github.com/apollographql/router/blob/db741ad683508bb05b1da687e53d6ab00962bb18/apollo-router/src/plugins/telemetry/config_new/selectors.rs#L473) for `RouterSelector`. If you wanted to add a new selector for authentication and call it `authenticated` you would have to add something like this in the enum:

```rust
pub(crate) enum SupergraphSelector {
    //....
    Authenticated {
        /// If the operation is authenticated, set to true to enable it
        authenticated: bool,
    }
    //....
}
```

The implementation would look like this:

```rust
impl Selector for SupergraphSelector {
    type Request = supergraph::Request;
    type Response = supergraph::Response;
    
    fn on_request(&self, request: &supergraph::Request) -> Option<opentelemetry::Value> {
        match self {
            // ...
            SupergraphSelector::Authenticated {
                authenticated
            } if *authenticated => {
                let is_authenticated = request.context.get::<bool>(APOLLO_AUTHENTICATED_USER).ok().flatten();
                match is_authenticated {
                    Some(is_authenticated) => Some(opentelemetry::Value::Bool(is_authenticated)),
                    None => None,
                }
            }
            // ...
            // For response
            _ => None,
        }
    }
}
```
    
You can test it properly like this:

```rust
#[test]
fn supergraph_authenticated() {
    let selector = SupergraphSelector::Authenticated {
        authenticated: true,
    };
    let context = crate::context::Context::new();
    let _ = context.insert(APOLLO_AUTHENTICATED_USER, true);
    assert_eq!(
        selector
            .on_request(
                &crate::services::SupergraphRequest::fake_builder()
                    .context(context.clone())
                    .build()
                    .unwrap()
            )
            .unwrap(),
        true.into()
    );

    assert_eq!(
        selector
            .on_request(
                &crate::services::SupergraphRequest::fake_builder()
                    .build()
                    .unwrap()
            ),
        None
    );
}
```

Finally, as an end user you would be able to create your own custom instrument like this:

```yaml title="router.yaml"
telemetry:
  instrumentation:
    instruments:
      supergraph:
        # Custom metric 
        authenticated_operation: 
          value: unit
          type: counter
          unit: operation
          description: "Number of authenticated operations"
          attributes:
            http.response.status_code: true
            "my_attribute":
              request_header: "x-my-header"
            graphql.authenticated:
                authenticated: true # Can also be used as a value for an attribute
          condition:
            eq:
              - true
              - authenticated: true
```