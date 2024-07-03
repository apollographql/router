### feat(coprocessor): add conditions on coprocessor stages ([PR #5557](https://github.com/apollographql/router/pull/5557))

Adding support of conditions on coprocessor stages (except for `Execution` service) based on existing [conditions](https://www.apollographql.com/docs/router/configuration/telemetry/instrumentation/conditions/) we already have in the router for custom telemetry. Including usage of existing [selectors](https://www.apollographql.com/docs/router/configuration/telemetry/instrumentation/selectors/)

Example if you would like to execute a coprocessor on supergraph response only on primary response and not subscription events for example:

```yaml title=router.yaml
coprocessor:
  url: http://127.0.0.1:3000 # mandatory URL which is the address of the coprocessor
  timeout: 2s # optional timeout (2 seconds in this example). If not set, defaults to 1 second
  supergraph:
    response: 
      condition:
        eq:
        - true
        - is_primary_response: true
      body: true
```

or another example to not execute the coprocessor at all if it's a subscription:

```yaml title=router.yaml
coprocessor:
  url: http://127.0.0.1:3000 # mandatory URL which is the address of the coprocessor
  timeout: 2s # optional timeout (2 seconds in this example). If not set, defaults to 1 second
  supergraph:
    response: 
      condition:
        not:
          eq:
          - subscription
          - operation_kind: string
      body: true
```

By [@bnjjj](https://github.com/bnjjj) in https://github.com/apollographql/router/pull/5557