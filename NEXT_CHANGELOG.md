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

### JWT authentication for the router ([Issue #912](https://github.com/apollographql/router/issues/912))

JWT authentication is now configurable for the router.

Here's a typical sample configuration fragment:

```yaml
authentication:
  jwt:
    jwks_url: https://dev-zzp5enui.us.auth0.com/.well-known/jwks.json
```

Until the documentation is published to the website, you can read [it](https://github.com/apollographql/router/blob/53d7b710a6bdc0fbef4d7fd0d13f49002ee70e84/docs/source/configuration/authn-jwt.mdx) from the pull request.

By [@garypen](https://github.com/garypen) in https://github.com/apollographql/router/pull/2348
