### Reduce Rhai short circuit response log noise ([PR #8364](https://github.com/apollographql/router/pull/8364))

Rhai scripts that short-circuit the pipeline by throwing now only log an error if a response body isn't present. 

For example the following will NOT log:
```
    throw #{
        status: 403,
        body: #{
            errors: [#{
                message: "Custom error with body",
                extensions: #{
                    code: "FORBIDDEN"
                }
            }]
        }
    };
```

For example the following WILL log:
```
throw "An error occurred without a body";
```
By [@BrynCooke](https://github.com/BrynCooke) in https://github.com/apollographql/router/pull/8364
