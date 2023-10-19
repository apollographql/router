### Router should respond with subscription-protocol header for callback ([Issue #3929](https://github.com/apollographql/router/issues/3929))

Callback protocol documentation specifies that router responds with `subscription-protocol: callback/1.0` header to the initialization (check) message. Currently router does not set this header on the response.

By [@bnjjj](https://github.com/bnjjj) in https://github.com/apollographql/router/pull/3939