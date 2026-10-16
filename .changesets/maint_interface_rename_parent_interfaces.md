### Update parent interfaces' implementer index when renaming an interface type ([PR #10381](https://github.com/apollographql/router/pull/10381))

Renaming an interface that implements other interfaces in `apollo-federation`'s internal schema representation left its old name in each parent interface's list of implementing interfaces. Production code does not currently rename interfaces, so there is no user-visible change; this keeps the index consistent for future callers.

By [@inanna-apollo](https://github.com/inanna-apollo) in https://github.com/apollographql/router/pull/10381
