### Preserve intermediate dependencies during fetch optimization

Keep separate fetches when combining them would discard an intermediate ordering dependency. This protects downstream representation inputs after transitive reduction while retaining safe sibling and direct parent/child combinations.

By [@inanna-apollo](https://github.com/inanna-apollo) in https://github.com/apollographql/router/pull/10290
