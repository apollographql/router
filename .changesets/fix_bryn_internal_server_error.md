### Internal server error handling change ([PR #5159](https://github.com/apollographql/router/pull/5159))

When an http 500 is returned we should not return the details of the error to the client. 
Instead, they are now logged at ERROR level.

In addition, the error is now returned as a graphql error rather than a plaintext error, giving a better experience in sandbox.

By [@BrynCooke](https://github.com/BrynCooke) in https://github.com/apollographql/router/pull/5159
