### add more details to the state machine `Running` processing ([Issue #3065](https://github.com/apollographql/router/issues/3065))

It's difficult to know from the logs why a router state reload has been triggered and the current logs don't make it clear that it's possible for a state change trigger to be ignored.

This change adds details about the triggers of potential state changes to the logs and also make it easier to see when an unentitled event causes a state change to be ignored.

By [@garypen](https://github.com/garypen) in https://github.com/apollographql/router/pull/3066