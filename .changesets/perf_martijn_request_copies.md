### Serialize the client's request for header rules only where a rule reads it

The headers plugin serialized the client's whole request on every subgraph and connector request, rule or no rule. It now does nothing for a subgraph or connector with no header rule, and serializes the client's request only where an `insert` rule reads its body (`from_body`). The byte count of each subgraph reply also no longer boxes a future per request. Header rules and counts are unchanged.

By [@martijnwalraven](https://github.com/martijnwalraven)
