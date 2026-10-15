// Fails inside the Rhai engine itself, before any router function is reached. Kept apart from
// rhai_redacted_error.rhai, whose router_service breaks the pipeline before an execution callback
// could ever run, so reaching the execution stage needs a script of its own.
//
// Used by both it_redacts_engine_errors_from_client_responses (whose log snapshot pins the line and
// position below) and client_errors_omit_rhai_engine_internals. Moving a line here means updating
// that snapshot.

fn execution_service(service) {
    service.map_request(Fn("call_undefined_function"));
}

fn call_undefined_function(request) {
    this_function_does_not_exist();
}
