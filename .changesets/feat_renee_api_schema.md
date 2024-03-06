## Experimental support for API Schema generation in Rust

As part of the process of replacing JavaScript code in the Router with more reliable and performant Rust implementations, the API schema can now be generated in Rust.

By default, the Router will transparently use both the JavaScript and Rust implementations side-by-side for a while, to make sure that there are no breaking changes with a wide variety of schemas. If this unexpectedly causes any issues, it's possible to revert to the JavaScript implementation by providing the new `experimental_api_schema_generation_mode` configuration option:

```yaml
experimental_api_schema_generation_mode: legacy
```

Once we are confident in the results, this experimental option will be removed and only the Rust-based API schema generation will be used.
