### Support disabling the health check endpoint ([PR #7519](https://github.com/apollographql/router/pull/7519))

During the development of Router 2.0, the health check endpoint support was converted to be a plugin. Unfortunately, the support for disabling the health check endpoint was lost during the conversion.

This is now fixed and a new unit test ensures that disabling the health check does not result in the creation of a health check endpoint.

By [@garypen](https://github.com/garypen) in https://github.com/apollographql/router/pull/7519
