//! Errors raised by the router's own Rhai bindings, as opposed to errors a script raised
//! deliberately with `throw`.

use rhai::Dynamic;
use rhai::Engine;
use rhai::EvalAltResult;
use rhai::Position;

/// The name scripts see for [`RouterInternalError`], in `type_of()` and in Rhai's own error text.
const SCRIPT_TYPE_NAME: &str = "RouterError";

/// The value carried by a failure inside the router's own Rhai bindings.
///
/// Rhai gives us no way to tell such a failure from a `throw` in the script - both arrive as
/// [`EvalAltResult::ErrorRuntime`] - but only the latter is a message the script author chose to
/// show to clients. `process_error` looks for this type so that binding failures are logged
/// server-side and replaced with a generic message in the client-facing response, instead of
/// disclosing router internals.
///
/// It is an opaque Rust type rather than a key in an object map so that a script can neither strip
/// the marker nor carry it into an error of its own by mutating and re-throwing the value it
/// caught: a script that wants its own status and message throws a fresh value.
#[derive(Clone)]
struct RouterInternalError {
    message: String,
}

/// Build the Rhai error to return from a failure inside the router's own Rhai bindings.
///
/// Every binding must raise its errors through this helper rather than converting a string into
/// `Box<EvalAltResult>` directly, otherwise the error text ends up in client-facing responses.
/// `tests::bindings_raise_their_errors_through_internal_error` enforces that.
///
/// Scripts can still `catch` these errors. The caught value stringifies to the message, so
/// `${err}` in an interpolated string reads as it always has, and exposes it as `err.message`.
pub(super) fn internal_error(message: impl std::fmt::Display) -> Box<EvalAltResult> {
    Box::new(EvalAltResult::ErrorRuntime(
        Dynamic::from(RouterInternalError {
            message: message.to_string(),
        }),
        Position::NONE,
    ))
}

/// The unredacted message of a binding failure, or `None` if `thrown` is something the script threw
/// itself.
///
/// The message names router internals - which binding failed and why - so it belongs in the logs
/// and in a script's `catch` block, never in a client-facing response.
pub(crate) fn internal_error_message(thrown: &Dynamic) -> Option<String> {
    thrown
        .read_lock::<RouterInternalError>()
        .map(|error| error.message.clone())
}

/// Give scripts the same access to a binding failure they had when it was a plain string: `${err}`
/// and `print(err)` yield the message, and so does `err.message`.
pub(super) fn register(engine: &mut Engine) {
    engine
        .register_type_with_name::<RouterInternalError>(SCRIPT_TYPE_NAME)
        .register_get("message", |error: &mut RouterInternalError| {
            error.message.clone()
        })
        .register_fn("to_string", |error: &mut RouterInternalError| {
            error.message.clone()
        })
        .register_fn("to_debug", |error: &mut RouterInternalError| {
            error.message.clone()
        });
}
