### Allow thrown rhai errors to opt out of logging in the Router ([PR #6124](https://github.com/apollographql/router/pull/6124))

By default, the Router will log an error when a Rhai script uses a `throw` to throw an error. You can opt out of this on an individual `throw` by specifying `log_error: false`:

```rhai
fn supergraph_service(service) {
    let f = |request| {
        throw #{
            status: 403,
            log_error: false, // Do not log this error
            body: #{
                errors: [#{
                    message: `I have raised a 403`,
                    extensions: #{
                        code: "ACCESS_DENIED"
                    }
                }]
            }
        };
    };
    service.map_request(f);
}
```

By [@andrewmcgivery](https://github.com/andrewmcgivery) in https://github.com/apollographql/router/pull/6124
