### Add documentation for adding proxy root certificates to router containers ([PR #8823](https://github.com/apollographql/router/pull/8823))

Adds documentation explaining how to add corporate proxy root certificates to Apollo Router containers. This is necessary for enterprise environments where TLS inspection proxies intercept HTTPS traffic.

The new documentation includes:
- Instructions for Docker deployments (runtime mount and custom image approaches)
- Instructions for Kubernetes deployments (init container and custom image approaches)
- Guidance for cloud deployments (AWS, Azure, GCP)
- Links added to all containerization deployment guides

By [@the-gigi-apollo](https://github.com/the-gigi-apollo) in https://github.com/apollographql/router/pull/8823
