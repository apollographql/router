use anyhow::{anyhow, Context, Result};
use camino::Utf8PathBuf;

use std::{convert::TryFrom, fs};

use crate::utils::{self, PKG_PROJECT_ROOT};

pub(crate) struct DocsRunner {
    pub(crate) project_root: Utf8PathBuf,
    pub(crate) docs_root: Utf8PathBuf,
}

impl DocsRunner {
    pub(crate) fn new() -> Result<Self> {
        let project_root = PKG_PROJECT_ROOT.clone();
        let docs_root = project_root.join("docs");
        Ok(Self {
            project_root,
            docs_root,
        })
    }

    pub(crate) fn build_error_code_reference(&self) -> Result<()> {
        utils::info("updating error reference material.");
        let docs_path = &self.docs_root.join("source").join("errors.md");
        let codes_dir = &self
            .project_root
            .join("src")
            .join("error")
            .join("metadata")
            .join("codes");

        // sort code files alphabetically
        let raw_code_files = fs::read_dir(&codes_dir)?;

        let mut code_files = Vec::new();
        for raw_code_file in raw_code_files {
            let raw_code_file = raw_code_file?;
            if raw_code_file.file_type()?.is_dir() {
                return Err(anyhow!("Error code directory {} contains a directory {:?}. It must only contain markdown files.", &codes_dir, raw_code_file.file_name()));
            } else {
                code_files.push(raw_code_file);
            }
        }
        code_files.sort_by_key(|f| f.path());

        let mut all_descriptions = String::new();

        // for each code description, get the name of the code from the filename,
        // and add it as a header. Then push the header and description to the
        // all_descriptions string
        for code in code_files {
            let path = Utf8PathBuf::try_from(code.path())?;

            let contents = fs::read_to_string(&path)?;
            let code_name = path
                .file_name()
                .ok_or_else(|| anyhow!("Path {} doesn't have a file name", &path))?
                .replace(".md", "");

            let description = format!("### {}\n\n{}\n\n", code_name, contents);

            all_descriptions.push_str(&description);
        }

        self.replace_content_after_token("<!-- BUILD_CODES -->", &all_descriptions, docs_path)
    }

    pub(crate) fn copy_contributing(&self) -> Result<()> {
        utils::info("updating contributing.md");

        let source_path = self.project_root.join("CONTRIBUTING.md");
        let destination_path = self.docs_root.join("source").join("contributing.md");

        let source_content_with_header = fs::read_to_string(&source_path)
            .with_context(|| format!("Could not read contents of {} to a String", &source_path))?;
        // Don't include the first header and the empty newline after it.
        let source_content = source_content_with_header
            .splitn(3, '\n')
            .collect::<Vec<&str>>()[2];
        self.replace_content_after_token("<!-- CONTRIBUTING -->", source_content, &destination_path)
    }

    fn replace_content_after_token(
        &self,
        html_comment_token: &str,
        source_content: &str,
        destination_path: &Utf8PathBuf,
    ) -> Result<()> {
        // build up a new docs page with existing content line-by-line
        // and then concat the replacement content
        let destination_content = fs::read_to_string(&destination_path).with_context(|| {
            format!(
                "Could not read contents of {} to a String",
                &destination_path
            )
        })?;
        let mut new_content = String::new();
        for line in destination_content.lines() {
            new_content.push_str(line);
            new_content.push('\n');
            if line.contains(html_comment_token) {
                break;
            }
        }
        new_content.push_str(source_content);

        fs::write(&destination_path, new_content)?;
        Ok(())
    }
}
