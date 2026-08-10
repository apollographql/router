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
/// Every binding must raise its errors through this helper - both the ones it builds itself, rather
/// than converting a string into `Box<EvalAltResult>` directly, and the ones rhai's own conversions
/// hand it already built, rather than propagating those unchanged - otherwise the error text ends up
/// in client-facing responses. `tests::bindings_raise_their_errors_through_internal_error` enforces
/// that.
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

/// How rhai renders a marked error when it formats the `Dynamic` carrying it with `Display`.
///
/// Rhai's `Display` for a custom value writes [`std::any::type_name`] rather than going through the
/// `to_string` registered below, so this is the token [`display_error_for_log`] and `process_error`
/// substitute the real message back into when they keep the engine's error text for the logs.
pub(crate) fn displayed_as() -> &'static str {
    std::any::type_name::<RouterInternalError>()
}

/// Render a whole Rhai error for the logs, with a marked error's message put back where rhai names
/// its Rust type.
///
/// For a callback failure `process_error` does this itself, because it has a client-facing response
/// to build at the same time. This is for the failures that only ever reach a log.
pub(super) fn display_error_for_log(error: &EvalAltResult) -> String {
    let displayed = error.to_string();
    match error.unwrap_inner() {
        EvalAltResult::ErrorRuntime(thrown, _) => match internal_error_message(thrown) {
            Some(message) => displayed.replace(displayed_as(), &message),
            None => displayed,
        },
        _ => displayed,
    }
}

/// Render a value a script passed to one of the router's logging functions.
///
/// Those take a `Dynamic` and format it with `Display`, which renders a marked error as
/// [`displayed_as`] rather than through the registered `to_string` - so a script logging the error
/// itself, `log_error(err)`, has to be routed past that the way an interpolated one already is.
pub(super) fn display_for_log(value: &Dynamic) -> String {
    internal_error_message(value).unwrap_or_else(|| value.to_string())
}

/// Give scripts the same access to a binding failure they had when it was a plain string: `${err}`,
/// `print(err)` and `log_error(err)` yield the message, and so does `err.message`.
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

#[cfg(test)]
mod tests {
    use super::*;

    fn thrown_value(error: EvalAltResult) -> Dynamic {
        match error {
            EvalAltResult::ErrorRuntime(thrown, _) => thrown,
            other => panic!("expected a runtime error, got: {other}"),
        }
    }

    // The three places a marked error is rendered outside a script: a router logging function,
    // which takes a `Dynamic` and formats it with `Display`; `display_error_for_log`, for the
    // failures that never reach a client; and `process_error`, which substitutes the message back
    // into rhai's own error text. All rest on `Display` naming the Rust type rather than calling
    // the `to_string` registered for scripts.
    #[test]
    fn it_renders_a_marked_error_as_its_message_rather_than_its_type() {
        let thrown = thrown_value(*internal_error("environment variable not found"));

        assert_eq!(thrown.to_string(), displayed_as());
        assert_eq!(
            display_for_log(&thrown),
            "environment variable not found",
            "log_error(err) has to read as the message, not as the marker"
        );
        assert_eq!(
            display_error_for_log(&internal_error("environment variable not found")),
            "Runtime error: environment variable not found",
            "a service callback failure has to read as the message, not as the marker"
        );
    }

    #[test]
    fn it_renders_everything_else_as_it_always_did() {
        let thrown = thrown_value("a message the script threw".into());

        assert_eq!(display_for_log(&thrown), "a message the script threw");
    }
}
