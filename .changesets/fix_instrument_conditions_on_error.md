### Respect custom instrument conditions in all error cases ([Issue #10525](https://github.com/apollographql/router/issues/10525))

Custom counters and histograms with a `condition` could be recorded even when the condition was false when certain errors occurred, such as a coprocessor failure. This could lead to overcounts and to metrics that are surprisingly missing attributes. The condition is now applied in all cases.

By [@glasser](https://github.com/glasser) in https://github.com/apollographql/router/pull/10526
