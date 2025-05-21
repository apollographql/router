# Hyper 1.0 upgrade decisions

Document useful information for posterity.

## Additional Crates

The hyper ecosystem has split functionality into multiple crates. Some
functionality has migrated to new crates (-util).

axum = { version = "0.6.20", features = ["headers", "json", "original-uri"] } -> { version = "0.7.9", features = ["json", "original-uri"] }
axum-extra = NEW -> { version = "0.9.6", features = [ "typed-header" ] }
Note: Not sure if I need to enabled typed-header, check this later

http = "0.2.11" -> "1.1.0"
http-body = "0.4.6" -> "1.0.1"
http-body-util = NEW "0.1.2"

hyper = { version = "0.14.28", features = ["server", "client", "stream"] } -> hyper = { version = "1.5.1", features = ["full"] }
hyper-util = NEW { version = "0.1.10", features = ["full"] }

## Type Changes

A lot of types are changing. It's not always a 1:1 change, because the new
versions of hyper/axum offer much more nuance. I've tried to apply the
following changes consistently.

### hyper::Body

This is no longer a struct, but a trait with multiple implementations depending
on the use case. I've applied the following principles.

#### Clearly driven from Axum

In this case, I'm just using the `axum::body::Body` type as a direct
replacement for `hyper::Body`. I'm assuming that the axum folks know what
they are doing.

#### Otherwise

My default choice is `http_body_util::combinators::UnsyncBoxBody`

This is chosen because it is a trait object which represents any of the many
structs which implement `hyper::body`. Unsync because the future Streams
which we use in the router are only Send, not Sync.

From an `UnsyncBoxBody` we can easily convert to and from various useful
stream representations.

### hyper::Error

In 0.14 this struct was a good choice, however we found it difficult to work
with as we started to connect futures streams back to axum responses.

We have replaced our use of `hyper::Error` with `axum::Error`.

### hyper::server::conn::Http -> hyper_util::server::conn::auto::Builder;

This is a straightforward drop-in replacement because the Http server
has been moved out of the `hyper` crate into `hyper_util` and renamed.

### hyper::body::HttpBody -> http_body::Body as HttpBody;

`HttpBody` no longer exists in `hyper`. This could be replaced either by
`http_body::Body` or `axum::body::HttpBody`. The latter is a re-export of the
former.

I've gone with the former for now, since it is clearly a dependency on
`http_body` rather than a dependency on `axum`.

### hyper::client::connect::dns::Name -> hyper_util::client::legacy::connect::dns::Name;
This is a straightforward drop-in replacement because the Name struct
has been moved out of the `hyper` crate into `hyper_util`.

### hyper::client::HttpConnector -> use hyper_util::client::legacy::connect::HttpConnector;
This is a straightforward drop-in replacement because the HttpConnector struct
has been moved out of the `hyper` crate into `hyper_util`.

### http_body::Full -> axum::body::Full

This is no longer required in hyper 1.0 conversion.

### use axum::headers::HeaderName -> use axum_extra::headers::HeaderName

This is a straightforward drop-in replacement because the ::headers module
has been moved out of the `axum` crate into `axum-extra`.

Note: Not sure if axum-extra TypedHeader feature needs to be enabled for
this to continue working. Enabled for now.

### use axum::body::boxed;

This function appears to be completely removed and no longer required.
Just delete it from the code base.

### use axum::body::StreamBody -> use http_body_util::StreamBody;

This type has been moved to the http_body_util crate.

### hyper::body::to_bytes(body) -> axum::body::to_bytes(body)

Drop in replacement as functionality migrated from hyper to axum
Note: There may be a better way to do this in hyper 1.0, leave as this
for now.

### hyper::Body::from(encoded) -> http_body_util::BodyStream::from(encoded)

`Body` is now a trait, so I *think* this needs to be converted to become a
`BodyStream`.  It may be that it should be a `Full`, check later.

### hyper::Body::empty() -> http_body_util::Empty::new()

`Body` is now a trait. `Empty` is an implementation of the trait which is
empty.

### hyper::Client  -> hyper_util::client::legacy::Client

The `Client` has been moved to the `hyper_util` crate.

### axum::Next is no longer generic

Simply remove the generic argument

### transport::Response -> crate::router::Response

The transport module is no longer required, so we can remove it

###   tower::retry::budget::Budget -> use tower::retry::budget::TpsBudget;

Ported to new tower Retry logic.
