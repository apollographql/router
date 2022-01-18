use usage_agent::server::ReportServer;

//TBD: Make the server address configurable

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt::fmt().json().init();
    let report_server = ReportServer::new("0.0.0.0:50051".parse()?);
    report_server.serve().await?;

    Ok(())
}
