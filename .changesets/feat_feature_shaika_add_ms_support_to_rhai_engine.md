### Add support for `unix_ms_now` in Rhai customizations ([Issue #5182](https://github.com/apollographql/router/issues/5182))

Rhai customizations can now use the `unix_ms_now()` function to obtain the current Unix timestamp in milliseconds since the Unix epoch. 

For example:

```rhai
fn supergraph_service(service) {
    let now = unix_ms_now();
}