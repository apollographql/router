# Upcoming Changelog Entries

This directory keeps files which individually represent entries that will represent the CHANGELOG produced for the next release.

> **Note**
>
> The files within this directory use a **convention which must be obeyed** in order for the file to be slurped up by automated tooling.

> **Warning**
>
> The aforementioned **tooling doesn't exist yet** but will be created soon. 😺

### How to create a Changelog entry

1. Push the change you are writing a changeset for up to GitHub.
2. Open a pull request for it.  Note that your PR title and body will be used to pre-populate the changeset.
3. On your local checkout, **run `cargo xtask changeset create` from the root of the repository** and follow the prompts.
4. Add, commit and push the changeset file that is created and push it up to GitHub.

### Conventions used in this `.changesets/` directory

The convention used in this directory and obeyed by the `cargo xtask changeset create` command is:

1. Files in this directory must use the `.md` file extension.
2. There must not be multiple changelog entries in a single file.
3. Files *must start with a prefix* that indicates the classification of the changeset.  The prefixes are as follows:
   - **Breaking**: `breaking_`
   - **Feature**: `feat_`
   - **Fixes**: `fix_`
   - **Configuration**: `config_`
   - **Maintenance**: `maint_`
   - **Documentation**: `docs_`
   - **Experimental**: `exp_`
4. The pattern following the prefix can be anything that matches `[a-z_]+` (i.e., any number of lowercased `a-z` and `_`).  Again, `.md` must be on the end as the extension.  For example, `feat_flying_forest_foxes.md`.
5. Other files not matching the above convention will be ignored, including this `README.md`.
6. The files must use the following format:

       ### Brief but complete sentence that stands on its own ([Issue #ISSUE_NUMBER](https://github.com/apollographql/router/issues/ISSUE_NUMBER))

       A description of the fix which stands on its own separate from the title.  It should embrace the use of Markdown to stylize the commentary so it looks great on the GitHub Releases, when shared on social cards, etc.

       By [@USERNAME](https://github.com/USERNAME) in https://github.com/apollographql/router/pull/PULL_NUMBER

     Note the key components:

     - _Brief but complete_ sentence as a **title** that stands on its own without needing to read the description.
     - A link to the **Issue** after the title in the specified format.  e.g., `([Issue #ISSUE_NUMBER](https://...)`.  If there are multiple issues, place multiple references inside the parenthesis.
     - A **description** which _doesn't need the title's context_ to be be understood.
     - A GitHub URL reference to **one or more authors** who contributed
         - If there are multiple authors, use `[@USERNAME](https://github.com/USERNAME)` for each mentioned username: e.g. `[@USERNAME1](https://github.com/USERNAME1) and [@USERNAME2](https://github.com/USERNAME2)`.  This caters to a search-and-replace mechanism when making the final Release.
     - A link to the **pull request** at the end (e.g, `in https://...`)
