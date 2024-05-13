### Add support of response status_code in rhai ([Issue #5042](https://github.com/apollographql/router/issues/5042))

Added support of `response.status_code` on `Response` interface in rhai.

Convert response status code to a string.

```rhai
if response.status_code.to_string() == "200" {
    print(`ok`);
}
```

Also useful if you want to convert response status code to a number

```rhai
if parse_int(response.status_code.to_string()) == 200 {
    print(`ok`);
}
```

By [@bnjjj](https://github.com/bnjjj) in https://github.com/apollographql/router/pull/5045