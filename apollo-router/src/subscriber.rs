//! The apollo-router Subscriber.
//!
//! Here are some constraints:
//!  - We'd like to use tower to compose our services
//!  - We'd like to be able to choose between Json or Text logging
//!  - We'd like to use EnvFilter to specify logging parameters
//!  - We'd like our configuration to be dynamic/re-loadable
//!
//! This set of constraints, in the round, act to substantially limit
//! our choices in terms of our Subscriber/Layer options with tracing.
//!
//! 1. Tower, in particular the use of buffer(), spawns threads in a
//! separate Tokio runtime. One consequence of this is that we have to
//! set a single global subscriber in order for the spans to be simply
//! propagated. The alternative is having multiple subscribers which
//! must be tracked within our code and then propagated into spawned
//! threads using the Buffer::pair() mechanism.
//!
//! 2. FmtSubscriber is heavily generic. The only viable mechanism for
//! reloading Layers within Tracing is to use the Reload module. However,
//! this won't accept a BoxedLayer, but requires a Concrete Layer and
//! imposes other restrictions on the implementation. RouterSubscriber
//! acts as the concrete implementation and delegates Json/Text decisions
//! to particular configurations of FmtSubscriber composed with an
//! EnvFilter.
//!
//! 3. With dynamic logging configuration, we need a way to register that
//! change. Originally we used multiple subscribers, but see (1) for the
//! problems associate with that. Now we are using a single, global
//! subscriber which supports a Reload layer. We can't use the Reload
//! layer from the tracing-subscriber crate because it doesn't properly
//! downcast when required by the tracing-opentelemetry crate. We've
//! copied the implementation of Reload from the tracing-subscriber crate
//! and added the appropriate downcast support.
//!
//! There is another alternative which we haven't examined which is using
//! Option<Layer> to enable/disable different Layers based on configuration.
//! That might be a simpler solution than using Reload, but it's not clear
//! how we would control layer enabling at runtime. That may be worth
//! exploring at some point.
//!
//! Summary:
//!  - We chose not to use multiple subscribers to make it simpler to write
//!  plugins and not have to understand why Buffer::pair() is required.
//!  - With a single, generic subscriber, we needed a way to represent that
//!  in the code base, hence RouterSubscriber
//!  - To make reloading work properly, we had to fork the Reload
//!  implementation from tracing-subscriber to add the downcasting support
//!  which makes things work.
//!
//!  Implementation Notes:
//!
//!  In our implemenation of download_raw() in our Reload layer, we rely on
//!  the fact that the tracing-opentelemetry implementation behaves as
//!  follows:
//!   - SpanExt::context() uses a downcast to WithContext which then invokes
//!     a function which we "know" is statically defined in the code and
//!     cannot change at runtime. This makes it "safe" to unsafely return
//!     a pointer and execute through that pointer.
//!  We will need to validate that this remains true as the various moving
//!  parts change (upgrade) over time.
use std::any::TypeId;

use once_cell::sync::OnceCell;
use tracing::span::Attributes;
use tracing::span::Record;
use tracing::subscriber::set_global_default;
use tracing::Event as TracingEvent;
use tracing::Id;
use tracing::Metadata;
use tracing::Subscriber;
use tracing_core::span::Current;
use tracing_core::Interest;
use tracing_core::LevelFilter;
use tracing_subscriber::fmt::format::DefaultFields;
use tracing_subscriber::fmt::format::Format;
use tracing_subscriber::fmt::format::Json;
use tracing_subscriber::fmt::format::JsonFields;
use tracing_subscriber::registry::Data;
use tracing_subscriber::registry::LookupSpan;
use tracing_subscriber::EnvFilter;
use tracing_subscriber::FmtSubscriber;
use tracing_subscriber::Layer;

use crate::reload::Handle;
use crate::reload::Layer as ReloadLayer;
use crate::router::ApolloRouterError;

pub(crate) type BoxedLayer = Box<dyn Layer<RouterSubscriber> + Send + Sync>;

type FmtSubscriberTextEnv = FmtSubscriber<DefaultFields, Format, EnvFilter>;
type FmtSubscriberJsonEnv = FmtSubscriber<JsonFields, Format<Json>, EnvFilter>;

/// Choice of JSON or Text output.
pub enum RouterSubscriber {
    JsonSubscriber(FmtSubscriberJsonEnv),
    TextSubscriber(FmtSubscriberTextEnv),
}

impl Subscriber for RouterSubscriber {
    // Required to make the trait work

    fn clone_span(&self, id: &Id) -> Id {
        match self {
            RouterSubscriber::JsonSubscriber(sub) => sub.clone_span(id),
            RouterSubscriber::TextSubscriber(sub) => sub.clone_span(id),
        }
    }

    fn try_close(&self, id: Id) -> bool {
        match self {
            RouterSubscriber::JsonSubscriber(sub) => sub.try_close(id),
            RouterSubscriber::TextSubscriber(sub) => sub.try_close(id),
        }
    }

    /// The delegated downcasting model is copied from the implementation
    /// of `Subscriber` for `Layered` in the tracing_subscriber crate.
    /// The logic appears to be sound, but be wary of problems here.
    unsafe fn downcast_raw(&self, id: std::any::TypeId) -> Option<*const ()> {
        // If downcasting to `Self`, return a pointer to `self`.
        if id == TypeId::of::<Self>() {
            return Some(self as *const _ as *const ());
        }

        // If not downcasting to `Self`, then check the encapsulated
        // subscribers to see if we can downcast one of them.
        match self {
            RouterSubscriber::JsonSubscriber(sub) => sub.downcast_raw(id),
            RouterSubscriber::TextSubscriber(sub) => sub.downcast_raw(id),
        }
    }

    // May not be required to work, but better safe than sorry

    fn current_span(&self) -> Current {
        match self {
            RouterSubscriber::JsonSubscriber(sub) => sub.current_span(),
            RouterSubscriber::TextSubscriber(sub) => sub.current_span(),
        }
    }

    fn drop_span(&self, id: Id) {
        // Rather than delegate, call try_close() to avoid deprecation
        // complaints
        self.try_close(id);
    }

    fn max_level_hint(&self) -> Option<LevelFilter> {
        match self {
            RouterSubscriber::JsonSubscriber(sub) => sub.max_level_hint(),
            RouterSubscriber::TextSubscriber(sub) => sub.max_level_hint(),
        }
    }

    fn register_callsite(&self, metadata: &'static Metadata<'static>) -> Interest {
        match self {
            RouterSubscriber::JsonSubscriber(sub) => sub.register_callsite(metadata),
            RouterSubscriber::TextSubscriber(sub) => sub.register_callsite(metadata),
        }
    }

    // Required by the trait

    fn enabled(&self, meta: &Metadata<'_>) -> bool {
        match self {
            RouterSubscriber::JsonSubscriber(sub) => sub.enabled(meta),
            RouterSubscriber::TextSubscriber(sub) => sub.enabled(meta),
        }
    }

    fn new_span(&self, attrs: &Attributes<'_>) -> Id {
        match self {
            RouterSubscriber::JsonSubscriber(sub) => sub.new_span(attrs),
            RouterSubscriber::TextSubscriber(sub) => sub.new_span(attrs),
        }
    }

    fn record(&self, span: &Id, values: &Record<'_>) {
        match self {
            RouterSubscriber::JsonSubscriber(sub) => sub.record(span, values),
            RouterSubscriber::TextSubscriber(sub) => sub.record(span, values),
        }
    }

    fn record_follows_from(&self, span: &Id, follows: &Id) {
        match self {
            RouterSubscriber::JsonSubscriber(sub) => sub.record_follows_from(span, follows),
            RouterSubscriber::TextSubscriber(sub) => sub.record_follows_from(span, follows),
        }
    }

    fn event(&self, event: &TracingEvent<'_>) {
        match self {
            RouterSubscriber::JsonSubscriber(sub) => sub.event(event),
            RouterSubscriber::TextSubscriber(sub) => sub.event(event),
        }
    }

    fn enter(&self, id: &Id) {
        match self {
            RouterSubscriber::JsonSubscriber(sub) => sub.enter(id),
            RouterSubscriber::TextSubscriber(sub) => sub.enter(id),
        }
    }

    fn exit(&self, id: &Id) {
        match self {
            RouterSubscriber::JsonSubscriber(sub) => sub.exit(id),
            RouterSubscriber::TextSubscriber(sub) => sub.exit(id),
        }
    }
}

impl<'a> LookupSpan<'a> for RouterSubscriber {
    type Data = Data<'a>;

    fn span_data(&'a self, id: &Id) -> Option<<Self as LookupSpan<'a>>::Data> {
        match self {
            RouterSubscriber::JsonSubscriber(sub) => sub.span_data(id),
            RouterSubscriber::TextSubscriber(sub) => sub.span_data(id),
        }
    }
}

pub(crate) struct BaseLayer;

// We don't actually need our BaseLayer to do anything. It exists as a holder
// for the layers set by the reporting.rs plugin
impl<S> Layer<S> for BaseLayer where S: Subscriber + for<'span> LookupSpan<'span> {}

static RELOAD_HANDLE: OnceCell<Handle<BoxedLayer, RouterSubscriber>> = OnceCell::new();

/// Check if the router reloading global subscriber is set.
pub fn is_global_subscriber_set() -> bool {
    matches!(RELOAD_HANDLE.get(), Some(_))
}

/// Set the router reloading global subscriber.
///
/// The provided subscriber is composed with a reloadable layer so that the default
/// global subscriber is now reloadable.
pub fn set_global_subscriber(subscriber: RouterSubscriber) -> Result<(), ApolloRouterError> {
    RELOAD_HANDLE
        .get_or_try_init(move || {
            // First create a boxed BaseLayer
            let cl: BoxedLayer = Box::new(BaseLayer {});

            // Now create a reloading layer from that
            let (reloading_layer, handle) = ReloadLayer::new(cl);

            // Box up our reloading layer
            let rl: BoxedLayer = Box::new(reloading_layer);

            // Compose that with our subscriber
            let composed = rl.with_subscriber(subscriber);

            // Set our subscriber as the global subscriber
            set_global_default(composed)?;

            // Return our handle to store in OnceCell
            Ok(handle)
        })
        .map_err(ApolloRouterError::SetGlobalSubscriberError)?;
    Ok(())
}

/// Replace the tracing layer.
///
/// Reload the current tracing layer with new_layer.
pub fn replace_layer(new_layer: BoxedLayer) -> Result<(), ApolloRouterError> {
    match RELOAD_HANDLE.get() {
        Some(hdl) => {
            hdl.reload(new_layer)
                .map_err(ApolloRouterError::ReloadTracingLayerError)?;
        }
        None => {
            return Err(ApolloRouterError::NoReloadTracingHandleError);
        }
    }
    Ok(())
}
