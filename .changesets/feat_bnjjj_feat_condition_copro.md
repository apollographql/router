### Support conditional coprocessor execution per stage of request lifecycle ([PR #5557](https://github.com/apollographql/router/pull/5557))

The router now supports conditional execution of the coprocessor for each stage of the request lifecycle (except for the `Execution` stage).

To configure, define conditions for a specific stage by using selectors based on headers or context entries. For example, based on a supergraph response you can configure the coprocessor not to execute for any subscription:
  


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

To learn more, see the documentation about [coprocessor conditions](https://www.apollographql.com/docs/router/customizations/coprocessor/#conditions).

By [@bnjjj](https://github.com/bnjjj) in https://github.com/apollographql/router/pull/5557