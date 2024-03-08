### use Object::with_capacity with selection set len in format_response ([PR #4775](https://github.com/apollographql/router/pull/4775))

This is far from a great improvement, I think this procedure probably needs to be changed to make a bigger dent. Maybe re-ordering and modifying in place, which would require much more effort. This change does seem to make a difference comparing flamegraphs, but it's small.

By [@xuorig](https://github.com/xuorig) in https://github.com/apollographql/router/pull/4775