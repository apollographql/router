pub fn extract_code_block_from_markdown(
    markdown: &str,
    header_text: &str,
) -> Result<String, Box<dyn std::error::Error>> {
    let lines: Vec<&str> = markdown.lines().collect();
    let mut header_found = false;
    let mut in_code_block = false;
    let mut code_lines = Vec::new();

    for line in lines {
        // Check if we found the header
        if line.to_lowercase().contains(&header_text.to_lowercase()) && line.starts_with('#') {
            header_found = true;
            continue;
        }

        // If we found the header, look for the next code block
        if header_found {
            if line.starts_with("```") {
                if in_code_block {
                    // End of code block - return the collected content
                    return Ok(code_lines.join("\n"));
                } else {
                    // Start of code block
                    in_code_block = true;
                }
            } else if in_code_block {
                code_lines.push(line);
            } else if line.starts_with('#')
                && !line.to_lowercase().contains(&header_text.to_lowercase())
            {
                // Hit another header without finding a code block
                break;
            }
        }
    }

    if !header_found {
        return Err(format!("{} section not found", header_text).into());
    }

    Err(format!("{} code block not found", header_text).into())
}

pub fn extract_html_table_from_markdown(
    markdown: &str,
    header_text: &str,
) -> Result<String, Box<dyn std::error::Error>> {
    let lines: Vec<&str> = markdown.lines().collect();
    let mut header_found = false;
    let mut in_table = false;
    let mut table_content = Vec::new();

    for line in lines {
        // Check if we found the header
        if line.to_lowercase().contains(&header_text.to_lowercase()) && line.starts_with('#') {
            header_found = true;
            continue;
        }

        // If we found the header, look for HTML table
        if header_found {
            if line.trim().contains("<table>") {
                in_table = true;
                continue;
            }

            if in_table {
                if line.trim().contains("</table>") {
                    // End of table - convert to markdown and return
                    return convert_html_table_to_markdown(&table_content.join("\n"));
                } else {
                    table_content.push(line);
                }
            } else if line.starts_with('#') {
                // Hit another header without finding table
                break;
            }
        }
    }

    if !header_found {
        return Err(format!("{} section not found", header_text).into());
    }

    if table_content.is_empty() {
        return Err(format!("{} HTML table not found", header_text).into());
    }

    Err(format!("{} HTML table not properly closed", header_text).into())
}

fn convert_html_table_to_markdown(html_table: &str) -> Result<String, Box<dyn std::error::Error>> {
    let mut markdown = String::new();
    let mut headers = Vec::new();
    let mut rows = Vec::new();
    let mut in_thead = false;
    let mut in_tbody = false;
    let mut current_row = Vec::new();
    let mut in_cell = false;
    let mut cell_content = String::new();

    for line in html_table.lines() {
        let trimmed = line.trim();

        if trimmed.contains("<thead>") {
            in_thead = true;
        } else if trimmed.contains("</thead>") {
            in_thead = false;
        } else if trimmed.contains("<tbody>") {
            in_tbody = true;
        } else if trimmed.contains("</tbody>") {
            in_tbody = false;
        } else if trimmed.contains("<tr>") {
            current_row.clear();
        } else if trimmed.contains("</tr>") {
            if in_thead && !current_row.is_empty() {
                headers = current_row.clone();
            } else if in_tbody && !current_row.is_empty() {
                rows.push(current_row.clone());
            }
            current_row.clear();
        } else if trimmed.starts_with("<th>") && trimmed.ends_with("</th>") {
            // Single-line th cell
            let cell_text = trimmed
                .replace("<th>", "")
                .replace("</th>", "")
                .trim()
                .to_string();
            current_row.push(cell_text);
        } else if trimmed.starts_with("<td>") && trimmed.ends_with("</td>") {
            // Single-line td cell
            let cell_text = trimmed
                .replace("<td>", "")
                .replace("</td>", "")
                .trim()
                .to_string();
            current_row.push(cell_text);
        } else if trimmed.starts_with("<th>") || trimmed.starts_with("<td>") {
            // Multi-line cell start
            in_cell = true;
            cell_content.clear();
            let cell_text = trimmed
                .replace("<th>", "")
                .replace("<td>", "")
                .trim()
                .to_string();
            if !cell_text.is_empty() {
                cell_content.push_str(&cell_text);
            }
        } else if in_cell && (trimmed.ends_with("</th>") || trimmed.ends_with("</td>")) {
            // Multi-line cell end
            let cell_text = trimmed
                .replace("</th>", "")
                .replace("</td>", "")
                .trim()
                .to_string();
            if !cell_text.is_empty() {
                if !cell_content.is_empty() {
                    cell_content.push(' ');
                }
                cell_content.push_str(&cell_text);
            }
            current_row.push(cell_content.trim().to_string());
            in_cell = false;
            cell_content.clear();
        } else if in_cell && !trimmed.is_empty() {
            // Multi-line cell content
            if !cell_content.is_empty() {
                cell_content.push(' ');
            }
            cell_content.push_str(trimmed);
        }
    }

    // Build markdown table
    if !headers.is_empty() {
        // Add header row
        markdown.push_str("| ");
        markdown.push_str(&headers.join(" | "));
        markdown.push_str(" |\n");

        // Add separator
        markdown.push_str("|");
        for _ in &headers {
            markdown.push_str("---------|");
        }
        markdown.push('\n');
    }

    // Add data rows
    for row in rows {
        if !row.is_empty() {
            markdown.push_str("| ");
            markdown.push_str(&row.join(" | "));
            markdown.push_str(" |\n");
        }
    }

    Ok(markdown)
}
