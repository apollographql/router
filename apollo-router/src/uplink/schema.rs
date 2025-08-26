use std::convert::Infallible;
use std::str::FromStr;

/// Represents the new state of a schema after an update.
#[derive(Eq, PartialEq, Debug, Clone)]
pub(crate) struct SchemaState {
    pub(crate) sdl: String,
    pub(crate) launch_id: Option<String>,
}

impl FromStr for SchemaState {
    type Err = Infallible;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(Self {
            sdl: s.to_string(),
            launch_id: None,
        })
    }
}
