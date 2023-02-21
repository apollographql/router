### Fix flaky uplink test ([Issue #2658](https://github.com/apollographql/router/issues/2658))

On Windows the resolution for timed events is 15ms. Increase the leeway on `entitlement_expander_claim_pause_claim` to allow for this.  

By [@bryncooke](https://github.com/bryncooke) in https://github.com/apollographql/router/pull/2659
