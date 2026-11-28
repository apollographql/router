### Add the `->flatten` mapping method for connectors

Adds the `->flatten` mapping method, which flattens a list of lists by one level, or by an optional integer depth, like JavaScript's `Array.prototype.flat`. Non-list inputs become a one-element list, as in `->map` and `->filter`. Available in every connect spec version.

By [@fernando-apollo](https://github.com/fernando-apollo) in https://github.com/apollographql/router/pull/10785
