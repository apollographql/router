#[cfg(not(windows))]
fn main() {
    use std::collections::HashMap;
    use std::fs::File;
    use std::io::Read;
    use std::io::Write;

    use launchpad::blocking::GraphQLClient;
    use launchpad::introspect::GraphIntrospectInput;
    use launchpad::introspect::{self};

    if let Ok("debug") = std::env::var("PROFILE").as_deref() {
        let client = GraphQLClient::new(
            "https://uplink.api.apollographql.com/",
            reqwest::blocking::Client::new(),
        )
        .unwrap();

        let introspection_response = introspect::run(
            GraphIntrospectInput {
                headers: HashMap::new(),
            },
            &client,
        )
        .unwrap();

        let data = introspection_response.schema_sdl;

        match File::open("uplink.graphql") {
            Err(_) => File::create("uplink.graphql")
                .expect("could not create uplink.graphql file")
                .write_all(data.as_bytes())
                .expect("could not write downloaded uplink schema"),
            Ok(mut file) => {
                let mut buf = Vec::new();
                file.read_to_end(&mut buf).unwrap();

                assert_eq!(
                    std::str::from_utf8(&buf).unwrap(),
                    data.as_str(),
                    "Uplink schema changed"
                );
            }
        }
    }
}

// the uplink schema check will fail on Windows due to different line endings
// this is already tested on other platforms and in CI
#[cfg(windows)]
fn main() {}
