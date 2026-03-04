### Allow Router to pull graph artifacts from unsecure (non-SSL) registries.

Allow users to configure a list of safe registry hostnames, so Router can pull graph artifacts over HTTP instead of HTTPS. Unsecure registries are commonly run within a private network such as a Kubernetes cluster or as a pull-through cache, where users want to avoid the overhead of setting up and distributing SSL certificates.

By [@sirddoger](https://github.com/sirdodger) in https://github.com/apollographql/router/pull/8919
