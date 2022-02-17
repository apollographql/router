use std::{fs::File, io::Write, process::Command};
use which::which;

fn main() {
    //~/.rover/bin/rover graph introspect https://uplink.api.apollographql.com/
    which("rover")
    .map_err(|_| "could not find path to rover executable, see installation instructions at https://github.com/apollographql/rover#installation-methods").unwrap();

    let output = Command::new("rover")
        .args([
            "graph",
            "introspect",
            "https://uplink.api.apollographql.com/",
        ])
        .output()
        .expect("failed to execute process");

    let data = output.stdout;
    File::create("uplink.graphql")
        .expect("could not createuplink.graphql file")
        .write_all(&data)
        .expect("could not write downloaded uplink schema");
}
