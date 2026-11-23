### Subgraph batching no longer hangs on a root fetch below the top of the plan

With subgraph batching enabled, a client batch waited for every root fetch in each operation's query plan before sending any of them, including root fetches that merge into data an earlier fetch returns. Those fetches can't be sent until the earlier fetch has its response, so the batch never completed. The batch now waits only for the root fetches each plan starts with, and later fetches go to their subgraph as they would without batching.

By [@tninesling](https://github.com/tninesling)
