### Document plugin ordering ([Issue #3207](https://github.com/apollographql/router/issues/3207))

Requests go through user Rust plugins in the same order as those plugins are configured in
the Router’s YAML configuration file, and responses in reverse order.
This is now documented behavior that users can rely on, with new tests to help maintain it.

Additionally, some Router features happen to use the plugin mechanism internally.
Those now all have a fixed ordering, whereas previous Router versions would use a mixture
of fixed order for some internal plugins and configuration file order for the rest.

By [@SimonSapin](https://github.com/SimonSapin) in https://github.com/apollographql/router/pull/3321
