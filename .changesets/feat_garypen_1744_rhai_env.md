### Add environment variable access to rhai ([Issue #1744](https://github.com/apollographql/router/issues/1744))

This introduces support for a new env module. There's no need to import it since it is already imported by the router Rhai engine. The `env` module contains one function:

```
fn get(key) -> value
```

`key` is expected to be a String and `value` is a String. The function may fail, so exceptions should be handled. Usage is fully documented in the router docs.

This example Rhai script illustrates how to use it:

```
    try {
        print(`LANG: ${env::get("LANG")}`);
    } catch(err) {
        print(`exception: ${err}`);
    }
```

fixes: #1744

By [@garypen](https://github.com/garypen) in https://github.com/apollographql/router/pull/3240