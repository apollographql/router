### Fix memory issues in the apollo metrics exporter ([PR #4107](https://github.com/apollographql/router/pull/4107))

There were a number of issues with the apollo metrics exporter which meant that under load the router would look as though it was leaking memory. It isn't a leak, strictly speaking, but is in fact "lingering" memory.

The root cause was a bounded `futures` channel which did not enforce the bounds as we expected and thus could over-consume memory. We have fixed the issue by:

 - making the situation of channel overflow less likely to happen
   - speeding up metrics processing
   - altering the priority of metrics submission vs metrics gathering
 - switching to use a `tokio` bounded channel which enforces the bound as we originally expected

With these changes in place we have observed that the router behaves very well with respect to memory consumption under high load.

By [@garypen](https://github.com/garypen) in https://github.com/apollographql/router/pull/4107