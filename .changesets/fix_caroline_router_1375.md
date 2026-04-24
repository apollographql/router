### JWKS fetch errors now log the URL and failure category for easier diagnosis ([PR #9258](https://github.com/apollographql/router/pull/9258))

When the JWKS server is unreachable, the router now logs a specific, actionable message that includes the URL and the failure category (timed out, connection failed, or generic failure) rather than the previous vague `"could not get url"` message.

By [@carodewig](https://github.com/carodewig) in https://github.com/apollographql/router/pull/9258
