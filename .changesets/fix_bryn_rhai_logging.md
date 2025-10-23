### (fix) Rhai short circuit responses will not log if a body is present ([PR #8364](https://github.com/apollographql/router/pull/8364))

Rhai scripts that short circuited the pipeline by throwing would now only log an error if a response body is not present. 

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
