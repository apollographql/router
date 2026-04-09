### Add per-subgraph and per-connector HTTP response size limits ([PR #9160](https://github.com/apollographql/router/pull/9160))

The router can now cap the number of bytes it reads from subgraph and connector HTTP response bodies, protecting against out-of-memory conditions when a downstream service returns an unexpectedly large payload.

The limit is enforced as the response body streams in — the router stops reading and returns a GraphQL error as soon as the limit is exceeded, without buffering the full body first.

Configure a global default and optional per-subgraph or per-source overrides:

```yaml
limits:
  subgraph:
    all:
      http_max_response_bytes: 10485760 # 10 MB for all subgraphs
    subgraphs:
      products:
        http_max_response_bytes: 20971520 # 20 MB override for 'products'

  connector:
    all:
      http_max_response_bytes: 5242880 # 5 MB for all connector sources
    sources:
      products.rest:
        http_max_response_bytes: 10485760 # 10 MB override for 'products.rest'
```

There is no default limit; responses are unrestricted unless you configure this option.

When a response is aborted due to the limit, the router:
- Returns a GraphQL error to the client with extension code `SUBREQUEST_HTTP_ERROR`
- Increments the `apollo.router.limits.subgraph_response_size.exceeded` or `apollo.router.limits.connector_response_size.exceeded` counter
- Records `apollo.subgraph.response.aborted: "response_size_limit"` or `apollo.connector.response.aborted: "response_size_limit"` on the relevant span

**Configuration migration**: Existing `limits` fields (previously at the top level of `limits`) are now nested under `limits.router`. A configuration migration is included that updates your config file automatically.

By [@carodewig](https://github.com/carodewig) in https://github.com/apollographql/router/pull/9160
