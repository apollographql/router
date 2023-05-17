use clap::Parser;

#[derive(Debug, Parser)]
#[command(author, version, about, long_about = None)]
pub struct Cli {
    /// Schema file
    pub schema: String,

    /// Query file
    pub query: Option<String>,
}
