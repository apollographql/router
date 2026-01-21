### Allow nullable `@key`s for response caching 

`@key`s can be nullable, but there was a bug in the response caching feature that blocked nullable `@key`s from being used. This fixes that behavior. Be careful when caching nullable data because it can be null! Docs added to call that out, but be very sure of what you're caching and write cache keys to be as simple and non-nullable as possible.

By [@aaronArinder](https://github.com/aaronArinder) in https://github.com/apollographql/router/pull/8767
