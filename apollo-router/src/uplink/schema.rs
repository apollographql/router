/// Represents the new state of a schema after an update.
pub struct SchemaState {
    pub sdl: String,
    pub launch_id: Option<String>,
}
