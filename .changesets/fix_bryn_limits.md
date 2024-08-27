### Payload limits may exceed configured maximum ([Issue #ISSUE_NUMBER](https://github.com/apollographql/router/issues/ISSUE_NUMBER))

When processing requests the configured limits as defined in the `limits` section may be ignored:
```yaml
limits:
  http_max_request_bytes: 2000000
```

Plugins that execute services during the `router` lifecycle will not respect the configured limits. Potentially leading to a denial of service attack vector.

#### Built features affected:
* Coprocessors configured to send the entire body of a request are vulnerable to this issue:
```yaml
coprocessor: 
  url: http://localhost:8080
  router: 
    request:
      body: true
```

#### Fix details
Body size limits are now moved to earlier in the pipeline to ensure that coprocessors and user plugins respect
the configured limits.
Reading a request body past the configured limit will now abort the request and return a 413 response 
to the client instead of delegating to the code reading the body to handle the error.

#### User impact
Body size limits are now enforced for all requests in the main graphql router pipeline. Custom plugins are covered by 
this and any attempt to read the body past the configured limit will abort the request and return a 413 response to the client.

Coprocessors, rhai and native plugins do not have an opportunity to intercept aborted requests. It is advised to use 
the telemetry features within the router if you need to track these events.

By [@bryncooke](https://github.com/AUTHOR) in https://github.com/apollographql/router/pull/PULL_NUMBER
