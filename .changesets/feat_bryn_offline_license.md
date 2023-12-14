### Offline license support ([Issue #4219](https://github.com/apollographql/router/issues/4219))

Running your Apollo Router fleet while fully connected to GraphOS is the best choice for the majority of Apollo users. However, there are scenarios that can prevent your routers from connecting to GraphOS for an extended period of time, ranging from disasters that break connectivity to isolated sites operating with air-gapped networks. If you need to restart or rapidly scale your entire router fleet but you're unable to communicate with Apollo Uplink, new router instances won't be able to serve traffic.

To support long-term disconnection scenarios, GraphOS supports **offline Enterprise licenses** for the Apollo Router. An offline license enables your routers to start and serve traffic without a persistent connection to GraphOS. The functionality available with an offline license is much of the same as being fully connected to GraphOS, including managed federation for supergraph CI (operation checks, schema linting, contracts, etc.).

Offline Enterprise license support for Apollo is available on an as-needed basis. It must be enabled on your Studio organization. For access, send a request to your Apollo contact.

By [@bryncooke](https://github.com/bryncooke) in https://github.com/apollographql/router/pull/4372
