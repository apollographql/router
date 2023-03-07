### Update local development Docker Compose configuration  ([Issue #2680](https://github.com/apollographql/router/issues/2680))

The `federation-demo` was used for testing in early versions of the Router but is no longer used, and we removed most references to it some time ago.  The `docker-compose.yml` (used primarily in the development of this repository) has been updated to reflect this, and now also includes Redis which is required for some tests. 

By [@bryncooke](https://github.com/bryncooke) in https://github.com/apollographql/router/pull/#2681
