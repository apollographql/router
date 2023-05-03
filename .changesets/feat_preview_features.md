### Also support "preview" features, in addition to "experimental" features 

This expands on the existing mechanism that was originally introduced in https://github.com/apollographql/router/pull/2242, which supports the notion of an "experimental" feature, and make it compatible with the notion of "preview" features.

When preview or experimental features are used, an `INFO`-level log is emitted during startup to notify of which features are used and shows URLs to their GitHub discussions, for feedback. Additionally, `router config experimental` and `router config preview` CLI sub-commands list all such features in the current Router version, regardless of which are used in a given configuration file.

For more information about launch stages, please see the documentation here: https://www.apollographql.com/docs/resources/product-launch-stages/

By [@o0ignition0o](https://github.com/o0ignition0o), [@abernix](https://github.com/abernix), and [@SimonSapin](https://github.com/SimonSapin) in https://github.com/apollographql/router/pull/2960
