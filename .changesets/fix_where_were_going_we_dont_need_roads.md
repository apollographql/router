### Only process uplink messages that are deemed to be newer ([Issue #2794](https://github.com/apollographql/router/issues/2794))

Uplink is served by multiple cloud providers to ensure high availability. However, this means that there will be periods of time uplink endpoints do not agree on what the latest data is.

This has not been a problem for most users, as the default mode of operation for the router is to fallback to the secondary uplink endpoint if the first fails.
The other mode of operation, triggered by setting `APOLLO_UPLINK_ENDPOINTS` is round-robin. When in this mode there is a much higher chance that the router will end up flapping due to disagreement on the uplink server side or by any user supplied proxies.

This change introduces an interim fix that checks uplink messages against the last known message to see if it is newer. We will be improving the robustness of the solution over the next weeks, so this can be seen as an incremental improvement.

Note that it is advised against using `APOLLO_UPLINK_ENDPOINTS` to try to cache uplink responses for HA purposes. Each request to uplink sends state, therefore limiting the usefulness of such a cache.   

Part of #2794

By [@BrynCooke](https://github.com/BrynCooke) in https://github.com/apollographql/router/pull/2803
