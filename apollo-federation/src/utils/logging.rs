/// This macro is a wrapper around `tracing::trace!` and should not be confused with our snapshot
/// testing. This primary goal of this macro is to add the necessary context to logging statements
/// so that external tools (like the snapshot log visualizer) can show how various key data
/// structures evolve over the course of planning a query.
///
/// There are two ways of creating a snapshot. The easiest is by passing the macro a indentifier
/// for the value you'd like to take a snapshot of. This will tag the snapshot type with the type
/// name of the value, create data that is JSON string using serde_json, and add the message
/// literal that you pass in. EX:
/// ```
/// snapshot!(dependency_graph, "updated dependency graph");
/// // Generates:
/// // trace!(snapshot = "FetchDependencyGraph", data = "{ .. }", "updated dependency graph");
/// ```
/// If you do not want to serialize the data, you can pass the name tag for the snapshot and data
/// in directly. Note that the data needs to implement the tracing crate's `Value` trait. Ideally,
/// this is a string representation of the data you're snapshotting. EX:
/// ```
/// snapshot!("FetchDependencyGraph", dependency_graph.to_string(), "updated dependency graph");
/// // Generates:
/// // trace!(snapshot = "FetchDependencyGraph", data = dependency_graph.to_string(), "updated dependency graph");
/// ```
macro_rules! snapshot {
    ($value:expr, $msg:literal) => {
        #[cfg(feature = "snapshot_tracing")]
        trace!(
            snapshot = std::any::type_name_of_val(&$value),
            data = serde_json::to_string(&$value).expect(concat!(
                "Could not serialize value for a snapshot with message: ",
                $msg
            )),
            $msg
        );
    };
    ($name:literal, $value:expr, $msg:literal) => {
        #[cfg(feature = "snapshot_tracing")]
        trace!(snapshot = $name, data = $value, $msg);
    };
}

pub(crate) use snapshot;
