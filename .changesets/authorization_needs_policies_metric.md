### Report operations that require policy authorization

The authorization metric now includes the `authorization.needs_policies` attribute. The router also
emits the metric for operations that require `@policy` labels but don't require authentication or
scope checks.
