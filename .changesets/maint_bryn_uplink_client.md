### Uplink connections now reuse reqwest client ([Issue #3333](https://github.com/apollographql/router/issues/3333))

Previously uplink requests created a new reqwest client each time, this may cause CPU spikes especially on OSX.
A single client will now be shared between requests of the same type. 

By [@BrynCooke](https://github.com/BrynCooke) in https://github.com/apollographql/router/pull/3703