### Add missing schemas for redis connections ([Issue #4173](https://github.com/apollographql/router/issues/4173))

We have supported additional schemas in our redis configuration since we fixed: https://github.com/apollographql/router/issues/3534, but we never updated our redis connection logic to process the new schema options. This is now fixed.

By [@garypen](https://github.com/garypen) in https://github.com/apollographql/router/pull/4174