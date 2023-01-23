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

## 🐛 Fixes

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


## 📚 Documentation

### Added documentation for listening on IPv6 ([Issue #1835](https://github.com/apollographql/router/issues/1835))

Added documentation for listening on IPv6
```yaml
supergraph:
  # The socket address and port to listen on. 
  # Note that this must be quoted to avoid interpretation as a yaml array.
  listen: '[::1]:4000'
```

By [@bryncooke](https://github.com/bryncooke) in https://github.com/apollographql/router/pull/2440


