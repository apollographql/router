use reqwest::blocking::Client;
use std::fs;

mod github;
mod markdown;

use github::fetch_github_file_content;
use markdown::{extract_code_block_from_markdown, extract_html_table_from_markdown};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Load environment variables from .env file
    dotenv::dotenv().ok();

    let client = Client::new();

    // Fetch and process all content sections
    let methods_content = fetch_and_process_methods(&client)?;
    let variables_content = fetch_and_process_variables(&client)?;
    let grammar_content = fetch_and_process_grammar(&client)?;
    let directives_content = fetch_and_process_directives(&client)?;

    // Process template and save to file
    process_template(
        &methods_content,
        &variables_content,
        &grammar_content,
        &directives_content,
    )?;

    Ok(())
}

fn fetch_and_process_methods(client: &Client) -> Result<String, Box<dyn std::error::Error>> {
    // Fetch and process methods from GitHub API
    let methods_markdown = fetch_github_file_content(
        client,
        "apollographql",
        "docs-rewrite",
        "content/pages/graphos/connectors/mapping/methods.mdx",
    )?;

    // Extract all method types and collect content
    let method_types = vec![
        "String methods",
        "Object methods",
        "Array methods",
        "Other methods",
    ];

    let mut methods_content = String::new();
    for method_type in method_types {
        match extract_html_table_from_markdown(&methods_markdown, method_type) {
            Ok(table) => {
                methods_content.push_str(&format!("## {}\n\n{}\n\n", method_type, table));
            }
            Err(e) => {
                eprintln!("Error extracting {}: {}", method_type, e);
            }
        }
    }

    Ok(methods_content)
}

fn fetch_and_process_variables(client: &Client) -> Result<String, Box<dyn std::error::Error>> {
    // Fetch and process variables from GitHub API
    let variables_markdown = fetch_github_file_content(
        client,
        "apollographql",
        "docs-rewrite",
        "content/pages/graphos/connectors/mapping/variables.mdx",
    )?;

    // Extract available variables table
    let variables_content =
        match extract_html_table_from_markdown(&variables_markdown, "Available variables") {
            Ok(table) => format!("## Available variables\n\n{}\n\n", table),
            Err(e) => {
                eprintln!("Error extracting available variables: {}", e);
                String::new()
            }
        };

    Ok(variables_content)
}

fn fetch_and_process_grammar(client: &Client) -> Result<String, Box<dyn std::error::Error>> {
    // Fetch and process grammar from GitHub API
    let grammar_markdown = fetch_github_file_content(
        client,
        "apollographql",
        "router",
        "apollo-federation/src/connectors/json_selection/README.md",
    )?;

    // Extract grammar code block from markdown
    let grammar_content =
        match extract_code_block_from_markdown(&grammar_markdown, "Formal grammar") {
            Ok(code) => format!("```\n{}\n```", code),
            Err(e) => {
                eprintln!("Error extracting grammar: {}", e);
                String::new()
            }
        };

    Ok(grammar_content)
}

fn fetch_and_process_directives(client: &Client) -> Result<String, Box<dyn std::error::Error>> {
    // Fetch and process directives from GitHub API
    let directives_markdown = fetch_github_file_content(
        client,
        "apollographql",
        "docs-rewrite",
        "content/pages/graphos/connectors/reference/directives.mdx",
    )?;

    // Extract directives code block from markdown
    let directives_content =
        match extract_code_block_from_markdown(&directives_markdown, "Connector specification") {
            Ok(code) => format!("```graphql\n{}\n```", code),
            Err(e) => {
                eprintln!("Error extracting directives: {}", e);
                String::new()
            }
        };

    Ok(directives_content)
}

fn process_template(
    methods_content: &str,
    variables_content: &str,
    grammar_content: &str,
    directives_content: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    // Read the template file
    let template_content = fs::read_to_string("src/template.md")?;

    // Replace placeholders with scraped content
    let processed_content = template_content
        .replace("{{ methods }}", methods_content.trim())
        .replace("{{ variables }}", variables_content.trim())
        .replace("{{ grammar }}", grammar_content.trim())
        .replace("{{ directives }}", directives_content.trim());

    // Write to the output file
    fs::write("connector-llm.md", processed_content)?;

    println!("Successfully generated connector-llm.md");
    Ok(())
}
