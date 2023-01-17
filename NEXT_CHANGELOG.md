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

## 🚀 Features

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
