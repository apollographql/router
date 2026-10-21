### Bound memory and time in connector validation for large and deeply nested schemas ([PR #10357](https://github.com/apollographql/router/pull/10357))

Connector validation could use memory and time far out of proportion to a subgraph's size:

- Selection validation copied the full list of seen fields at every object it walked. Since 2.16.0 this also applied to list-shaped selections, so an 800-element literal array peaked at 168 MB. It now peaks at 4.4 MB.
- Schema type shapes inlined every referenced type, so a type reachable through more than one field per level (for example `billingAddress` and `shippingAddress` both of type `Address`, repeated down a hierarchy) grew as 2^depth. A 16-level schema of that form used 1.9 GB and took 1.5 s to validate. It now takes under 1 ms.
- Comparing type references compared entire schemas by value, which made validating fields of interfaces with many implementations slow. An interface with 800 implementations took 560 ms to validate. It now takes 75 ms.
- Validating deeply nested `->match` selections took time quadratic in the path depth to collect source locations. Nesting 80 deep took 690 ms. It now takes 65 ms.

Validation results are unchanged. One error message improves: a missing field on a `$this` or `$batch` path that runs through a recursive type, such as `$this.parent.child.missing`, used to be reported from the type where the recursion was cut (`` `Child` doesn't have a field named `missing` ``). It is now reported on the full path (`` `$this.parent.child` doesn't have a field named `missing` ``).

By [@dariuszkuc](https://github.com/dariuszkuc) in https://github.com/apollographql/router/pull/10357
