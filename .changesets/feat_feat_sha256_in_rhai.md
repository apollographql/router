### Add support for SHA256 hashing in Rhai ([Issue #4939](https://github.com/apollographql/router/issues/4939))

The router supports a new `sha256` module to create SHA256 hashes in Rhai scripts. The module supports the `sha256::digest` function.

An example script that uses the module: 

```rs
fn supergraph_service(service){
    service.map_request(|request|{
        log_info("hello world");
        let sha = sha256::digest("hello world");
        log_info(sha);
    });
}
```


By [@lleadbet](https://github.com/lleadbet) in https://github.com/apollographql/router/pull/4940
