use base64::Engine;

use crate::plugins::diagnostics::DiagnosticsError;
use crate::plugins::diagnostics::DiagnosticsResult;
use crate::plugins::diagnostics::memory::MemoryDump;

/// Parameters for generating diagnostic reports
#[derive(Debug, Default)]
pub(crate) struct ReportData<'a> {
    /// System information content
    pub system_info: Option<&'a str>,
    /// Router configuration content
    pub router_config: Option<&'a str>,
    /// Supergraph schema content
    pub supergraph_schema: Option<&'a str>,
    /// Memory dump data
    pub memory_dumps: &'a [MemoryDump],
}

impl<'a> ReportData<'a> {
    /// Create a new ReportData with all fields
    pub(super) fn new(
        system_info: Option<&'a str>,
        router_config: Option<&'a str>,
        supergraph_schema: Option<&'a str>,
        memory_dumps: &'a [MemoryDump],
    ) -> Self {
        Self {
            system_info,
            router_config,
            supergraph_schema,
            memory_dumps,
        }
    }
}

/// HTML report generator that creates self-contained diagnostic reports
pub(crate) struct HtmlGenerator {
    template: String,
}

impl HtmlGenerator {
    /// Create a new HTML generator by loading the template
    pub(crate) fn new() -> DiagnosticsResult<Self> {
        // Load the HTML template from the resources directory
        let template = include_str!("resources/template.html").to_string();

        Ok(Self { template })
    }

    /// Generate dashboard HTML with separate script resources (for live mode)
    pub(crate) fn generate_dashboard_html(&self) -> DiagnosticsResult<String> {
        let mut html = self.template.clone();

        // Use separate script tags for dashboard mode (not embedded)
        html = self.use_separate_script_tags(html)?;

        // Replace timestamp with current time
        let timestamp = chrono::Utc::now()
            .format("%Y-%m-%d %H:%M:%S UTC")
            .to_string();
        html = html.replace("{{TIMESTAMP}}", &timestamp);

        // Set dashboard mode flag and no embedded data - will be loaded via API
        html = html.replace("{{DASHBOARD_MODE}}", "true");
        html = html.replace("{{SYSTEM_INFO_BASE64}}", "");
        html = html.replace("{{ROUTER_CONFIG_BASE64}}", "");
        html = html.replace("{{SCHEMA_BASE64}}", "");
        html = html.replace("{{MEMORY_DUMPS_JSON}}", "[]");

        Ok(html)
    }

    /// Generate a complete HTML report with embedded data (for export mode)
    pub(crate) fn generate_report(&self, data: ReportData<'_>) -> DiagnosticsResult<String> {
        let mut html = self.template.clone();

        // Embed JavaScript files inline for export mode
        html = self.embed_javascript_files(html)?;

        // Replace timestamp with current time
        let timestamp = chrono::Utc::now()
            .format("%Y-%m-%d %H:%M:%S UTC")
            .to_string();
        html = html.replace("{{TIMESTAMP}}", &timestamp);

        // Set export mode flag (not dashboard mode)
        html = html.replace("{{DASHBOARD_MODE}}", "false");

        // Embed system info
        if let Some(info) = data.system_info {
            let encoded = base64::engine::general_purpose::STANDARD.encode(info);
            html = html.replace("{{SYSTEM_INFO_BASE64}}", &encoded);
        } else {
            html = html.replace("{{SYSTEM_INFO_BASE64}}", "");
        }

        // Embed router config
        if let Some(config) = data.router_config {
            let encoded = base64::engine::general_purpose::STANDARD.encode(config);
            html = html.replace("{{ROUTER_CONFIG_BASE64}}", &encoded);
        } else {
            html = html.replace("{{ROUTER_CONFIG_BASE64}}", "");
        }

        // Embed supergraph schema
        if let Some(schema) = data.supergraph_schema {
            let encoded = base64::engine::general_purpose::STANDARD.encode(schema);
            html = html.replace("{{SCHEMA_BASE64}}", &encoded);
        } else {
            html = html.replace("{{SCHEMA_BASE64}}", "");
        }

        // Process memory dumps
        let memory_dumps_json = serde_json::to_string(data.memory_dumps).map_err(|e| {
            DiagnosticsError::Internal(format!("Failed to serialize memory dumps: {}", e))
        })?;
        html = html.replace("{{MEMORY_DUMPS_JSON}}", &memory_dumps_json);

        Ok(html)
    }

    /// Embed JavaScript files inline, replacing script src links with embedded content
    fn embed_javascript_files(&self, mut html: String) -> DiagnosticsResult<String> {
        // List of JavaScript files to embed (in order they appear in template)
        let js_files = [
            (
                "backtrace-processor.js",
                include_str!("resources/backtrace-processor.js"),
            ),
            (
                "viz-js-integration.js",
                include_str!("resources/viz-js-integration.js"),
            ),
            (
                "flamegraph-renderer.js",
                include_str!("resources/flamegraph-renderer.js"),
            ),
            (
                "callgraph-svg-renderer.js",
                include_str!("resources/callgraph-svg-renderer.js"),
            ),
            ("data-access.js", include_str!("resources/data-access.js")),
            (
                "custom_elements.js",
                include_str!("resources/custom_elements.js"),
            ),
            ("main.js", include_str!("resources/main.js")),
        ];

        // Replace each script src with embedded content
        for (filename, content) in js_files.iter() {
            let script_tag = format!("<script src=\"{}\"></script>", filename);
            let embedded_script = format!("<script>\n{}\n</script>", content);
            html = html.replace(&script_tag, &embedded_script);
        }

        Ok(html)
    }

    /// Use separate script tags (for dashboard mode, not embedded)
    fn use_separate_script_tags(&self, mut html: String) -> DiagnosticsResult<String> {
        // List of JavaScript files that need base path prefixed
        let js_files = [
            "backtrace-processor.js",
            "viz-js-integration.js",
            "flamegraph-renderer.js",
            "callgraph-svg-renderer.js",
            "data-access.js",
            "custom_elements.js",
            "main.js",
        ];

        // Prefix each script src with the diagnostics base path
        for filename in js_files.iter() {
            let original_tag = format!("<script src=\"{}\"></script>", filename);
            let prefixed_tag = format!("<script src=\"/diagnostics/{}\"></script>", filename);
            html = html.replace(&original_tag, &prefixed_tag);
        }

        Ok(html)
    }
}

#[cfg(test)]
mod tests {
    use tempfile::tempdir;

    use super::*;
    use crate::plugins::diagnostics::memory;

    #[tokio::test]
    async fn test_html_generator_creation() {
        let generator = HtmlGenerator::new();
        assert!(generator.is_ok());

        let generator = generator.unwrap();
        assert!(generator.template.contains("{{TIMESTAMP}}"));
        assert!(generator.template.contains("{{MEMORY_DUMPS_JSON}}"));
    }

    #[tokio::test]
    async fn test_process_empty_memory_directory() {
        let temp_dir = tempdir().unwrap();

        let result = memory::load_memory_dumps(temp_dir.path()).await;
        assert!(result.is_ok());

        let dumps = result.unwrap();
        assert!(dumps.is_empty());
    }

    #[test]
    fn test_timestamp_extraction() {
        let timestamp =
            memory::MemoryDump::extract_timestamp_from_filename("router_heap_dump_1704067200.prof");
        assert!(timestamp.is_some());

        let timestamp =
            memory::MemoryDump::extract_timestamp_from_filename("invalid_filename.prof");
        assert!(timestamp.is_none());
    }

    #[tokio::test]
    async fn test_generate_report_basic() {
        let generator = HtmlGenerator::new().unwrap();
        let _temp_dir = tempdir().unwrap();

        let report_data = ReportData::new(
            Some("System info content"),
            Some("Router config content"),
            Some("Schema content"),
            &[], // empty memory dumps
        );
        let html = generator.generate_report(report_data);

        assert!(html.is_ok());
        let html_content = html.unwrap();

        // Verify template placeholders were replaced
        assert!(!html_content.contains("{{TIMESTAMP}}"));
        assert!(!html_content.contains("{{SYSTEM_INFO_BASE64}}"));
        assert!(!html_content.contains("{{ROUTER_CONFIG_BASE64}}"));
        assert!(!html_content.contains("{{SCHEMA_BASE64}}"));
        assert!(!html_content.contains("{{MEMORY_DUMPS_JSON}}"));

        // Verify it contains base64 encoded data
        assert!(
            html_content
                .contains(&base64::engine::general_purpose::STANDARD.encode("System info content"))
        );
    }

    #[tokio::test]
    async fn test_generate_complete_html_report_with_mock_data() {
        // Test that generates a complete HTML report using predictable mock data
        let generator = HtmlGenerator::new().unwrap();

        // Use predictable mock data instead of filesystem fixtures
        let mock_system_info = "SYSTEM INFORMATION\n==================\n\nOperating System: linux (linux)\nArchitecture: x86_64 (amd64)\nTarget Family: unix\nProcess ID: 12345\nRouter Version: 2.5.0\n\nMEMORY INFORMATION\n------------------\nTotal Memory: 16.00 GB (17179869184 bytes)\nAvailable Memory: 8.00 GB (8589934592 bytes)\n\nCPU INFORMATION\n---------------\nPhysical CPU cores: 8\nCPU Model: Test CPU\n\nSYSTEM LOAD\n-----------\nLoad Average (1min): 1.50\nLoad per CPU (1min): 0.19 (18.8% utilization)\n\nRELEVANT ENVIRONMENT VARIABLES\n------------------------------\nNo relevant Apollo environment variables set";

        let mock_router_config = r#"server:
  listen: 127.0.0.1:4000

experimental_diagnostics:
  enabled: true
  listen: 127.0.0.1:8089

telemetry:
  exporters:
    tracing:
      common:
        enabled: true"#;

        let mock_schema = r#"type Query {
  me: User
  topProducts(first: Int = 5): [Product]
}

type User @key(fields: "id") {
  id: ID!
  username: String
  reviews: [Review]
}

type Product @key(fields: "upc") {
  upc: String!
  name: String
  price: Int
  reviews: [Review]
}

type Review @key(fields: "id") {
  id: ID!
  body: String
  author: User @provides(fields: "username")
  product: Product
}"#;

        // Create mock memory dumps data
        let mock_memory_dumps = vec![
            MemoryDump {
                name: "router_heap_dump_1234567890.prof".to_string(),
                data: base64::engine::general_purpose::STANDARD.encode("heap profile: 1024:   8192 [  1024:   8192] @   0x1234 0x5678\n\nMAPPED_LIBRARIES:\n7f0000000000-7f0000001000 r-xp 00000000 08:01 123456 /usr/bin/router\n\n"),
                size: 150,
                timestamp: Some("2024-01-01 12:00:00".to_string()),
            }
        ];

        // Generate HTML report
        let report_data = ReportData::new(
            Some(mock_system_info),
            Some(mock_router_config),
            Some(mock_schema),
            &mock_memory_dumps,
        );
        let html_content = generator.generate_report(report_data).unwrap();

        // Verify HTML structure (basic HTML validity)
        assert!(html_content.starts_with("<!DOCTYPE html>"));
        assert!(html_content.contains("<html"));
        assert!(html_content.contains("</html>"));
        assert!(html_content.contains("<head>"));

        // Verify embedded content is present (base64 encoded)
        let system_info_b64 = base64::engine::general_purpose::STANDARD.encode(mock_system_info);
        assert!(html_content.contains(&system_info_b64));

        let config_b64 = base64::engine::general_purpose::STANDARD.encode(mock_router_config);
        assert!(html_content.contains(&config_b64));

        let schema_b64 = base64::engine::general_purpose::STANDARD.encode(mock_schema);
        assert!(html_content.contains(&schema_b64));

        // Verify JavaScript components are embedded
        assert!(html_content.contains("HeapProfileParser"));
        assert!(html_content.contains("renderCallGraphWithVizJS"));
        assert!(html_content.contains("renderFlameGraph"));

        // Verify tab structure exists
        assert!(html_content.contains("showTab"));
        assert!(html_content.contains("data-tab="));

        // Verify no template placeholders remain unreplaced
        assert!(
            !html_content.contains("{{TIMESTAMP}}"),
            "TIMESTAMP placeholder should be replaced"
        );
        assert!(
            !html_content.contains("{{SYSTEM_INFO_BASE64}}"),
            "SYSTEM_INFO_BASE64 placeholder should be replaced"
        );
        assert!(
            !html_content.contains("{{ROUTER_CONFIG_BASE64}}"),
            "ROUTER_CONFIG_BASE64 placeholder should be replaced"
        );
        assert!(
            !html_content.contains("{{SCHEMA_BASE64}}"),
            "SCHEMA_BASE64 placeholder should be replaced"
        );
        assert!(
            !html_content.contains("{{MEMORY_DUMPS_JSON}}"),
            "MEMORY_DUMPS_JSON placeholder should be replaced"
        );

        // Verify memory dump processing worked with our mock file
        // Should contain JSON structure for memory dumps
        assert!(html_content.contains("\"name\""));
        assert!(html_content.contains("\"data\""));
        assert!(html_content.contains("router_heap_dump_1234567890.prof"));

        // Verify the HTML is substantial (contains all embedded content)
        assert!(
            html_content.len() > 10000,
            "HTML should be substantial with embedded JavaScript and data"
        );
    }
}
