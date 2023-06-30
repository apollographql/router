### Enable serde_json float_roundtrip feature ([Issue #2951](https://github.com/apollographql/router/issues/2951))

This feature is explicitly designed to:

"""
Use sufficient precision when parsing fixed precision floats from JSON to ensure that they maintain accuracy when round-tripped through JSON. This comes at an approximately 2x performance cost for parsing floats compared to the default best-effort precision.
"""

Despite the performance impact for floats, we need the fix in order to prevent float values losing precision when processed by the router.

By [@garypen](https://github.com/garypen) in https://github.com/apollographql/router/pull/3338