// Search for an operation name, if we find it then
// create a header with it.
fn router_service_request(context) {
    // Find our operation name
    let op_name = get_operation_name(context);
    // If we have one, set our header
    if op_name.len() != 0 {
        context.headers["X-operation-name"] = op_name;
    }
    // Always return the context 
    context
}
