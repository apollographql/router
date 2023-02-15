### Support for UUID and Unix timestamp functions in Rhai 

*Description*

When building Rhai scripts, it's often that you'll need to add headers that either uniquely identify a request, or append timestamp information for processing information later, such as crafting a trace header or otherwise. 

While the default `timestamp()` and similar functions (e.g. `apollo_start`) can be used for a somewhat similar function, it also isn't able to be translated into an epoch. As a result, it doesn't acutely address the asks we've heard from users.

This adds a `uuid()` and `unix_now()` function to obtain a UUID and Unix timestamp, respectively.

By [@lleadbet](https://github.com/lleadbet) in https://github.com/apollographql/router/pull/2617
