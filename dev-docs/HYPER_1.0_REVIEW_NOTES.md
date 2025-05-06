# Hyper 1.0 Review Notes

## Generally Useful Information

Read HYPER_1.0_UPDATE.md first. This provides a lot of generally
useful information.

### Crate updates

Many crates have been updated as part of the update. In some parts of
codebase we had to continue using the older version of the crate so
that opentelemetry (which has not been updated to by hyper 1.0
compliant) would continue to work.

tonic-0_9 = { version = "0.9.0", features = [
reqwest-0_11 = { version = "0.11.27", default-features = false, features = [
http-0_2 = { version = "0.2.12", package = "http" }

When opentelemetry is updated to use hyper 1.0 we will remove these changes.

### Body Manipulation

The change in Hyper to have many different types of bodies implementing the
Body trait means it was useful to have a set of useful body manipulation
functions which are collected in apollo-router/src/services/router/body.rs.

Being familiar with these in the review will be helpful as they are used in
many locations.

### hyper_header_limits feature

We removed this since it's not required in hyper 1.0

### XXX Comments

Anywhere you see a XXX comment is an indication that this should be reviewed
carefully.

## Focussed Review

Please pay particular attention to these files, since they proved tricky
to update:

### apollo-router/src/axum_factory/axum_http_server_factory.rs

Some of the configuration is centralised on `next` as http_config and passed
to serve_router_on_listen_addr. We can't do that following the hyper update
because there are now different builders for Http1 or Http2 configuration.

The HandleErrorLayer has been removed at line 443 and the comment there
explains the change. Anyone with more specific knowledge about how
decompression works should review this carefully.

metrics_handler/license_handler no longer need to be generic.

Changes in Axum routing mean that handle_graphql is cleaner to write as a
generic function.

### apollo-router/src/axum_factory/listeners.rs

Some of the most complex changes with respect to TCP stream handling were
encountered here. Note that `TokioIo` and `hyper::service::service_fn` were
used to "wrap" Axum application and service handler to integrate everything
together. Please familiarise yourselves with how these work so that you
can review the changes in this file.

There is an unresolved problem in the port with graceful shutdown which we
still need to figure out. I believe it is the cause of one of our jaeger
tests which are failing.

The primary additional changes here are releating to how hyper services
are configured, built and served.

### apollo-router/src/axum_factory/tests.rs

`UnixStream` was provided as a helpful wrapper around `tokio::net::UnixStream`
to simplify integration with existing tests.:wq

### apollo-router/src/plugins/connectors/make_requests.rs

In order to be able to compare snapshots we hae to `map` our requests
into a tuple where the request has a converted body.

We can't preserve the existing request becase the type of the body (RouterBody)
would't match. This means we can still snapshot across body contents.

### apollo-router/src/plugins/coprocessor/mod.rs

We replace the RouterBodyConverter type with a MapResponse.

### apollo-router/src/plugins/limits/limited.rs

We remove `poll_trailers` sincethe router doesn't do anything meaninfgul with
trailers (and neither did this implementation)

The `poll_data` is replaced with `poll_frame` to utilise our new stream
conversion functionality.

### apollo-router/src/plugins/telemetry/config_new/connector/selectors.rs

In tests we replaced bodies of "" with empty bodies. That seems fine, but
more informed opinions are sought here. We've done that in a few other files
as well and the tests are also all passing.

### apollo-router/src/plugins/traffic_shaping/retry.rs

I'm not sure why all of the tests from line 91 were deleted. Anyone have any
ideas?

### apollo-router/src/services/http/tests.rs

These tests were particularly tricky to convert, so please examine them
carefully for any issues. Especially with regard to TLS.


