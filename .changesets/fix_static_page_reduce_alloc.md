### Store static pages in `Bytes` structure to avoid expensive allocation per request ([PR #4528](https://github.com/apollographql/router/pull/4528))

The `CheckpointService` created by the `StaticPageLayer` caused a non-insignificant amount of memory to be allocated on every request. The service stack gets cloned on every request, and so does the rendered template.

The template is now stored in a `Bytes` struct instead which is cheap to clone.

By [@xuorig](https://github.com/xuorig) in https://github.com/apollographql/router/pull/4528