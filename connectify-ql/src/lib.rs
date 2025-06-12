pub mod codegen;
mod lexer;
pub mod linking;
pub mod parser;
pub mod sources;

pub use parser::{ParsedAol, aol_parse};
pub use sources::Sources;
