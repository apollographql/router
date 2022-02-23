fn main() {
    let proto_files = vec!["proto/agents.proto", "proto/reports.proto"];

    tonic_build::configure()
        .build_server(true)
        .compile(&proto_files, &["."])
        .unwrap_or_else(|e| panic!("protobuf compile error: {}", e));

    for file in proto_files {
        println!("cargo:rerun-if-changed={}", file);
    }
}
