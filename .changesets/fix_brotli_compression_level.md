### Reduce Brotli compression level ([Issue #6857](https://github.com/apollographql/router/issues/6857))

The current Brotli compression level used is `11`, which is the highest quality (and slowest). The value has been
changed to `4`
to mimic the other compression algorithms' `fast` setting; it is also a much more reasonable value for dynamic
workloads.

By [@carodewig](https://github.com/carodewig) in https://github.com/apollographql/router/pull/7007
