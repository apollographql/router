### Widen `wait_for_router_ready` budget in callback subscription tests (ROUTER-1881)

`wait_for_router_ready` polled with a 1-second per-request `reqwest` timeout, yielding
only ~59 attempts within the 60-second outer deadline. On ARM Linux CI under heavy
scheduling pressure the router can take ~61 seconds to start accepting HTTP connections
after logging "GraphQL endpoint exposed", causing a spurious panic.

Reduces the per-request timeout to 200ms (~270 attempts per 60s) and widens the outer
deadline at both call sites from 60s to 90s, giving ~400 attempts across 90s.
