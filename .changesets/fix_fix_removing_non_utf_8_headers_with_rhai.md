### Fix error when removing non-UTF-8 headers with Rhai plugin ([PR #7801](https://github.com/apollographql/router/pull/7801))

When trying to remove non-UTF-8 headers from a Rhai plugin, users were faced with an unhelpful error. Now, non-UTF-8 values will be lossy converted to UTF-8 when accessed from Rhai. This change affects `get`, `get_all`, and `remove` operations.

By [@Velfi](https://github.com/Velfi) in https://github.com/apollographql/router/pull/7801
