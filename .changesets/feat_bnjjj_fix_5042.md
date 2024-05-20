### Add support for `status_code` response to Rhai ([Issue #5042](https://github.com/apollographql/router/issues/5042))

The router now supports `response.status_code` on the `Response` interface in Rhai.

Examples using the response status code:

- Converting a response status code to a string:

```rhai
if response.status_code.to_string() == "200" {
    print(`ok`);
}
```

- Converting a response status code to a number:

```rhai
if parse_int(response.status_code.to_string()) == 200 {
    print(`ok`);
}
```

By [@bnjjj](https://github.com/bnjjj) in https://github.com/apollographql/router/pull/5045