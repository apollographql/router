### use Object::with_capacity with selection set len in format_response ([PR #4775](https://github.com/apollographql/router/pull/4775))

This preallocates output object size in response formatting, bringing a small performance improvement

By [@xuorig](https://github.com/xuorig) in https://github.com/apollographql/router/pull/4775