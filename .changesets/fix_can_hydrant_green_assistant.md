### Fix CPU Count for cgroup environments

This fixes an issue where the fleet_detector plugin would not infer correctly the CPU limits for a system used cgroup2 or cgroup.

By [@nmoutschen](https://github.com/nmoutschen) in https://github.com/apollographql/router/pull/6787