### Connector validation errors no longer print huge shapes

A connector validation error that describes a mismatched shape, such as an object interpolated into a URL, printed that shape in full. When the shape was an object of a type reachable through several fields at each level, the message could run to many megabytes. These messages now print at most 3 levels of nesting and 8 fields or elements of any one shape, with `…` standing for the rest. Shapes within those limits print as before.

This upgrades `shape` to 0.9.2, which adds the bounded printer.

By [@benjamn](https://github.com/benjamn)
