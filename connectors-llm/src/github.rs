use base64::{Engine as _, engine::general_purpose};
use reqwest::blocking::Client;
use serde_json::Value;
use std::env;

pub fn fetch_github_file_content(
    client: &Client,
    owner: &str,
    repo: &str,
    path: &str,
) -> Result<String, Box<dyn std::error::Error>> {
    let github_token = env::var("GITHUB_TOKEN")
        .map_err(|_| "GITHUB_TOKEN environment variable not found. Please set it in .env file.")?;

    let url = format!(
        "https://api.github.com/repos/{}/{}/contents/{}",
        owner, repo, path
    );

    let response = client
        .get(&url)
        .header("Authorization", format!("token {}", github_token))
        .header("User-Agent", "connectors-llm-tool")
        .send()?;

    if !response.status().is_success() {
        return Err(format!(
            "GitHub API request failed with status: {}",
            response.status()
        )
        .into());
    }

    let json: Value = response.json()?;

    // Extract base64 content
    let base64_content = json["content"]
        .as_str()
        .ok_or("Content field not found in GitHub API response")?
        .replace('\n', ""); // Remove newlines from base64

    // Decode base64 to get actual file content
    let decoded_bytes = general_purpose::STANDARD.decode(base64_content)?;
    let content = String::from_utf8(decoded_bytes)?;

    Ok(content)
}
