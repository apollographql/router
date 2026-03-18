### Pin transitive `h2` dependency at minimum v0.4.13 to pick up critical flow-control and deadlock fixes ([PR #9033](https://github.com/apollographql/router/pull/9033))

Updates the transitive `h2` dependency `0.4.12 → 0.4.13`. Because `h2` is pulled in via `hyper` and not declared directly, Renovate does not manage it — this PR adds an explicit `[workspace.dependencies]` floor to force the resolution. There are no API or behavioral changes to router code; only the `Cargo.toml` floor declaration and `Cargo.lock` pin are affected.

`h2` 0.4.13 (released January 5, 2026) contains two silent liveness fixes that are particularly relevant to a production router — both produce connections that appear alive but stop transferring data with no error surfaced:

- **Flow-control stall on padded DATA frames ([#869](https://github.com/hyperium/h2/pull/869)):** When a peer sends `DATA` frames with HTTP/2 padding, the padding bytes were not being returned to the flow-control window. This caused the connection-level window to drain to zero and permanently stall downloads. This was confirmed on h2 0.4.12 against real-world servers (e.g., Google's Chrome symbol server).
- **Capacity deadlock under concurrent streams ([#860](https://github.com/hyperium/h2/pull/860)):** Under concurrent load with `max_concurrent_streams` limits in effect, flow-control capacity could be assigned to streams still in `pending_open` state. Those streams could never consume the capacity, starving already-open streams and freezing all outgoing traffic on the connection indefinitely.

By [@theJC](https://github.com/theJC) in https://github.com/apollographql/router/pull/9033
