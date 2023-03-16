### Uplink metrics ([Issue #2769](https://github.com/apollographql/router/issues/2769))

Adds metrics for uplink of the format:
```
# HELP uplink uplink
# TYPE uplink histogram
uplink_bucket{kind="duration",query="SupergraphSdl",service_name="apollo-router",type="unchanged",url="https://uplink.api.apollographql.com/graphql",le="0.001"} 0
uplink_bucket{kind="duration",query="SupergraphSdl",service_name="apollo-router",type="unchanged",url="https://uplink.api.apollographql.com/graphql",le="0.005"} 0
uplink_bucket{kind="duration",query="SupergraphSdl",service_name="apollo-router",type="unchanged",url="https://uplink.api.apollographql.com/graphql",le="0.015"} 0
uplink_bucket{kind="duration",query="SupergraphSdl",service_name="apollo-router",type="unchanged",url="https://uplink.api.apollographql.com/graphql",le="0.05"} 0
uplink_bucket{kind="duration",query="SupergraphSdl",service_name="apollo-router",type="unchanged",url="https://uplink.api.apollographql.com/graphql",le="0.1"} 0
uplink_bucket{kind="duration",query="SupergraphSdl",service_name="apollo-router",type="unchanged",url="https://uplink.api.apollographql.com/graphql",le="0.2"} 0
uplink_bucket{kind="duration",query="SupergraphSdl",service_name="apollo-router",type="unchanged",url="https://uplink.api.apollographql.com/graphql",le="0.3"} 1
uplink_bucket{kind="duration",query="SupergraphSdl",service_name="apollo-router",type="unchanged",url="https://uplink.api.apollographql.com/graphql",le="0.4"} 1
uplink_bucket{kind="duration",query="SupergraphSdl",service_name="apollo-router",type="unchanged",url="https://uplink.api.apollographql.com/graphql",le="0.5"} 1
uplink_bucket{kind="duration",query="SupergraphSdl",service_name="apollo-router",type="unchanged",url="https://uplink.api.apollographql.com/graphql",le="1"} 1
uplink_bucket{kind="duration",query="SupergraphSdl",service_name="apollo-router",type="unchanged",url="https://uplink.api.apollographql.com/graphql",le="5"} 1
uplink_bucket{kind="duration",query="SupergraphSdl",service_name="apollo-router",type="unchanged",url="https://uplink.api.apollographql.com/graphql",le="10"} 1
uplink_bucket{kind="duration",query="SupergraphSdl",service_name="apollo-router",type="unchanged",url="https://uplink.api.apollographql.com/graphql",le="+Inf"} 1
uplink_sum{kind="duration",query="SupergraphSdl",service_name="apollo-router",type="unchanged",url="https://uplink.api.apollographql.com/graphql"} 0.21680258
uplink_count{kind="duration",query="SupergraphSdl",service_name="apollo-router",type="unchanged",url="https://uplink.api.apollographql.com/graphql"} 1
```
This is a histogram of duration which contains the following attributes:
* url: the url that was polled
* query: SupergraphSdl|Entitlement
* type: new|unchanged|http_error|uplink_error
* code: The error code depending on type
* error: The error message

A limitation of this is that it can't display metrics for the first poll to uplink as telemetry hasn't been set up yet.

By [@BrynCooke](https://github.com/BrynCooke) in https://github.com/apollographql/router/pull/2779
