use std::fs;
use std::path::PathBuf;

mod studio;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cargo_manifest: serde_json::Value = basic_toml::from_str(
        &fs::read_to_string(PathBuf::from(&env!("CARGO_MANIFEST_DIR")).join("Cargo.toml"))
            .expect("could not read Cargo.toml"),
    )
    .expect("could not parse Cargo.toml");

    let router_bridge = cargo_manifest
        .get("dependencies")
        .expect("Cargo.toml does not contain dependencies")
        .as_object()
        .expect("Cargo.toml dependencies key is not an object")
        .get("router-bridge")
        .expect("Cargo.toml dependencies does not have an entry for router-bridge");
    let router_bridge_version = router_bridge
        .as_str()
        .or_else(|| {
            router_bridge
                .as_object()
                .and_then(|o| o.get("version"))
                .and_then(|version| version.as_str())
        })
        .expect("router-bridge does not have a version");

    let mut it = router_bridge_version.split('+');
    let _ = it.next();
    let fed_version = it.next().expect("invalid router-bridge version format");

    println!("cargo:rustc-env=FEDERATION_VERSION={fed_version}");

    studio::main()
}
