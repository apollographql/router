### Fix non-parent sampling ([PR #6481](https://github.com/apollographql/router/pull/6481))

When the user specifies a non-parent sampler the router should ignore the information from upstream and use its own sampling rate.

The following configuration would not work correctly:

```
  exporters:
    tracing:
      common:
        service_name: router
        sampler: 0.00001
        parent_based_sampler: false
```
All spans are being sampled.
This is now fixed and the router will correctly ignore any upstream sampling decision.

By [@BrynCooke](https://github.com/BrynCooke) in https://github.com/apollographql/router/pull/6481
