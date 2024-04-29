### make the coprocessor URL configurable per stage ([PR #5005](https://github.com/apollographql/router/pull/5005))

This makes it possible to have different coprocessor URLs per stage. The initial URL configuration still serves as a global default, but it can be overriden per stage.

As an example, we can have a router request and supergraph request coprocessors with different URLs:

```yaml title="router.yaml"
coprocessor:
  url: http://127.0.0.1:8081
  router:
    request:
      headers: true
  supergraph::
    request:
      body: true
      url: http://127.0.0.1:8082
```


By [@Geal](https://github.com/Geal) in https://github.com/apollographql/router/pull/5005