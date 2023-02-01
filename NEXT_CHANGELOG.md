# Changelog for the next release

All notable changes to Router will be documented in this file.

This project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

<!-- <KEEP> THIS IS AN SET OF TEMPLATES TO USE WHEN ADDING TO THE CHANGELOG.

## ❗ BREAKING ❗
## 🚀 Features
## 🐛 Fixes
## 📃 Configuration
Configuration changes will be [automatically migrated on load](https://www.apollographql.com/docs/router/configuration/overview#upgrading-your-router-configuration). However, you should update your source configuration files as these will become breaking changes in a future major release.
## 🛠 Maintenance
## 📚 Documentation
## 🥼 Experimental

## Example section entry format

### Headline ([Issue #ISSUE_NUMBER](https://github.com/apollographql/router/issues/ISSUE_NUMBER))

Description! And a link to a [reference](http://url)

By [@USERNAME](https://github.com/USERNAME) in https://github.com/apollographql/router/pull/PULL_NUMBER
</KEEP> -->

## 🛠 Maintenance

### CI: Enable compliance checks /except/ licenses.html update ([Issue #2514](https://github.com/apollographql/router/issues/2514))

In [#1573](https://github.com/apollographql/router/pull/1573), we removed the compliance checks for non-release CI pipelines, because the cargo-about output would change ever so slightly.
Some checks however are very important and prevent us from inadvertently downgrading libraries and needing to open [#2512](https://github.com/apollographql/router/pull/2512).

This PR does the following changes:
- Introduce `cargo xtask licenses` to check and update licenses.html.
- Separate compliance (cargo-deny, which includes license checks) and licenses generation (cargo-about) in xtask
- Enable compliance as part of our CI checks for each open PR
- Update cargo xtask all so it checks compliance and licenses

Updating licenses.html is now driven by `cargo xtask licenses`, which is part of the release checklist.

By [@o0Ignition0o](https://github.com/o0Ignition0o) in https://github.com/apollographql/router/pull/XXX_TBD
