### Remove last references to federation-demo ([Issue #2680](https://github.com/apollographql/router/issues/2680))

The federation-demo was used for testing in early versions of the Router but is no longer used.
The docker-compose.yml has been updated to reflect this, and now also includes Redis which is required for some tests. 

By [@bryncooke](https://github.com/bryncooke) in https://github.com/apollographql/router/pull/#2681
