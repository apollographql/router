### Remove the memory leaks from apollo telemetry ([PR #4094](https://github.com/apollographql/router/pull/4094))

A memory leak in HashMap::into_iter() causes issues for the router.

The relevant rust issues are:

Original issue in HashBrown
https://github.com/rust-lang/hashbrown/issues/438
Update in rust-lang:
https://github.com/rust-lang/rust/pull/116956

The fix will not be available in rust unti mid-November. In the meantime, this fix mitigates the impact on the router in the area most frequently exercising HashMap::into_iter().

By [@garypen](https://github.com/garypen) in https://github.com/apollographql/router/pull/4094