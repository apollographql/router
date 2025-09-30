//! HTML report generator for diagnostics plugin
//!
//! Generates self-contained HTML reports with embedded diagnostic data and JavaScript
//! visualizations. Supports two modes:
//!
//! - **Dashboard mode**: Live interactive dashboard with external script loading for API-based data
//! - **Embedded mode**: Complete self-contained report with all data embedded as base64
//!
//! The generator uses a template-based approach with injection points for:
//! - Tailwind CSS styles (embedded at compile time)
//! - JavaScript visualization libraries (flamegraphs, call graphs, heap profiling)
//! - Diagnostic data (system info, router config, supergraph schema, memory dumps)

use base64::Engine;

use crate::plugins::diagnostics::DiagnosticsError;
use crate::plugins::diagnostics::DiagnosticsResult;
use crate::plugins::diagnostics::memory::MemoryDump;

/// Embedded Tailwind CSS
const TAILWIND_CSS: &str = include_str!("resources/styles.css");

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

        // Inject script tags pointing to separate files
        let script_injection = r#"
    <script src="/diagnostics/custom_elements.js"></script>
    <script src="/diagnostics/backtrace-processor.js"></script>
    <script src="/diagnostics/viz-js-integration.js"></script>
    <script src="/diagnostics/flamegraph-renderer.js"></script>
    <script src="/diagnostics/callgraph-svg-renderer.js"></script>
    <script src="/diagnostics/data-access.js"></script>
    <script src="/diagnostics/main.js"></script>"#;

        // Inject dashboard mode configuration
        let data_injection = r#"
    <script>
        const IS_DASHBOARD_MODE = true;
        const EMBEDDED_DATA = null;
    </script>"#;

        // Inject styles
        let styles_injection = format!("<style>{}</style>", TAILWIND_CSS);
        html = html.replace("<!-- STYLES_INJECTION_POINT -->", &styles_injection);

        html = html.replace("<!-- SCRIPT_INJECTION_POINT -->", script_injection);
        html = html.replace("<!-- DATA_INJECTION_POINT -->", data_injection);

        Ok(html)
    }

     /// Generate a complete HTML report with embedded data (for embedded mode)
    pub(crate) fn generate_embedded_html(&self, data: ReportData<'_>) -> DiagnosticsResult<String> {
        let mut html = self.template.clone();

        // Build embedded script injection
        let script_injection = self.build_embedded_scripts()?;

        // Build data injection
        let data_injection = self.build_data_injection(data)?;

        // Inject styles
        let styles_injection = format!("<style>{}</style>", TAILWIND_CSS);
        html = html.replace("<!-- STYLES_INJECTION_POINT -->", &styles_injection);

        // Perform injections
        html = html.replace("<!-- SCRIPT_INJECTION_POINT -->", &script_injection);
        html = html.replace("<!-- DATA_INJECTION_POINT -->", &data_injection);

        Ok(html)
    }

    /// Build embedded script tags with inline JavaScript content
    fn build_embedded_scripts(&self) -> DiagnosticsResult<String> {
        let js_files = [
            include_str!("resources/custom_elements.js"),
            include_str!("resources/backtrace-processor.js"),
            include_str!("resources/viz-js-integration.js"),
            include_str!("resources/flamegraph-renderer.js"),
            include_str!("resources/callgraph-svg-renderer.js"),
            include_str!("resources/data-access.js"),
            include_str!("resources/main.js"),
        ];

        let mut scripts = String::new();
        for content in js_files.iter() {
            scripts.push_str("\n    <script>\n");
            scripts.push_str(content);
            scripts.push_str("\n    </script>");
        }

        Ok(scripts)
    }

    /// Build data injection script with embedded data
    fn build_data_injection(&self, data: ReportData<'_>) -> DiagnosticsResult<String> {
        // Encode data as base64
        let system_info = data
            .system_info
            .map(|s| base64::engine::general_purpose::STANDARD.encode(s))
            .unwrap_or_default();

        let router_config = data
            .router_config
            .map(|s| base64::engine::general_purpose::STANDARD.encode(s))
            .unwrap_or_default();

        let schema = data
            .supergraph_schema
            .map(|s| base64::engine::general_purpose::STANDARD.encode(s))
            .unwrap_or_default();

        // Serialize memory dumps
        let memory_dumps_json = serde_json::to_string(data.memory_dumps).map_err(|e| {
            DiagnosticsError::Internal(format!("Failed to serialize memory dumps: {}", e))
        })?;

        // Build the injection script
        let injection = format!(
            r#"
    <script>
        const IS_DASHBOARD_MODE = false;
        const EMBEDDED_DATA = {{
            systemInfo: '{}',
            routerConfig: '{}',
            schema: '{}',
            memoryDumps: {}
        }};
    </script>"#,
            system_info, router_config, schema, memory_dumps_json
        );

        Ok(injection)
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
        assert!(
            generator
                .template
                .contains("<!-- SCRIPT_INJECTION_POINT -->")
        );
        assert!(generator.template.contains("<!-- DATA_INJECTION_POINT -->"));
    }

    #[tokio::test]
    async fn test_process_empty_memory_directory() {
        let temp_dir = tempdir().unwrap();

        let result = memory::load_memory_dumps(temp_dir.path()).await;
        assert!(result.is_ok());

        let dumps = result.unwrap();
        assert!(dumps.is_empty());
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
        let html = generator.generate_embedded_html(report_data);

        assert!(html.is_ok());
        let html_content = html.unwrap();

        // Verify injection points were replaced
        assert!(!html_content.contains("<!-- SCRIPT_INJECTION_POINT -->"));
        assert!(!html_content.contains("<!-- DATA_INJECTION_POINT -->"));

        // Verify it contains base64 encoded data
        assert!(
            html_content
                .contains(&base64::engine::general_purpose::STANDARD.encode("System info content"))
        );

        // Verify embedded data structure
        assert!(html_content.contains("const IS_DASHBOARD_MODE = false"));
        assert!(html_content.contains("const EMBEDDED_DATA = {"));

        // Verify Tailwind CSS is embedded
        assert!(
            html_content.contains("<style>") && html_content.contains("tailwindcss"),
            "Tailwind CSS should be embedded in <style> tag"
        );
        assert!(
            !html_content.contains("<!-- STYLES_INJECTION_POINT -->"),
            "STYLES_INJECTION_POINT should be replaced"
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
                timestamp: Some(1704110400), // 2024-01-01 12:00:00 UTC
            }
        ];

        // Generate HTML report
        let report_data = ReportData::new(
            Some(mock_system_info),
            Some(mock_router_config),
            Some(mock_schema),
            &mock_memory_dumps,
        );
        let html_content = generator.generate_embedded_html(report_data).unwrap();

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

        // Verify injection points were replaced
        assert!(
            !html_content.contains("<!-- SCRIPT_INJECTION_POINT -->"),
            "SCRIPT_INJECTION_POINT should be replaced"
        );
        assert!(
            !html_content.contains("<!-- DATA_INJECTION_POINT -->"),
            "DATA_INJECTION_POINT should be replaced"
        );

        // Verify mode and data structure
        assert!(html_content.contains("const IS_DASHBOARD_MODE = false"));
        assert!(html_content.contains("const EMBEDDED_DATA = {"));

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

        // Verify Tailwind CSS is embedded
        assert!(
            html_content.contains("<style>") && html_content.contains("tailwindcss"),
            "Tailwind CSS should be embedded in <style> tag"
        );
        assert!(
            !html_content.contains("<!-- STYLES_INJECTION_POINT -->"),
            "STYLES_INJECTION_POINT should be replaced"
        );
        assert!(
            !html_content.contains("cdn.tailwindcss.com"),
            "Should not contain Tailwind CDN reference"
        );
    }
}
