# Changelog for the next release

All notable changes to Router will be documented in this file.

This project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

<!-- <KEEP> THIS IS AN SET OF TEMPLATES TO USE WHEN ADDING TO THE CHANGELOG.

## ❗ BREAKING ❗
## 🚀 Features
## 🐛 Fixes
## 📃 Configuration
## 🛠 Maintenance
## 📚 Documentation
## 🥼 Experimental

## Example section entry format

### Headline ([Issue #ISSUE_NUMBER](https://github.com/apollographql/router/issues/ISSUE_NUMBER))

Description! And a link to a [reference](http://url)

By [@USERNAME](https://github.com/USERNAME) in https://github.com/apollographql/router/pull/PULL_NUMBER
</KEEP> -->

## 🚀 Features

### Add support for `base64::encode()` / `base64::decode()` in Rhai ([Issue #2025](https://github.com/apollographql/router/issues/2025))

Two new functions, `base64::encode()` and `base64::decode()`, have been added to the capabilities available within Rhai scripts to Base64-encode or Base64-decode strings, respectively.

By [@garypen](https://github.com/garypen) in https://github.com/apollographql/router/pull/2394

### Override the root TLS certificate list for subgraph requests ([Issue #1503](https://github.com/apollographql/router/issues/1503))

In some cases, users need to use self-signed certificates or use a custom certificate authority (CA) when communicating with subgraphs.

It is now possible to consigure these certificate-related details using configuration for either specific subgraphs or all subgraphs, as follows:

```yaml
tls:
  subgraph:
    all:
      certificate_authorities: "${file./path/to/ca.crt}"
    # Use a separate certificate for the `products` subgraph.
    subgraphs:
      products:
        certificate_authorities: "${file./path/to/product_ca.crt}"
```

The file referenced in the `certificate_authorities` value is expected to be the combination of several PEM certificates, concatenated together into a single file (as is commonplace with Apache TLS configuration).

These certificates are only configurable via the Router's configuration since using `SSL_CERT_FILE` would also override certificates for sending telemetry and communicating with Apollo Uplink.

While we do not currently support terminating TLS at the Router (from clients), the `tls` is located at the root of the configuration file to allow all TLS-related configuration to be semantically grouped together in the future.

Note: If you are attempting to use a self-signed certificate, it must be generated with the proper file extension and with `basicConstraints` disabled.  For example, a `v3.ext` extension file:

```
subjectKeyIdentifier   = hash
authorityKeyIdentifier = keyid:always,issuer:always
# this has to be disabled
# basicConstraints       = CA:TRUE
keyUsage               = digitalSignature, nonRepudiation, keyEncipherment, dataEncipherment, keyAgreement, keyCertSign
subjectAltName         = DNS:local.apollo.dev
issuerAltName          = issuer:copy
```

Using this `v3.ext` file, the certificate can be generated with the appropriate certificate signing request (CSR) - in this example, `server.csr` - using the following `openssl` command:

```
openssl x509 -req -in server.csr -signkey server.key -out server.crt -extfile v3.ext
```

This will produce the file as `server.crt` which can be passed as `certificate_authorities`.

By [@Geal](https://github.com/geal) in https://github.com/apollographql/router/pull/2008

### Measure the Router's processing time ([Issue #1949](https://github.com/apollographql/router/issues/1949) [Issue #2057](https://github.com/apollographql/router/issues/2057))

The Router now emits a metric called `apollo_router_processing_time` which measures the time spent executing the request **minus** the time spent waiting for an external requests (e.g., subgraph request/response or external plugin request/response).  This measurement accounts both for the time spent actually executing the request sa well as the time spent waiting for concurrent client requests to be executed.  The unit of measurement for the metric is in seconds, though this is not meant to indicate in any way that the Router is going to add actual seconds of overhead.

By [@Geal](https://github.com/geal) in https://github.com/apollographql/router/pull/2371

### Allow the disabling of automated persisted queries (APQ) ([PR #2386](https://github.com/apollographql/router/pull/2386))

Automatic persisted queries (APQ) support is still enabled by default, but can now be disabled using configuration:

```yaml
supergraph:
  apq:
    enabled: false
```

By [@Geal](https://github.com/geal) in https://github.com/apollographql/router/pull/2386

### Anonymous product usage analytics ([Issue #2124](https://github.com/apollographql/router/issues/2124), [Issue #2397](https://github.com/apollographql/router/issues/2397), [Issue #2412](https://github.com/apollographql/router/issues/2412))

Following up on https://github.com/apollographql/router/pull/1630, the Router transmits anonymous usage telemetry about configurable feature usage which helps guide Router product development.  No information is transmitted in our usage collection that includes any request-specific information.  Knowing what features and configuration our users are depending on allows us to evaluate opportunities to reduce complexity and remain diligent about the surface area of the Router over time.  The privacy of your and your user's data is of critical importance to the core Router team and we handle it with great care in accordance with our [privacy policy](https://www.apollographql.com/docs/router/privacy/), which clearly states which data we collect and transmit and offers information on how to opt-out.

Booleans and numeric values are included, however, any strings are represented as `<redacted>` to avoid leaking confidential or sensitive information.

For example:
```json
{
   "session_id": "fbe09da3-ebdb-4863-8086-feb97464b8d7", // Randomly generated at Router startup.
   "version": "1.4.0", // The version of the router
   "os": "linux",
   "ci": null, // If CI is detected then this will name the CI vendor
   "usage": {
     "configuration.headers.all.request.propagate.named.<redacted>": 3,
     "configuration.headers.all.request.propagate.default.<redacted>": 1,
     "configuration.headers.all.request.len": 3,
     "configuration.headers.subgraphs.<redacted>.request.propagate.named.<redacted>": 2,
     "configuration.headers.subgraphs.<redacted>.request.len": 2,
     "configuration.headers.subgraphs.len": 1,
     "configuration.homepage.enabled.true": 1,
     "args.config-path.redacted": 1,
     "args.hot-reload.true": 1,
     //Many more keys. This is dynamic and will change over time.
     //More...
     //More...
     //More...
   }
 }
```

Users can disable this mechanism by setting the environment variable `APOLLO_TELEMETRY_DISABLED=true` in their environment.

By [@bryncooke](https://github.com/bryncooke) in https://github.com/apollographql/router/pull/2173, https://github.com/apollographql/router/issues/2398, https://github.com/apollographql/router/pull/2413


## 🐛 Fixes

### Don't send header names to Studio if `send_headers` is `none` ([Issue #2403](https://github.com/apollographql/router/issues/2403))

We no longer transmit header **names** to Apollo Studio when `send_headers` is set to `none` (the default).  Previously, when `send_headers` was set to `none` (like in the following example) the header names were still transmitted with _empty_ header values.   No actual values were ever being sent unless `send_headers` was sent to a more permissive option like `forward_headers_only` or `forward_headers_except`.

```yaml
telemetry:
  apollo:
    send_headers: none
```

By [@bnjjj](https://github.com/bnjjj) in https://github.com/apollographql/router/pull/2425


### Response with `Content-type: application/json` when encountering incompatible `Content-type` or `Accept` request headers ([Issue #2334](https://github.com/apollographql/router/issues/2334))

When receiving requests with `content-type` and `accept` header mismatches (e.g., on multipart requests) the Router now utilizes a correct `content-type` header in its response.

By [@Meemaw](https://github.com/Meemaw) in https://github.com/apollographql/router/pull/2370

### Fix `APOLLO_USAGE_REPORTING_INGRESS_URL` behavior when Router was run without a configuration file

The environment variable `APOLLO_USAGE_REPORTING_INGRESS_URL` (not usually necessary under typical operation) was **not** being applied correctly when the Router was run without a configuration file.
In addition, defaulting of environment variables now directly injects the variable rather than injecting via expansion expression.  This means that the use of `APOLLO_ROUTER_CONFIG_ENV_PREFIX` (even less common) doesn't affect injected configuration defaults.

By [@bryncooke](https://github.com/bryncooke) in https://github.com/apollographql/router/pull/2432

### Fix panic in schema parse error reporting ([Issue #2269](https://github.com/apollographql/router/issues/2269))

In order to support introspection, 
some definitions like `type __Field { … }` are implicitly added to schemas.
This addition was done by string concatenation at the source level.
In some cases like unclosed braces, a parse error could be reported at a position
beyond the size of the original source.
This would cause a panic because only the unconcatenated string 
is given the the error reporting library `miette`.

Instead, the Router now parses introspection types separately
and “concatenates” definitions at the AST level.

By [@SimonSapin](https://github.com/SimonSapin) in https://github.com/apollographql/router/issues/2448

## 🛠 Maintenance

### Remove unused factory traits ([Issue #2180](https://github.com/apollographql/router/pull/2372))

We removed a factory trait that was only used in a single implementation, which removes the overall requirement that execution and subgraph building take place via that factory trait.

By [@Geal](https://github.com/geal) in https://github.com/apollographql/router/pull/2372

### Optimize header propagation plugin's regular expression matching ([PR #2392](https://github.com/apollographql/router/pull/2392))

We've changed the header propagation plugins' behavior to reduce the chance of memory allocations occurring when applying regex-based header propagation rules.

By [@o0Ignition0o](https://github.com/o0Ignition0o) in https://github.com/apollographql/router/pull/2392

## 📚 Documentation

### Creating custom metrics in plugins ([Issue #2294](https://github.com/apollographql/router/issues/2294))

To create your custom metrics in [Prometheus](https://prometheus.io/) you can use the [`tracing` macros](https://docs.rs/tracing/latest/tracing/index.html#macros) to generate an event. If you observe a specific naming pattern for your event, you'll be able to generate your own custom metrics directly in Prometheus.

To publish a new metric, use tracing macros to generate an event that contains one of the following prefixes:

`monotonic_counter.` _(non-negative numbers)_: Used when the metric will only ever increase.
`counter.`: For when the metric may increase or decrease over time.
`value.`: For discrete data points (i.e., when taking the sum of values does not make semantic sense)
`histogram.`: For building histograms (takes `f64`)

This information is also available in [the Apollo Router documentation](https://www.apollographql.com/docs/router/customizations/native#add-custom-metrics).

By [@bnjjj](https://github.com/bnjjj) in https://github.com/apollographql/router/pull/2417

## 🥼 Experimental

### JWT authentication ([Issue #912](https://github.com/apollographql/router/issues/912))

Experimental JWT authentication is now configurable.  Here's a typical sample configuration fragment:

```yaml
authentication:
  experimental:
    jwt:
      jwks_url: https://dev-zzp5enui.us.auth0.com/.well-known/jwks.json
```

Until the documentation is published, you can [read more about configuring it](https://github.com/apollographql/router/blob/dev/docs/source/configuration/authn-jwt.mdx) in our GitHub repository source.

By [@garypen](https://github.com/garypen) in https://github.com/apollographql/router/pull/2348
