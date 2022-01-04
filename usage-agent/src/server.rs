use usage_agent::server::ReportServer;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let report_server = ReportServer::new("0.0.0.0:50051".parse()?);
    report_server.serve().await?;

    Ok(())
}
