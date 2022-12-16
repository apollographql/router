# Changelog for the next release

All notable changes to Router will be documented in this file.

This project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

<!-- <THIS IS AN EXAMPLE, DO NOT REMOVE>

# [x.x.x] (unreleased) - 2022-mm-dd
> Important: X breaking changes below, indicated by **❗ BREAKING ❗**
## ❗ BREAKING ❗
## 🚀 Features
## 🐛 Fixes
## 🛠 Maintenance
## 📚 Documentation
## 🥼 Experimental

## Example section entry format

### Headline ([Issue #ISSUE_NUMBER](https://github.com/apollographql/router/issues/ISSUE_NUMBER))

Description! And a link to a [reference](http://url)

By [@USERNAME](https://github.com/USERNAME) in https://github.com/apollographql/router/pull/PULL_NUMBER
-->

# [x.x.x] (unreleased) - 2022-mm-dd

## 🚀 Features

### Apollo uplink: Configurable schema poll timeout ([PR #2271](https://github.com/apollographql/router/pull/2271))

In addition to the url and poll interval, Uplink poll timeout can now be configured via command line arg and env variable:

```bash
        --apollo-uplink-timeout <APOLLO_UPLINK_TIMEOUT>
            The timeout for each of the polls to Apollo Uplink. [env: APOLLO_UPLINK_TIMEOUT=] [default: 30s]
```

It defaults to 30 seconds.

By [@o0Ignition0o](https://github.com/o0Ignition0o) in https://github.com/apollographql/router/pull/2271

### Override the root certificate list for subgraph requests ([Issue #1503](https://github.com/apollographql/router/issues/1503))

we might want to connect over TLS to a subgraph with a self signed certificate, or using a custom certificate authority.
This adds a configuration option to set the list of certificate authorities for all the subgraphs, as follows:

```yaml
tls:
  subgraphs:
    certificate_authorities_path: /path/to/ca.crt
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


## 🐛 Fixes

### Replace notify recommended watcher with PollWatcher ([Issue #2245](https://github.com/apollographql/router/issues/2245))

We noticed that we kept receiving issues about hot reload. We tried to fix this a while back by moving from HotWatch to Notify, but there are still issues. The problem appears to be caused by having different mechanisms on different platforms. Switching to the PollWatcher, which offers less sophisticated functionality but is the same on all platforms, should solve these issues at the expense of slightly worse reactiveness.

By [@garypen](https://github.com/garypen) in https://github.com/apollographql/router/pull/2276

### Keep the error path when redacting subgraph errors ([Issue #1818](https://github.com/apollographql/router/issues/1818))

Error redaction was erasing the error's path, which made it impossible to affect the errors to deferred responses. Now the redacted errors keep the path. Since the response shape for the primary and deferred responses are defined from the API schema, there is no possibility of leaking internal schema information here.

By [@Geal](https://github.com/geal) in https://github.com/apollographql/router/pull/2273

## 🛠 Maintenance

### Return more consistent errors ([Issue #2101](https://github.com/apollographql/router/issues/2101))

Change some of our errors we returned by following [this specs](https://www.apollographql.com/docs/apollo-server/data/errors/). It adds a `code` field in `extensions` describing the current error. 

By [@bnjjj](https://github.com/bnjjj) in https://github.com/apollographql/router/pull/2178

## 🥼 Experimental


### Introduce a `router_service` ([Issue #1496](https://github.com/apollographql/router/issues/1496))

A `router_service` is now part of our service stack, which allows plugin developers to process raw http requests and raw http responses, that wrap the already available `supergraph_service`

By [@o0Ignition0o](https://github.com/o0Ignition0o) in https://github.com/apollographql/router/pull/2170
