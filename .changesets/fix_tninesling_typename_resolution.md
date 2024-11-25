### Fix incorrect overriding of concrete type names with interface names when merging responses ([PR #6250](https://github.com/apollographql/router/pull/6250))

When using `@interfaceObject`, differing pieces of data can come back with either concrete types or interface types depending on the source. Previously, receiving the data in a particular order could incorrectly result in the interface name of a type overwriting its concrete name.

To make the response merging order-agnostic, the router now checks the schema to ensure concrete types are not overwritten with interfaces or less specific types.

By [@tninesling](https://github.com/tninesling) in https://github.com/apollographql/router/pull/6250
