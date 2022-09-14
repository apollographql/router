#[cfg(not(windows))]
pub fn main() {
    use std::collections::HashMap;
    use std::fs::File;
    use std::io::Read;
    use std::io::Write;
    use std::path::PathBuf;

    use introspector_gadget::blocking::GraphQLClient;
    use introspector_gadget::introspect;
    use introspector_gadget::introspect::GraphIntrospectInput;

    if let Ok("debug") = std::env::var("PROFILE").as_deref() {
        let client = GraphQLClient::new(
            "https://uplink.api.apollographql.com/",
            reqwest::blocking::Client::new(),
        )
        .unwrap();

        let should_retry = true;
        let introspection_response = introspect::run(
            GraphIntrospectInput {
                headers: HashMap::new(),
            },
            &client,
            should_retry,
        )
        .unwrap();

        let data = introspection_response.schema_sdl;

        let path = PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").unwrap())
            .join("src")
            .join("uplink")
            .join("uplink.graphql");
        match File::open(&path) {
            Err(_) => File::create(path)
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
pub fn main() {}
