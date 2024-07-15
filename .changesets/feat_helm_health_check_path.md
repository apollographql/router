### Provide proper support in helm for when router's health_check's default path is being used([Issue #5652](https://github.com/apollographql/router/issues/5652))

When helm chart is defining the liveness and readiness check probes, if the router has been configured to use a non-default healtcheck path, use that configured health_check path rather than the default /health path.

By [Jon Christiansen](https://github.com/theJC) in https://github.com/apollographql/router/pull/5653