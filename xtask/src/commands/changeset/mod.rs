/// This module is generated:
///   1. Only if you actually change the `matching_pull_request.graphql` query.
///   1. By installing `graphql_client_cli`
///
///          cargo install graphql_client_cli
///
///   1. By running:
///      1. A command that downloads the GitHub Schema.  It's large so we don't
///         need to check it in.  Make sure to download it INTO the
///         `src/commands/changeset/` directory.
///
///             wget https://docs.github.com/public/schema.docs.graphql`
///
///      2. Generate against this downloaded schema.  Run this from inside the
///         `src/commands/changeset/` directory.
///
///             graphql-client generate \
///               --schema-path ./schema.docs.graphql \
///               --response-derives='Debug' \
///               --custom-scalars-module='crate::commands::changeset::scalars' \
///               ./matching_pull_request.graphql
///
mod matching_pull_request;
mod scalars;

use std::fmt;
use std::fs;
use std::fs::remove_file;
use std::fs::DirEntry;
use std::path::PathBuf;
use std::str::FromStr;

use ::reqwest::Client;
use anyhow::Result;
use console::style;
use dialoguer::console::Term;
use dialoguer::theme::ColorfulTheme;
use dialoguer::Confirm;
use dialoguer::Editor;
use dialoguer::Input;
use dialoguer::Select;
use itertools::Itertools;
use matching_pull_request::matching_pull_request::ResponseData;
use matching_pull_request::matching_pull_request::Variables;
use matching_pull_request::MatchingPullRequest;
use serde::Serialize;
use tinytemplate::format_unescaped;
use tinytemplate::TinyTemplate;
use xtask::PKG_PROJECT_ROOT;

#[derive(Serialize)]
struct TemplateResource {
    number: String,
    url: String,
}

#[derive(Serialize)]
struct TemplateContext {
    pulls: Vec<TemplateResource>,
    issues: Vec<TemplateResource>,
    title: String,
    body: String,
    author: String,
}

const REPO_WITH_OWNER: &str = "apollographql/router";

const EXAMPLE_TEMPLATE: &str = "### { title }
{{- if issues -}}
  {{- if issues }} {{ endif -}}
  {{- for issue in issues -}}
    ([Issue #{issue.number}]({issue.url}))
    {{- if not @last }}, {{ endif -}}
  {{- endfor -}}
{{ else -}}
  {{- if pulls -}}
    {{- if pulls }} {{ endif -}}
    {{- for pull in pulls -}}
      ([PR #{pull.number}]({pull.url}))
      {{- if not @last }}, {{ endif -}}
    {{- endfor -}}
  {{- else -}}
  {{- endif -}}
{{- endif }}

{body}

By [@{author}](https://github.com/{author}){{ if pulls }} in {{ for pull in pulls -}}
{pull.url}{{ if not @last }}, {{ endif }}
{{- endfor }}{{ endif }}
";

impl Command {
    pub fn run(&self) -> Result<()> {
        match self {
            Command::Create(command) => command.run(),
        }
    }
}

#[derive(Debug, clap::Subcommand)]
pub enum Command {
    /// Add a new changeset
    Create(Create),
}

#[allow(clippy::derive_ord_xor_partial_ord)]
#[derive(Debug, Copy, Clone, Eq, PartialEq, Hash, Ord)]
enum Classification {
    Breaking,
    Feature,
    Fix,
    Configuration,
    Maintenance,
    Documentation,
    Experimental,
}

impl Classification {
    /// These "short names" are the prefixes that are used on the files
    /// themselves and also for the `--class` flag for the CLI.
    fn as_short_name(&self) -> &'static str {
        match self {
            Classification::Breaking => "breaking",
            Classification::Feature => "feat",
            Classification::Fix => "fix",
            Classification::Configuration => "config",
            Classification::Maintenance => "maint",
            Classification::Documentation => "docs",
            Classification::Experimental => "exp",
        }
    }

    /// Defines the ordering that eventually appears in the emitted CHANGELOG
    /// and the order options appear in the TUI.
    const ORDERED_ALL: &'static [Self] = &[
        Classification::Breaking,
        Classification::Feature,
        Classification::Fix,
        Classification::Configuration,
        Classification::Maintenance,
        Classification::Documentation,
        Classification::Experimental,
    ];
}

impl std::cmp::PartialOrd for Classification {
    fn partial_cmp(&self, other: &Classification) -> Option<std::cmp::Ordering> {
        Self::ORDERED_ALL
            .iter()
            .position(|item| item == self)
            .partial_cmp(&Self::ORDERED_ALL.iter().position(|item| item == other))
    }
}

type ParseError = &'static str;
impl FromStr for Classification {
    type Err = ParseError;
    fn from_str(classification: &str) -> Result<Self, Self::Err> {
        if classification.starts_with("break") {
            return Ok(Classification::Breaking);
        }
        if classification.starts_with("feat") {
            return Ok(Classification::Feature);
        }
        if classification.starts_with("fix") {
            return Ok(Classification::Fix);
        }
        if classification.starts_with("config") {
            return Ok(Classification::Configuration);
        }
        if classification.starts_with("maint") {
            return Ok(Classification::Maintenance);
        }
        if classification.starts_with("docs") {
            return Ok(Classification::Documentation);
        }
        if classification.starts_with("exp") {
            return Ok(Classification::Experimental);
        }

        Err("unknown classification")
    }
}

impl fmt::Display for Classification {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        let pretty = match self {
            Classification::Breaking => "❗ BREAKING ❗",
            Classification::Feature => "🚀 Features",
            Classification::Fix => "🐛 Fixes",
            Classification::Configuration => "📃 Configuration",
            Classification::Maintenance => "🛠 Maintenance",
            Classification::Documentation => "📚 Documentation",
            Classification::Experimental => "🧪 Experimental",
        };
        write!(f, "{}", pretty)
    }
}

#[derive(Debug, clap::Parser)]
pub struct Create {
    /// Use the current branch as the file name
    #[clap(short = 'b', long = "with-branch-name")]
    with_branch_name: bool,

    /// The classification of the changeset
    #[clap(short = 'c', long = "class")]
    classification: Option<Classification>,
}

async fn github_graphql_post_request(
    token: &str,
    url: &str,
    request_body: &graphql_client::QueryBody<Variables>,
) -> Result<graphql_client::Response<ResponseData>, ::reqwest::Error> {
    let client = Client::builder().build()?;

    let res = client
        .post(url)
        .header(
            "User-Agent",
            format!("github {} releasing", REPO_WITH_OWNER),
        )
        .header("Authorization", format!("Bearer {}", token))
        .json(request_body)
        .send()
        .await?;
    let response_body: graphql_client::Response<ResponseData> = res.json().await?;
    Ok(response_body)
}

fn get_changesets_dir() -> camino::Utf8PathBuf {
    PKG_PROJECT_ROOT.join(".changesets")
}

impl Create {
    pub fn run(&self) -> Result<()> {
        let changesets_dir_path = get_changesets_dir();
        tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(async {
                let items = Classification::ORDERED_ALL;

                let selected_classification: Classification = if self.classification.is_some() {
                    println!(
                        "{} {} {}",
                        style("Using").yellow(),
                        style(self.classification.unwrap()).cyan(),
                        style("classification from CLI arguments").yellow()
                    );
                    self.classification.unwrap()
                } else {
                    let selection = Select::with_theme(&ColorfulTheme::default())
                        .with_prompt("What is the classification?")
                        .items(items)
                        .interact_on_opt(&Term::stderr())?
                        .expect("no classification was selected");
                    items[selection]
                };

                let gh_cli_path = which::which("gh");
                let use_gh_cli = if gh_cli_path.is_err() {
                    println!("{}", style("If you install and authorize the GitHub CLI, we can use information from the PR!").underlined().on_blue().yellow().bright().bold());
                    println!("  Find more details at: {}", style("https://cli.github.com/").bold());
                    false
                } else if Confirm::new()
                        .default(true)
                        .with_prompt(format!(
                            "{}",
                            style("You have the GitHub CLI installed!  Can we use it to access the API and pre-populate values for the changelog?").yellow(),
                        ))
                        .interact()?
                    {
                      println!("{}", style("Great! That'll make your life easier.").yellow());
                      true
                    } else {
                        println!("Ok! We won't talk to GitHub, so you'll be on your own.");
                        false
                    };

                let use_branch_name = if self.with_branch_name {
                    true
                } else {
                    let selection = Select::with_theme(&ColorfulTheme::default())
                        .default(0)
                        .with_prompt("How do you want to name it?")
                        .items(&["Branch Name", "Random Name"])
                        .interact_on_opt(&Term::stderr())?
                        .expect("no naming convention was selected");

                    // Match if the first index was "Branch Name" (from `items`)
                    selection == 0
                };

                // Get the branch name, optionally, using `git rev-parse --abbrev-ref HEAD`.
                let branch_name: Option<String> = match std::process::Command::new("git")
                    .arg("rev-parse")
                    .arg("--abbrev-ref")
                    .arg("HEAD")
                    .output()
                {
                    Ok(output) => {
                        if output.status.success() {
                            Some(String::from_utf8(output.stdout).unwrap().trim().to_string())
                        } else {
                            None
                        }
                    }
                    Err(e) => panic!("failed to open: {e}"),
                };

                // If the branch name worked out, we'll use that, otherwise, random.
                let initial_text = if use_branch_name && branch_name.is_some() {
                    let branch_regex = regex::Regex::new(r"[^a-z0-9]")?;
                    branch_regex.replace_all(
                        branch_name
                        .clone()
                        .unwrap()
                        .to_lowercase()
                        .as_str(), "_"
                        ).to_string()
                } else {
                    memorable_wordlist::snake_case(48)
                };

                let input: String = Input::new()
                    .with_prompt(format!(
                        "{} {} {}",
                        style("Any edits to the slug for the").yellow(),
                        style(selected_classification.to_string()).cyan(),
                        style("changeset?").yellow(),
                    ))
                    .with_initial_text(initial_text)
                    .interact_text()?;

                let new_changeset_path = changesets_dir_path.join(format!(
                    "{}_{}.md",
                    selected_classification.as_short_name(),
                    input
                ));

                let mut tt = TinyTemplate::new();
                tt.add_template("message", EXAMPLE_TEMPLATE)?;

                let default_context = TemplateContext {
                    title: String::from("Brief but complete sentence that stands on its own"),
                    issues: vec!(TemplateResource {
                        url: format!("https://github.com/{}/issues/ISSUE_NUMBER", REPO_WITH_OWNER),
                        number: String::from("ISSUE_NUMBER"),
                    }),
                    pulls: vec!(TemplateResource {
                        url: format!("https://github.com/{}/pull/PULL_NUMBER", REPO_WITH_OWNER),
                        number: String::from("PULL_NUMBER"),
                    }),
                    author: String::from("AUTHOR"),
                    body: String::from("A description of the fix which stands on its own separate from the title.  It should embrace the use of Markdown to stylize the commentary so it looks great on the GitHub Releases, when shared on social cards, etc."),
                };

                let context: TemplateContext = if use_gh_cli && branch_name.is_some() {
                    match get_token_from_gh_cli(gh_cli_path.unwrap()) {
                        Err(_) => default_context,
                        Ok(gh_token) => {
                            // Good for testing. ;)
                            // let search = format!("repo:{} is:open is:pr head:{}", REPO_WITH_OWNER, "garypen/stricter-jwt-authentication");
                            let search = format!("repo:{} is:open is:pr head:{}", REPO_WITH_OWNER, &branch_name.as_ref().unwrap());
                            let query = <MatchingPullRequest as graphql_client::GraphQLQuery>::build_query(Variables { search });
                            let response = github_graphql_post_request(&gh_token, "https://api.github.com/graphql", &query).await?;

                            // There's only ever one query because the operation only asks for the `first: 1`.
                            let all_prs_info = pr_info_from_response(response.data.expect("no data"));

                            let pr_info_opt = all_prs_info.first();

                            match pr_info_opt {
                                Some(pr_info) => {
                                    let issues= pr_info.closing_issues_references.as_ref().map(|i| {
                                        i.nodes.as_ref().unwrap().iter().map(|j| {
                                            j.as_ref().unwrap()
                                        })
                                    }).unwrap().filter(|p| {
                                        p.repository.name_with_owner == REPO_WITH_OWNER
                                    }).map(|p| {
                                        TemplateResource {
                                            number: p.number.to_string(),
                                            url: p.url.to_string(),
                                        }
                                    });

                                    let pr_body = pr_info.body.clone().replace("\r\n", "\n");

                                    // Remove the trailing part of the checklist from the PR body.
                                    // In the future, we will use the "start metadata" HTML tag, but for now,
                                    // we support both.
                                    let pr_body_trailer_regex = regex::Regex::new(
                                    r"(?ms)(^<!-- start metadata -->\n---$\n)?^\*\*Checklist\*\*$[\s\S]*",
                                    )?;

                                    // Remove all the "Fixes" references, since we're already going to reference
                                    // those in the course of generating the template.
                                    let pr_body_fixes_regex = regex::Regex::new(
                                        r"(?m)^(- )?Fix(es)? #.*$",
                                    )?;

                                    // Run the above Regexes and trim the blurb.
                                    let clean_pr_body = pr_body_fixes_regex
                                        .replace_all(pr_body_trailer_regex
                                        .replace(&pr_body, "")
                                        .trim(), "")
                                        .trim()
                                        .to_string();

                                    TemplateContext {
                                        title: pr_info.title.clone(),
                                        issues: issues.collect_vec(),
                                        pulls: vec!(TemplateResource {
                                            number: pr_info.number.to_string(),
                                            url: pr_info.url.to_string(),
                                        }),
                                        body: clean_pr_body,
                                        author: pr_info.author.as_ref().unwrap().login.to_string(),
                                    }
                                },
                                None => {
                                    // TODO In a follow-up we should figure out how forks work with the GitHub API.
                                    println!(
                                        "{} {} {} {} {}",
                                        style("The changeset will be").magenta(),
                                        style("generic").red().bold(),
                                        style("as we didn't find any PRs on GitHub for").magenta(),
                                        style(&branch_name.as_ref().unwrap()).green(),
                                        style("! (We don't support forks right now.)")
                                    );
                                    default_context
                                }
                            }
                        }
                    }
                } else {
                    default_context
                };

                tt.set_default_formatter(&format_unescaped);
                let rendered_template = tt.render("message", &context).unwrap().replace('\r', "");

                if new_changeset_path.exists() {
                    panic!("The desired changeset name already exists and proceeding would clobber it.  Edit or delete the existing changeset with the same name.");
                }

                fs::write(&new_changeset_path, &rendered_template)?;
                println!(
                    "{} {} {} {}",
                    style("Created new").yellow(),
                    style(selected_classification.to_string()).cyan(),
                    style("changeset named").yellow(),
                    style(&new_changeset_path).cyan(),
                );

                if Confirm::new()
                    .default(true)
                    .with_prompt(format!(
                        "{} {} {} {}?",
                        style("Do you want to open").yellow(),
                        style(&new_changeset_path).cyan(),
                        style("in").yellow(),
                        style("$EDITOR").green(),
                    ))
                    .interact()?
                {
                    if let Some(rv) = Editor::new()
                        .extension(".md")
                        .trim_newlines(true)
                        .edit(&rendered_template)
                        .unwrap()
                    {
                        fs::write(&new_changeset_path, rv)?;
                    } else {
                        println!(
                            "{}",
                            style("Editing was aborted and changes were not saved.")
                                .red()
                                .on_yellow()
                        );
                    }
                }

                println!(
                    "{}",
                    style("Be sure to finalize the changeset, commit it and push it to Git.")
                        .magenta()
                );

                Ok(())
            })
    }
}

fn get_token_from_gh_cli(gh_cli_path: PathBuf) -> Result<String, &'static str> {
    let result = std::process::Command::new(gh_cli_path)
        .args(["auth", "token"])
        .output()
        .expect("this didn't go well");
    if !result.status.success() {
        Err("We couldn't run `gh auth token`.  Perhaps run `gh auth login`.")
    } else {
        let gh_token_with_nl =
            String::from_utf8(result.stdout).expect("should have had newline token");
        let gh_token = gh_token_with_nl.trim().to_string();
        if gh_token.is_empty() {
            Err("Doesn't look like you have a valid token. Run `gh auth login`.")
        } else {
            Ok(gh_token)
        }
    }
}

fn pr_info_from_response(
    response_data: ResponseData,
) -> Vec<matching_pull_request::matching_pull_request::PrInfo> {
    response_data.search.nodes.map(|node| {
        let maybe_prs = node.into_iter().map(|p| {
            p.unwrap()
        });

        maybe_prs.filter_map(|maybe_pr| {
            if let matching_pull_request::matching_pull_request::PrSearchResultNodes::PullRequest(info) = maybe_pr {
                Some(info)
            } else {
                None
            }
        }).collect()
    }).unwrap_or_default()
}

fn get_changeset_files() -> Vec<Changeset> {
    fs::read_dir(get_changesets_dir())
        .unwrap()
        .collect::<std::io::Result<Vec<_>>>()
        .unwrap()
        .iter()
        .filter_map(|file_entry| file_entry.try_into().ok())
        .collect::<Vec<Changeset>>()
}

fn generate_content_from_changeset_files(changelog_entries: &[Changeset]) -> String {
    let mut changelog_entries = changelog_entries.to_owned();
    changelog_entries.sort();

    let mut output: String = String::from("");

    // We'll use this to track the classification, and print it one per change.
    let mut last_kind = None;

    for entry in changelog_entries {
        // For each classification change, print the heading.
        if last_kind.is_none() || Some(entry.classification) != last_kind {
            let new_header = format!("## {}\n\n", entry.classification);
            output += &*new_header;
        }
        last_kind = Some(entry.classification);

        // Add the entry's content to the block of text!
        let entry = format!("{}\n\n", entry.content);
        output += &*entry;
    }
    output
}

fn remove_changeset_files(changesets: &Vec<Changeset>) -> bool {
    let mut failure: bool = false;
    for changeset in changesets {
        if remove_file(&changeset.path).is_ok() {
            println!("Deleted {:?}", changeset.path);
        } else {
            eprintln!("Could not delete {:?}", changeset.path);
            failure = true;
        }
    }
    !failure
}

pub fn slurp_and_remove_changesets() -> String {
    let changesets = get_changeset_files();
    let content = generate_content_from_changeset_files(&changesets);
    remove_changeset_files(&changesets);
    content
}

#[allow(clippy::derive_ord_xor_partial_ord)]
#[derive(Clone, Debug, Eq, Ord)]
struct Changeset {
    classification: Classification,
    content: String,
    path: PathBuf,
}

impl std::cmp::PartialEq for Changeset {
    fn eq(&self, other: &Self) -> bool {
        self.classification == other.classification
    }
}

impl std::cmp::PartialOrd for Changeset {
    fn partial_cmp(&self, other: &Changeset) -> Option<std::cmp::Ordering> {
        self.classification.partial_cmp(&other.classification)
    }
}

impl TryFrom<&DirEntry> for Changeset {
    type Error = String;
    fn try_from(entry: &DirEntry) -> std::result::Result<Self, Self::Error> {
        let path = entry.path();
        let content = fs::read_to_string(&path).unwrap().trim().to_string();
        Ok(Changeset {
            classification: entry
                .file_name()
                .to_string_lossy()
                .parse()
                .map_err(|e: &str| e.to_string())?,
            content,
            path,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn it_templatizes_with_multiple_issues_in_title_and_multiple_prs_in_footer() {
        let test_context = TemplateContext {
            title: String::from("TITLE"),
            issues: vec![
                TemplateResource {
                    url: format!(
                        "https://github.com/{}/issues/ISSUE_NUMBER1",
                        String::from("REPO_WITH_OWNER")
                    ),
                    number: String::from("ISSUE_NUMBER1"),
                },
                TemplateResource {
                    url: format!(
                        "https://github.com/{}/issues/ISSUE_NUMBER2",
                        String::from("REPO_WITH_OWNER")
                    ),
                    number: String::from("ISSUE_NUMBER2"),
                },
            ],
            pulls: vec![
                TemplateResource {
                    url: format!(
                        "https://github.com/{}/pull/PULL_NUMBER1",
                        String::from("REPO_WITH_OWNER")
                    ),
                    number: String::from("PULL_NUMBER1"),
                },
                TemplateResource {
                    url: format!(
                        "https://github.com/{}/pull/PULL_NUMBER2",
                        String::from("REPO_WITH_OWNER")
                    ),
                    number: String::from("PULL_NUMBER2"),
                },
            ],
            author: String::from("AUTHOR"),
            body: String::from("BODY"),
        };

        let mut tt = TinyTemplate::new();
        tt.add_template("message", EXAMPLE_TEMPLATE).unwrap();
        tt.set_default_formatter(&format_unescaped);
        let rendered_template = tt
            .render("message", &test_context)
            .unwrap()
            .replace('\r', "");
        insta::assert_snapshot!(rendered_template);
    }

    #[test]
    fn it_templatizes_with_multiple_prs_in_footer() {
        let test_context = TemplateContext {
            title: String::from("TITLE"),
            issues: vec![TemplateResource {
                url: format!(
                    "https://github.com/{}/issues/ISSUE_NUMBER",
                    String::from("REPO_WITH_OWNER")
                ),
                number: String::from("ISSUE_NUMBER"),
            }],
            pulls: vec![
                TemplateResource {
                    url: format!(
                        "https://github.com/{}/pull/PULL_NUMBER1",
                        String::from("REPO_WITH_OWNER")
                    ),
                    number: String::from("PULL_NUMBER1"),
                },
                TemplateResource {
                    url: format!(
                        "https://github.com/{}/pull/PULL_NUMBER2",
                        String::from("REPO_WITH_OWNER")
                    ),
                    number: String::from("PULL_NUMBER2"),
                },
            ],
            author: String::from("AUTHOR"),
            body: String::from("BODY"),
        };

        let mut tt = TinyTemplate::new();
        tt.add_template("message", EXAMPLE_TEMPLATE).unwrap();
        tt.set_default_formatter(&format_unescaped);
        let rendered_template = tt
            .render("message", &test_context)
            .unwrap()
            .replace('\r', "");
        insta::assert_snapshot!(rendered_template);
    }

    #[test]
    fn it_templatizes_with_multiple_issues_in_title() {
        let test_context = TemplateContext {
            title: String::from("TITLE"),
            issues: vec![
                TemplateResource {
                    url: format!(
                        "https://github.com/{}/issues/ISSUE_NUMBER1",
                        String::from("REPO_WITH_OWNER")
                    ),
                    number: String::from("ISSUE_NUMBER1"),
                },
                TemplateResource {
                    url: format!(
                        "https://github.com/{}/issues/ISSUE_NUMBER2",
                        String::from("REPO_WITH_OWNER")
                    ),
                    number: String::from("ISSUE_NUMBER2"),
                },
            ],
            pulls: vec![TemplateResource {
                url: format!(
                    "https://github.com/{}/pull/PULL_NUMBER",
                    String::from("REPO_WITH_OWNER")
                ),
                number: String::from("PULL_NUMBER"),
            }],
            author: String::from("AUTHOR"),
            body: String::from("BODY"),
        };

        let mut tt = TinyTemplate::new();
        tt.add_template("message", EXAMPLE_TEMPLATE).unwrap();
        tt.set_default_formatter(&format_unescaped);
        let rendered_template = tt
            .render("message", &test_context)
            .unwrap()
            .replace('\r', "");
        insta::assert_snapshot!(rendered_template);
    }

    #[test]
    fn it_templatizes_with_prs_in_title_when_empty_issues() {
        let test_context = TemplateContext {
            title: String::from("TITLE"),
            issues: vec![],
            pulls: vec![TemplateResource {
                url: format!(
                    "https://github.com/{}/pull/PULL_NUMBER",
                    String::from("REPO_WITH_OWNER")
                ),
                number: String::from("PULL_NUMBER"),
            }],
            author: String::from("AUTHOR"),
            body: String::from("BODY"),
        };

        let mut tt = TinyTemplate::new();
        tt.add_template("message", EXAMPLE_TEMPLATE).unwrap();
        tt.set_default_formatter(&format_unescaped);
        let rendered_template = tt
            .render("message", &test_context)
            .unwrap()
            .replace('\r', "");
        insta::assert_snapshot!(rendered_template);
    }

    #[test]
    fn it_templatizes_without_prs_in_title_when_issues_present() {
        let test_context = TemplateContext {
            title: String::from("TITLE"),
            issues: vec![TemplateResource {
                url: format!(
                    "https://github.com/{}/pull/ISSUE_NUMBER",
                    String::from("REPO_WITH_OWNER")
                ),
                number: String::from("ISSUE_NUMBER"),
            }],
            pulls: vec![TemplateResource {
                url: format!(
                    "https://github.com/{}/pull/PULL_NUMBER",
                    String::from("REPO_WITH_OWNER")
                ),
                number: String::from("PULL_NUMBER"),
            }],
            author: String::from("AUTHOR"),
            body: String::from("BODY"),
        };

        let mut tt = TinyTemplate::new();
        tt.add_template("message", EXAMPLE_TEMPLATE).unwrap();
        tt.set_default_formatter(&format_unescaped);
        let rendered_template = tt
            .render("message", &test_context)
            .unwrap()
            .replace('\r', "");
        insta::assert_snapshot!(rendered_template);
    }

    #[test]
    fn it_templatizes_with_neither_issues_or_prs() {
        let test_context = TemplateContext {
            title: String::from("TITLE"),
            issues: vec![],
            pulls: vec![],
            author: String::from("AUTHOR"),
            body: String::from("BODY"),
        };

        let mut tt = TinyTemplate::new();
        tt.add_template("message", EXAMPLE_TEMPLATE).unwrap();
        tt.set_default_formatter(&format_unescaped);
        let rendered_template = tt
            .render("message", &test_context)
            .unwrap()
            .replace('\r', "");
        insta::assert_snapshot!(rendered_template);
    }
}
