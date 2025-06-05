// Internally mutable and cheap to clone

#[derive(Clone)]
pub struct Context {
    query_planning: HashableAnyMap,
    general: AnyMap,
}
