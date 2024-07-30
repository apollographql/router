### Helm: Support renaming key for retrieving APOLLO_KEY secret ([Issue #5661](https://github.com/apollographql/router/issues/5661))

A user of the router Helm chart can now rename the key used to retrieve the value of the secret key referenced by `APOLLO_KEY`.

Previously, the router Helm chart hardcoded the key name to `managedFederationApiKey`. This didn't support users whose infrastructure required custom key names when getting secrets, such as Kubernetes users who need to use specific key names to access a `secretStore` or `externalSecret`. This change provides a user the ability to control the name of the key to use in retrieving that value.

By [Jon Christiansen](https://github.com/theJC) in https://github.com/apollographql/router/pull/5662
