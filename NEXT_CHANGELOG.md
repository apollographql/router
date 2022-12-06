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

## ❗ BREAKING ❗
## 🚀 Features

### Add support for setting multi-value header keys to rhai ([Issue #2211](https://github.com/apollographql/router/issues/2211))

Adds support for setting a header map key with an array. This causes the HeaderMap key/values to be appended() to the map, rather than inserted().

Example use from rhai as:

```
  response.headers["set-cookie"] = [
    "foo=bar; Domain=localhost; Path=/; Expires=Wed, 04 Jan 2023 17:25:27 GMT; HttpOnly; Secure; SameSite=None",
    "foo2=bar2; Domain=localhost; Path=/; Expires=Wed, 04 Jan 2023 17:25:27 GMT; HttpOnly; Secure; SameSite=None",
  ];
```

By [@garypen](https://github.com/garypen) in https://github.com/apollographql/router/pull/2219

## 🐛 Fixes
## 🛠 Maintenance
## 📚 Documentation
## 🥼 Experimental
