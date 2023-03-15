### make argument parsing optional in the `Executable` builder ([PR #2666](https://github.com/apollographql/router/pull/2666))

The `Executable` builder was parsing command line arguments, which was causing issues when used as part of a larger application with its own set of flags, that would not be recognized by the router. This allows parsing the arguments separately, then passing the required ones to the `Executable` builder directly. The default behaviour is still parsing from inside the builder.

By [@Geal](https://github.com/Geal) in https://github.com/apollographql/router/pull/2666