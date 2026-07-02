//! `wit-to-gql` — generate a GraphQL SDL from a WebAssembly component's exported functions.
//!
//! Thin CLI over the `wit-gql` library so the generator and the router's runtime dispatch share one
//! implementation. Usage: `wit-to-gql <component.wasm> [-o OUTPUT]` (writes to stdout if no `-o`).

use std::path::PathBuf;

use anyhow::{Context, Result, bail};

fn main() -> Result<()> {
    let mut args = std::env::args_os().skip(1);
    let mut component: Option<PathBuf> = None;
    let mut output: Option<PathBuf> = None;

    while let Some(arg) = args.next() {
        match arg.to_str() {
            Some("-o") | Some("--output") => {
                let path = args.next().context("-o/--output requires a path")?;
                output = Some(PathBuf::from(path));
            }
            Some("-h") | Some("--help") => {
                println!("usage: wit-to-gql <component.wasm> [-o OUTPUT]");
                return Ok(());
            }
            _ if component.is_none() => component = Some(PathBuf::from(arg)),
            _ => bail!("unexpected argument: {:?}", arg),
        }
    }

    let component = component.context("usage: wit-to-gql <component.wasm> [-o OUTPUT]")?;
    let bytes = std::fs::read(&component)
        .with_context(|| format!("failed to read {}", component.display()))?;
    let (resolve, world) = wit_gql::decode_component(&bytes)
        .with_context(|| format!("failed to decode {}", component.display()))?;
    let source_label = component
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| component.display().to_string());
    let sdl = wit_gql::generate(&resolve, world, &source_label)?;

    match output {
        Some(path) => std::fs::write(&path, sdl)
            .with_context(|| format!("failed to write {}", path.display()))?,
        None => print!("{sdl}"),
    }
    Ok(())
}
