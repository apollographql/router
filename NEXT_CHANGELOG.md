# Changelog for the next release

All notable changes to Router will be documented in this file.

This project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

<!-- <THIS IS AN EXAMPLE, DO NOT REMOVE>

# [x.x.x] (unreleased) - 2022-mm-dd
> Important: X breaking changes below, indicated by **❗ BREAKING ❗**
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
-->

# [1.9.0] (unreleased) - 2023-mm-dd

## 🚀 Features

### Add support for `base64::encode()` / `base64::decode()` in Rhai ([Issue #2025](https://github.com/apollographql/router/issues/2025))

Two new functions, `base64::encode()` and `base64::decode()` may now be used to Base64-encode or Base64-decode strings, respectively.

By [@garypen](https://github.com/garypen) in https://github.com/apollographql/router/pull/2394

### Anonymous product usage analytics ([Issue #2124](https://github.com/apollographql/router/issues/2124), [Issue #2397](https://github.com/apollographql/router/issues/2397), [Issue #2412](https://github.com/apollographql/router/issues/2412))

Following up on https://github.com/apollographql/router/pull/1630, the Router transmits anonymous usage telemetry about configurable feature usage which helps guide Router product development.  No information is transmitted in our usage collection that includes any request-specific information.  Knowing what features and configuration our users are depending on allows us to evaluate opportunity to reduce complexity and remain diligent about the surface area of the Router.  The privacy of your and your user's data is of critical importantance to the core Router team and we handle it in accordance with our [privacy policy](https://www.apollographql.com/docs/router/privacy/), which clearly states which data we collect and transmit and offers information on how to opt-out.
Note that strings are output as `<redacted>` so that we do not leak confidential or sensitive information.
Boolean and numerics are output.

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
     "configuration.headers.all.request.len": 3
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

Users can disable the sending this data by using the command line flag `--anonymous-telemetry-disabled` or setting the environment variable `APOLLO_TELEMETRY_DISABLED=true`

By [@bryncooke](https://github.com/bryncooke) in https://github.com/apollographql/router/pull/2173, https://github.com/apollographql/router/issues/2398, https://github.com/apollographql/router/pull/2413

### Override the root certificate list for subgraph requests ([Issue #1503](https://github.com/apollographql/router/issues/1503))

we might want to connect over TLS to a subgraph with a self signed certificate, or using a custom certificate authority.
This adds a configuration option to set the list of certificate authorities for all the subgraphs, as follows:

```yaml
tls:
  subgraph:
    all:
      certificate_authorities: "${file./path/to/ca.crt}"
    # override per subgraph
    subgraphs:
      products:
        certificate_authorities: "${file./path/to/product_ca.crt}"
```

The file is expected to be a list of certificates in PEM format, concatenated (as in Apache configuration).

This uses a configuration option because the SSL_CERT_FILE environment variable would override certificates for telemetry and Uplink as well.
The configuration option takes root in a tls field to allow for future work around TLS termination in the router (if it does not happen, the option is fine as is, but if it does, we would like to have them in the same place). This is a global option for all subgraphs.

If this is used with self signed certificates, those certificates have to be generated with the proper extensions:

extensions file `v3.ext`:

```
subjectKeyIdentifier   = hash
authorityKeyIdentifier = keyid:always,issuer:always
# this has to be disabled
# basicConstraints       = CA:TRUE
keyUsage               = digitalSignature, nonRepudiation, keyEncipherment, dataEncipherment, keyAgreement, keyCertSign
subjectAltName         = DNS:local.apollo.dev
issuerAltName          = issuer:copy
```

And the certificate can be generated as follows from a certificate signing request:

```
openssl x509 -req -in server.csr -signkey server.key -out server.crt -extfile v3.ext
```

By [@Geal](https://github.com/geal) in https://github.com/apollographql/router/pull/2008

### Make APQ optional ([PR #2386](https://github.com/apollographql/router/pull/2386))

Automatic persisted queries support is enabled by default, this adds an option to deactivate it:

```yaml
supergraph:
  apq:
    enabled: false
```

By [@Geal](https://github.com/geal) in https://github.com/apollographql/router/pull/2386

## 🐛 Fixes

### Specify content type to `application/json` on requests with content-type/accept header missmatch ([Issue #2334](https://github.com/apollographql/router/issues/2334))

When receiving requests with invalid content-type/accept header missmatch (e.g multipart requests) , it now specifies the right `content-type` header.

By [@Meemaw](https://github.com/Meemaw) in https://github.com/apollographql/router/pull/2370


## 🛠 Maintenance

### Remove unused factory traits ([Issue #2180](https://github.com/apollographql/router/pull/2372))

Building the execution and subgraph services had to go through a factory trait before, which is not
needed anymore since there is only one useful implementation.

By [@Geal](https://github.com/geal) in https://github.com/apollographql/router/pull/2372

### Optimize header propagation plugin's regex matching ([PR #2392](https://github.com/apollographql/router/pull/2392))

We've changed the plugin to reduce the chances of generating memory allocations when applying regex-based header propagation rules.

By [@o0Ignition0o](https://github.com/o0Ignition0o) in https://github.com/apollographql/router/pull/2392

## 📚 Documentation

### Add documentation to create custom metrics in plugins ([Issue #2294](https://github.com/apollographql/router/issues/2294))

To create your custom metrics in [Prometheus](https://prometheus.io/) you can use the [tracing macros](https://docs.rs/tracing/latest/tracing/index.html#macros) to generate an event. If you observe a specific naming pattern for your event you'll be able to generate your own custom metrics directly in Prometheus.

To publish a new metric, use tracing macros to generate an event that contains one of the following prefixes:

`monotonic_counter.` (non-negative numbers): Used when the counter should only ever increase
`counter.`: Used when the counter can go up or down
`value.`: Used for discrete data points (i.e., summing them does not make semantic sense)
`histogram.`: Used for histograms (takes f64)

By [@bnjjj](https://github.com/bnjjj) in https://github.com/apollographql/router/pull/2417

## 🥼 Experimental

### JWT authentication for the router ([Issue #912](https://github.com/apollographql/router/issues/912))

Experimental JWT authentication is now configurable for the router.

Here's a typical sample configuration fragment:

```yaml
authentication:
  jwt:
    jwks_url: https://dev-zzp5enui.us.auth0.com/.well-known/jwks.json
```

Until the documentation is published to the website, you can read [it](https://github.com/apollographql/router/blob/dev/docs/source/configuration/authn-jwt.mdx) from the repository.

By [@garypen](https://github.com/garypen) in https://github.com/apollographql/router/pull/2348

