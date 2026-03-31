### Enable the router to pull graph artifacts from insecure (non-SSL) registries

You can configure a list of safe registry hostnames to enable the router to pull graph artifacts over HTTP instead of HTTPS. Insecure registries are commonly run within a private network such as a Kubernetes cluster or as a pull-through cache, where you want to avoid the overhead of setting up and distributing SSL certificates.

By [@sirddoger](https://github.com/sirdodger) in https://github.com/apollographql/router/pull/8919
