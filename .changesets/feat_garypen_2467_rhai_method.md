### Add support for getting request method in Rhai ([Issue #2467](https://github.com/apollographql/router/issues/2467))

It may be useful to know the method of a request in your Rhai script.

This adds support for getting the method and performing comparisons against String representations.

```
fn process_request(request) {
    if request.method == "OPTIONS"  {
        request.headers["x-custom-header"] = "value"
    }
}
```

Note: You may not modify the method.

By [@garypen](https://github.com/garypen) in https://github.com/apollographql/router/pull/3355