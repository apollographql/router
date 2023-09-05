### Experimental support for Server-Sent Events (SSE) subscriptions

Add support for Server-Sent Events (SSE) subscriptions using the GraphQL SSE protocol.

The configuration is as follows:

```yaml
subscription:
  enabled: true
  mode:
    experimental_sse:
      all:
        path: /stream
      subgraphs:
        <subgraph_name>:
          path: /stream
          backoff_factor: 4
```

The following configuration options are available:

| option         | type      | default | description                                                                                                                                                                                                  |
| -------------- | --------- | ------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ | --- |
| enabled        | bool      | true    | If is set to false will ignore all the settings and use the default values.                                                                                                                                  |
| path           | String    | None    | if provided, the SSE client will use the `subgraph_url>/<path>`.                                                                                                                                             |
| reconnect      | bool      | true    | If it is `true` the client will automatically try to reconnect if the stream ends due to an error. If it is `false` the client will stop receiving events after an error.                                    |
| retry_initial  | bool      | true    | If `true` the client will automatically retry the connection, with the same delay and backoff behaviour as for reconnects due to stream error. If `false`, the client will not retry the initial connection. |
| delay          | HumanTime | 1s      | After an error, the client will wait this long before the first attempt to reconnect. Subsequent reconnect attempts may wait longer, depending on the `backoff_factor`.                                      |
| backoff_factor | int       | 2       | Configure the factor by which delays between reconnect attempts will exponentially increase, up to `delay_max`.                                                                                              |
| delay_max      | HumanTime | 60s     | /Configure the maximum delay between reconnects. The exponential backoff configured by `backoff_factor` will not cause a delay greater than this value.                                                      |     |

By [@bakjos](https://github.com/bakjos) in https://github.com/apollographql/router/pull/3715
