//! License enforcement: halting requests with an expired license, and rate-limited logging of
//! expiry warnings. Applied directly to the router pipeline as a `tower::Layer` (see [`layer`]),
//! not as a plugin.

pub(crate) mod layer;
