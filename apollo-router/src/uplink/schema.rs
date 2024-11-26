/// Represents the new state of a schema after an update.
#[derive(Eq, PartialEq)]
pub(crate) struct SchemaState {
    pub(crate) sdl: String,
    pub(crate) launch_id: Option<String>,
}
