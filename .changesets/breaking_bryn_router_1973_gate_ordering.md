### Request validation errors answer before authorization errors ([PR #9911](https://github.com/apollographql/router/pull/9911))

Authorization enforcement runs at execution, after the router validates the request. An operation that fails both checks now receives the validation error alone: a missing or invalid variable returns the 400 validation response, and a subscription or `@defer` operation sent without the matching `Accept` header returns the 406, where these previously received the authorization errors. Fixing the request then surfaces the authorization errors.

By [@BrynCooke](https://github.com/BrynCooke) in https://github.com/apollographql/router/pull/9911
