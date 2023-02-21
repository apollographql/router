### Add a rhai global variable resolver and populate it ([Issue #2628](https://github.com/apollographql/router/issues/2628))

rhai doesn't place constants into global scope. Functions are expected to be pure. We only became aware of this limitation this week, mainly as a consequence of people having difficulties with the experimental APOLLO_AUTHENTICATION_JWT_CLAIMS constant.

Our fix is to introduce a new global variable resolver and populate it with a `Router` global constant. It has three members:

 - `APOLLO_START -> should be used in place of `apollo_start`
 - `APOLLO_SDL -> should be used in place of `apollo_sdl`
 - `APOLLO_AUTHENTICATION_JWT_CLAIMS`

You access a member of this variable as follows:

```
   let my_var = Router.APOLLO_SDL;
```

We retain the existing non-experimental constants (for purposes of backwards compatibility), but recommend that you shift to the new global constants. We are removing the APOLLO_AUTHENTICATION_JWT_CLAIMS constant.

By [@garypen](https://github.com/garypen) in https://github.com/apollographql/router/pull/2627