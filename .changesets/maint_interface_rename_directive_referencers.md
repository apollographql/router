### Update directive references when renaming an interface type ([PR #10380](https://github.com/apollographql/router/pull/10380))

Renaming an interface type in `apollo-federation`'s internal schema representation left the old name in the index of types each directive is applied to. Production code does not currently rename interfaces, so there is no user-visible change; this keeps the index consistent for future callers.

By [@inanna-apollo](https://github.com/inanna-apollo) in https://github.com/apollographql/router/pull/10380
