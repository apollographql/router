### Improve documentation for `Rhai` globals ([Issue #2671](https://github.com/apollographql/router/issues/2671))

The router's `Rhai` interface can simulate closures: https://rhai.rs/book/language/fn-closure.html

However, and this is an important restriction:

"
The [anonymous function](https://rhai.rs/book/language/fn-anon.html) syntax, however, automatically captures [variables](https://rhai.rs/book/language/variables.html) that are not defined within the current scope, but are defined in the external scope – i.e. the scope where the [anonymous function](https://rhai.rs/book/language/fn-anon.html) is created. "

Thus it's not possible for a `Rhai` closure to make reference to a global variable.

This hasn't previously been an issue, but we've now added support for referencing global variables, one at the moment `Router`, and so this kind of thing might be attempted:

```sh
fn supergraph_service(service){
    let f = |request| {
        let v = Router.APOLLO_SDL;
        print(v);
    };
    service.map_request(f);
}
```
That won't work and you'll get an error something like: `service callback failed: Variable not found: Router (line 4, position 17)`

There are two workarounds. Either:

1. Create a local copy of the global that can be captured by the closure:
```
fn supergraph_service(service){
    let v = Router.APOLLO_SDL;
    let f = |request| {
        print(v);
    };
    service.map_request(f);
}
```
Or:
2. Use a function pointer rather than closure syntax:
```
fn supergraph_service(service) {
    const request_callback = Fn("process_request");
    service.map_request(request_callback);
}

fn process_request(request) {
    print(`${Router.APOLLO_SDL}`);
}
```

By [@garypen](https://github.com/garypen) in https://github.com/apollographql/router/pull/3308