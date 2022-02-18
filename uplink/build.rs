use std::{
    fs::File,
    io::{Read, Write},
    process::Command,
};
use which::which;

#[cfg(not(windows))]
fn main() {
    if let Ok("debug") = std::env::var("PROFILE").as_deref() {
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

        let mut buf = Vec::new();
        let _ = File::open("uplink.graphql")
            .unwrap()
            .read_to_end(&mut buf)
            .unwrap();
        let data = output.stdout;
        assert_eq!(
            std::str::from_utf8(&buf).unwrap(),
            std::str::from_utf8(&data).unwrap(),
            "Uplink schema changed"
        );

        File::create("uplink.graphql")
            .expect("could not createuplink.graphql file")
            .write_all(&data)
            .expect("could not write downloaded uplink schema");
    }
}

// the uplink schema check will fail on Windows due to different line endings
// this is already tested on other platforms and in CI
#[cfg(windows)]
fn main() {}
