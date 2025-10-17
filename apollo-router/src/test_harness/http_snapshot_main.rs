use apollo_router::snapshot_server;

#[tokio::main]
async fn main() {
    snapshot_server().await
}
