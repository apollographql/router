### Bound memory and time in connector validation for large and deeply nested schemas

Connector validation could use memory and time far out of proportion to a subgraph's size:

- Selection validation copied the full list of seen fields at every object it walked. Since 2.16.0 this also applied to list-shaped selections, so an 800-element literal array peaked at 168 MB.
- Schema type shapes inlined every referenced type, so a type reachable through more than one field per level (for example `billingAddress` and `shippingAddress` both of type `Address`, repeated down a hierarchy) grew as 2^depth. A 16-level schema of that form used over 1 GB.
- Comparing type references compared entire schemas by value, which made validating fields of interfaces with many implementations slow.
- Deeply nested `->match` selections rehashed and re-collected shape and name metadata at every level.

Validation results and error messages are unchanged.

By [@dariuszkuc](https://github.com/dariuszkuc) in https://github.com/apollographql/router/pull/PR_NUMBER
