### Brief but complete sentence that stands on its own ([Issue #ISSUE_NUMBER](https://github.com/apollographql/router/issues/4939))

This adds a new `sha256` module to create SHA256 hashes within Rhai scripts. An example looks like: 

```rs
fn supergraph_service(service){
    service.map_request(|request|{
        log_info("hello world");
        let sha = sha256::digest("hello world");
        log_info(sha);
    });
}
```

The only function currently is `digest`. 

By [@lleadbet](https://github.com/lleadbet) in https://github.com/apollographql/router/pull/4940
