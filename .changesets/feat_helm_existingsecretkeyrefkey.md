### Provide support to control of the key name used to retrieve the secret used for APOLLO_KEY ([Issue #5661](https://github.com/apollographql/router/issues/5661))

The Router Helm chart currently is hardcoded to use managedFederationApiKey as the key name to use to retrieve the value 
out of the referenced secretkey.   Some kubernetes customers require the use of a secretStore / externalSecret,
and may be required to use a specific key name when obtaining secrets from these sources.

This change provides the user the ability to control the name of the key to use in retrieving that value.

By [Jon Christiansen](https://github.com/theJC) in https://github.com/apollographql/router/pull/5662
