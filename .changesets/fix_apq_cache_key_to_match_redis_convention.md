### Replaces null separator in apq cache key with : to match redis convention

This PR conforms the apq cache key to follow redis convention. This helps when using redis clients to properly display keys in nested form.

query plan cache key was fixed in a similar pr: #4583

By [@tapaderster](https://github.com/tapaderster) in https://github.com/apollographql/router/pull/4886