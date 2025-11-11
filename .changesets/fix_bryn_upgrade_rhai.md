### Parse scientific notation correctly in Rhai scripts ([PR #8528](https://github.com/apollographql/router/pull/8528))

The router now correctly parses scientific notation (like `1.5e10`) in Rhai scripts and JSON operations. Previously, the Rhai scripting engine failed to parse these numeric formats, causing runtime errors when your scripts processed data containing exponential notation.

This fix upgrades Rhai from 1.21.0 to 1.23.6, resolving the parsing issue and ensuring your scripts handle scientific notation seamlessly.

By [@BrynCooke](https://github.com/BrynCooke) in https://github.com/apollographql/router/pull/8528
