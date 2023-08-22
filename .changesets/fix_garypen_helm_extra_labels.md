### Helm: If there are `extraLabels` add them to all resources ([PR #3622](https://github.com/apollographql/router/pull/3622))

This extends the functionality of `extraLabels` so that, if they are defined, they will be templated for all resources created by the chart.
    
Previously, they were only templated onto the `Deployment` resource.

By [@garypen](https://github.com/garypen) and [@bjoernw](https://github.com/bjoernw) in https://github.com/apollographql/router/pull/3622
