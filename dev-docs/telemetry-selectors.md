# Telemetry selectors

The router has now plenty of [selectors](https://www.apollographql.com/docs/router/configuration/telemetry/instrumentation/selectors/) which are really helpful for our users to customize the telemetry as they want. For example with selectors they're able to add custom attributes on [spans](https://www.apollographql.com/docs/router/configuration/telemetry/instrumentation/spans), or create custom [instruments](https://www.apollographql.com/docs/router/configuration/telemetry/instrumentation/instruments) to have custom metrics but also conditionnaly [log events](https://www.apollographql.com/docs/router/configuration/telemetry/instrumentation/events/). For example, existing selectors are `request_header` to fetch the value of a specific request header, `trace_id` to get the current trace id, `supergraph_query` to get the current query executed at the supergraph level.

## Goal

Both selectors can be used as a value for an attribute or a metric value but also as a condition. What we would like to achieve in the future is to provide a great flexibility to our users to not have to create our own metrics when adding new features. For example let's talk about authentication features, we could provide a selector called `authenticated` which would return a boolean to know if the user executing the query is authenticated or not, thanks to this selector we would end up for our users to create their own metrics to know for example how many authenticated metrics are executed. If you combine this with a selector giving the operation cost we could create an histogram indicating what's the distribution of operation cost for authenticated queries.

## How to add your own selector ?

Everything you need to know happens in [this file](https://github.com/apollographql/router/blob/dev/apollo-router/src/plugins/telemetry/config_new/selectors.rs), a selector implements [`Selector` trait](https://github.com/apollographql/router/blob/db741ad683508bb05b1da687e53d6ab00962bb18/apollo-router/src/plugins/telemetry/config_new/mod.rs#L35) which has 2 associated types, one for the `Request` and one for the `Response` type. It's mainly useful because a `Selector` can happen at different service level, like `router`|`supergraph`|`subgraph` and so it will let you write the right logic into the methods provided by this trait. You have method `fn on_request(&self, request: &Self::Request) -> Option<opentelemetry::Value>` to fetch a value from the request itself and `fn on_response(&self, response: &Self::Response) -> Option<opentelemetry::Value>` to fetch a value from the response. For example if we talk about the `request_header` selector it will happen [at the request level](https://github.com/apollographql/router/blob/db741ad683508bb05b1da687e53d6ab00962bb18/apollo-router/src/plugins/telemetry/config_new/selectors.rs#L482).

You won't have to create new types implementing this `Selector` trait as we already have 3 different kind of `Selector`, [`RouterSelector`](https://github.com/apollographql/router/blob/db741ad683508bb05b1da687e53d6ab00962bb18/apollo-router/src/plugins/telemetry/config_new/selectors.rs#L76), [`SupergraphSelector`](https://github.com/apollographql/router/blob/db741ad683508bb05b1da687e53d6ab00962bb18/apollo-router/src/plugins/telemetry/config_new/selectors.rs#L161) and [`SubgraphSelector`](https://github.com/apollographql/router/blob/db741ad683508bb05b1da687e53d6ab00962bb18/apollo-router/src/plugins/telemetry/config_new/selectors.rs#L276). Both of these types are enum and include different available selectors for each services.

If you want to add your own selector you just have to add a new variant in these enums. And handle the logic properly in the implementation of the `Selector` trait on this enum. Example [here](https://github.com/apollographql/router/blob/db741ad683508bb05b1da687e53d6ab00962bb18/apollo-router/src/plugins/telemetry/config_new/selectors.rs#L473) for `RouterSelector`. If you wanted to add a new selector for authentication and call it `authenticated` you would have to add something like this in the enum:

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

And the implementation would look like this:

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
    
And you can test it properly like this:

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

And finally as an end user you would be able to create your own custom instrument like this for example:

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