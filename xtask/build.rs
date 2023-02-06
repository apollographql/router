use std::fs::File;
use std::io::prelude::*;

const GITHUB_API_SCHEMA_SOURCE: &str = "https://docs.github.com/public/schema.docs.graphql";
const GITHUB_API_SCHEMA_DESTINATION: &str = "src/commands/changeset/github_api_schema.graphql";

fn main() {
    println!("cargo:rerun-if-changed={GITHUB_API_SCHEMA_DESTINATION}");
    if !std::fs::metadata(GITHUB_API_SCHEMA_DESTINATION).is_ok() {
        println!("downloading github graphql api schema.");

        tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(async { download_api_schema().await })
    }
}

async fn download_api_schema() {
    let resp = reqwest::get(GITHUB_API_SCHEMA_SOURCE)
        .await
        .expect("couldn't download schema");
    let mut buffer = File::create(GITHUB_API_SCHEMA_DESTINATION)
        .expect(format!("couldn't create {GITHUB_API_SCHEMA_DESTINATION}").as_str());

    buffer
        .write_all(&resp.bytes().await.unwrap())
        .expect(format!("couldn't save {GITHUB_API_SCHEMA_DESTINATION}").as_str());
}
