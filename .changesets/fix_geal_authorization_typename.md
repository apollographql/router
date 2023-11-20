### special case for __typename in authorization ([PR #3821](https://github.com/apollographql/router/pull/3821))

When evaluating authorization directives on fields returning interfaces,
we require the usage of fragments with type conditions if the interface
implementors have different security requirements. For the __typename
field though, we must make an exception, because it should be available
for all implementors


By [@Geal](https://github.com/Geal) in https://github.com/apollographql/router/pull/3821