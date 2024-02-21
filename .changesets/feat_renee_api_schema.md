## Experimental support for API Schema generation in Rust

As part of the process of replacing JavaScript code in the Router with more reliable and performant Rust implementations, the API schema can now be generated in Rust. The new `experimental_api_schema_generation_mode` option controls which implementation is used.

```yaml {4,8} title="router.yaml"
experimental_api_schema_generation_mode: both
```

The default mode, `both`, runs JavaScript and Rust API schema generation side by side, compares the results, and reports back to Apollo when there is a difference. If you run into problems, set this option to `legacy` to revert to using JavaScript only.

Once we are confident in the results, this experimental option will be removed and only the Rust-based API schema generation will be used.
