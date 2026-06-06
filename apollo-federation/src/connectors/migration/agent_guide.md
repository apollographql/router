# Apollo Connectors `connect/v0.3` → `connect/v0.4` migration skill

You are helping a developer upgrade an Apollo Connectors–enabled
supergraph from `@link(url: "https://specs.apollo.dev/connect/v0.3")`
to `connect/v0.4`. The [SubSelection/LitObject grammar
unification](https://github.com/apollographql/router/pull/9261) in v0.4
changes how a small but important class of `@connect(selection: …)`
expressions parse. Across a 7,375-supergraph customer corpus:

- 97.4% of `@connect(selection: …)` strings parse identically in v0.3
  and v0.4 — no migration needed.
- 2.1% have at least one site where the v0.4 reading differs from v0.3.

Almost every divergent site falls into one of two mechanical
categories, and the analyzer resolves both **without asking the
developer anything**:

- **Bare `null` / `true` / `false`** in value position (`status: null`,
  `success: true`). In v0.3 the parser looked up a *field* named
  `null`/`true`/`false`, didn't find it, and returned `None`, which
  response normalization surfaced as `null` — accidentally matching the
  literal the developer meant. v0.4 reads it as a literal directly. The
  observable output is the same, so this needs **no edit**.
- **Quoted tokens** (`"@odata.nextLink"`, `"USD"`, `"0.00"`) and **bare
  identifiers**. In v0.3 these were *field accesses* (`Key::Quoted` /
  `Key::Field`); v0.4 silently rereads them as string literals. The
  behavior-preserving fix is a deterministic `$.` fortification:
  `"USD"` → `$."USD"`, `soldTo` → `$.soldTo`. The analyzer applies these
  mechanically.

This is the central design rule: **a quoted token had no string-literal
meaning in v0.3, so treating it as a literal would silently change
behavior — which a migration must never do on the developer's behalf.**
We always preserve the v0.3 reading. If the developer genuinely wants a
literal string, that is a deliberate edit they make later, not something
the migration guesses at.

What remains after those two categories is a small residue of genuinely
ambiguous, structural divergences the analyzer cannot resolve. Those —
and only those — become **questions for the developer**. The whole point
of this skill is to distill the work down to those questions: apply
everything mechanical, then interview the developer about the few real
forks, rather than making them ratify hundreds of non-decisions.

## How this skill works

This is a single agent-driven session, not a file you hand a human to
edit:

1. **Analyze** the project. `connect-migrate analyze` writes a
   **manifest** sorting every divergent site into three buckets:
   *Rewrites to apply* (deterministic), *No action needed* (no-op), and
   *Questions for the developer* (genuine ambiguity).
2. **Apply** the deterministic rewrites yourself, using the precise
   locators in each machine block.
3. **Interview** the developer over the questions — only the genuine
   forks, distilled — and apply their answers.
4. **Verify** by re-running `analyze` and confirming the divergence is
   resolved.

`connect-migrate` has no `apply` subcommand by design: the manifest *is*
the interface, and you are the executor. The edits happen inside your
session, alongside the conversation, so you never break the developer's
flow by bouncing them through a separate tool.

**Two entry points.** Either:

- **Fresh** — no manifest yet: start at Step 1 (analyze).
- **Resume** — you've been handed an existing manifest (often one the
  developer curated — see [Step 3](#step-3-apply-the-rewrites)): skip
  analysis and start at Step 3, applying exactly what the manifest now
  lists. Re-run `analyze` only if you suspect the source changed since
  the manifest was written (Step 3's `id` check catches that).

---

## Prerequisite: install `connect-migrate`

The CLI lives at <https://github.com/apollographql/connect-migrate>.

**While the repo is private, the one-line installer can't be fetched
anonymously** — `curl …/raw.githubusercontent.com/.../install.sh` 404s
without auth. The reliable path is an authenticated release download via
the GitHub CLI (`gh`), which picks up your existing login:

```sh
gh release download -R apollographql/connect-migrate \
  -p "*$(uname -s | tr 'A-Z' 'a-z')-$(uname -m | sed 's/x86_64/x64/;s/aarch64/arm64/')*" \
  --dir /tmp/cm && mkdir -p ~/.local/bin && chmod +x /tmp/cm/connect-migrate-* \
  && mv /tmp/cm/connect-migrate-* ~/.local/bin/connect-migrate
```

(Installs to `~/.local/bin` — no `sudo`, matching `install.sh`. Ensure
`~/.local/bin` is on your `PATH`.)

If you have a token, the piped installer also works:
`curl -fsSL -H "Authorization: token $(gh auth token)" …/install.sh | sh`.
Once the repo is public, the plain `curl …/install.sh | sh` is fine.

Verify with `connect-migrate --version`. **If you cannot install or run
it, stop and tell the developer** — do not hand-migrate (see Step 1).
Windows users download the `.exe` from the Releases page.

---

## Step 1: confirm scope and run the analyzer

*(Resuming from an existing manifest? Skip to [Step 3](#step-3-apply-the-rewrites).)*

Open by asking the developer where to look. The analyzer needs a project
root containing `.graphql` schema files — typically the repository root
or a `subgraphs/` subdirectory.

> Suggested prompt:
>
> > Where should I look for your connector schemas? A directory path
> > relative to the project root is enough; if you're not sure, the
> > repository root is usually the right answer.

Confirm the CLI is available (`connect-migrate --version`); if it errors
with "command not found," install it per the
[Prerequisite](#prerequisite-install-connect-migrate) section, then
retry. If `install.sh` fails (e.g. no binary for the developer's
platform), surface its error verbatim and stop — do not build from
source unattended.

Then, from the project root, write a **timestamped** manifest so each
run is its own durable artifact and never clobbers a prior one:

```sh
connect-migrate analyze subgraphs > connect-migrate-manifest-$(date -u +%Y-%m-%dT%H-%M-%SZ).md
```

`analyze` walks `.graphql` files, finds every `@connect(selection: …)`
directive, dual-parses each selection under the v0.3 and v0.4 grammars,
and writes the differing sites to stdout. Selections that parse
identically are not emitted — they need no migration. Use `-o <path>`
to have `analyze` write the file itself, or `--format json` for one
JSONL record per site if you'd rather consume typed records than parse
markdown.

**The tool's manifest is the source of truth — do not hand-migrate.**
Every edit you make comes from a manifest the tool actually produced.
If you cannot install or run `connect-migrate` (no published binary for
your platform, install failed, no repo access), **stop and tell the
developer** — do **not** reconstruct the migration from memory, the
docs, or web searches. A hand-built migration silently drops
fortifications, and (see Step 5) a post-upgrade `analyze` will *not*
catch the omission — so a from-memory attempt looks clean while quietly
changing behavior. The tool is cheap; guessing is not.

---

## Step 2: read the verdict

Read the `<!-- result: … -->` marker at the top of the manifest and
switch on it before reading prose. See [Result kinds](#result-kinds)
for the full prescription:

- **`empty-scan`** — no `.graphql` files matched. Report the path you
  passed; ask for a different one. Stop.
- **`nothing-to-migrate`** — files scanned, no `@connect` directives.
  Report it; ask whether to look elsewhere. Stop.
- **`safe-to-upgrade`** — directives present, zero divergence. State the
  verdict. No fortifications are needed; the only change is the `@link`
  bump (Step 6) — bump it with the developer's go-ahead.
- **`safe-after-rewrites`** — divergence exists but every site is
  mechanical: apply the rewrites (Step 3), verify (Step 5), then bump the
  `@link` (Step 6). There are **no questions** — a clean bill of health.
- **`needs-decisions`** — apply the rewrites (Step 3), interview the
  developer over the questions (Step 4), verify (Step 5), bump (Step 6).

Do not edit source until you've read the verdict; whoever performs the
`@link` bump, it is the **last** step and only after Step 5 verifies
(see Steps 5–6).

---

## Step 3: apply the rewrites

The manifest's **`## Rewrites to apply`** section lists deterministic
`$.` fortifications. Each is behavior-preserving by construction — it
restores a v0.3 field access that v0.4 would otherwise read as a literal
— so you apply them without asking the developer to adjudicate. Show the
diff; a single batched go-ahead before writing is good practice.

**The `## Rewrites to apply` section is the authoritative work list.**
Apply *exactly* the `site v2` blocks present there, using each block's
`rewrite_to` verbatim — no more, no less. This is what makes the section
editable: to **skip** a rewrite, delete its block; to **change** a
replacement, edit its `rewrite_to`. Never recompute a fortification the
developer removed or overrode, and never apply one that isn't listed. If
the developer curates the list (in the file or by telling you), honor
the curated list as-is.

**Resuming from a handed-in manifest?** Before applying, confirm it's
still current: each block's `id` should still match a directive in the
source at its `file`/`line`. If an `id` no longer resolves, the source
changed since the manifest was written — re-run `analyze` (Step 1) and
re-collect any decisions rather than applying a stale block.

Each rewrite carries a machine block:

```
<!-- connect-migrate site v2
  id: 7470d4c2
  file: itemlibraryserv.graphql
  line: 99
  col: 205
  byte_offset: 8746
  coordinate: Query.merchantItem
  kind: key_quoted_flipped_to_literal_string
  text: "USD"
  source_range: 99..104
  followed_by: nothing
  recommendation: keep-v0.3
  rewrite_to: "$.\"USD\""
-->
```

To apply one:

1. Open `file:` and navigate to **`line:`/`col:`** (or `byte_offset:`) —
   the start of the host `@connect(...)` directive. **Do not use
   `coordinate:` as the locator** — it isn't unique across files; it's
   informational only, useful to sanity-check you're at the right
   directive.
2. Inside that directive's `selection: "…"` (or `"""…"""`) argument,
   the token `text` occupies `source_range` (byte offsets into the
   selection body). Replace it with `rewrite_to` — the exact
   replacement text.
   - **Mind the escaping.** `rewrite_to` is shown JSON-escaped in the
     machine block (e.g. `"$.\"USD\""`). The value you actually splice
     is `$."USD"`. In a **block-string** `"""…"""` selection, write it
     **unescaped** — `currencyCode: $."USD"`. Only in a **single-line**
     `"…"` selection do the inner quotes need escaping —
     `selection: "currencyCode: $.\"USD\""`. (Over-escaping inside a
     block string is the most common splice slip.)
3. **Preserve quoting and indentation.** Keep `"""` vs `"` as the source
   has it. GraphQL block strings strip common leading whitespace at
   parse time, so re-indent the spliced text to match the surrounding
   lines for a clean diff; nothing depends on indentation semantically.

### Safety contract

Every edit must leave the source **at least as good as it was**:

- **Parsing.** Each touched `.graphql` file must still parse under
  `connect/v0.4`. The Step 5 re-analyze is the authoritative check.
- **Behavior.** Write only the fortification in `rewrite_to`. Do not
  expand the change set.
- **Formatting.** Untouched lines stay byte-identical; only the token's
  byte range inside the selection changes.
- **Recovery.** If a post-apply check fails for a site, revert that
  edit and report it before declaring success. Partial success is fine
  only when the developer can see exactly what did and didn't apply.

If you can't satisfy these for a site, **stop and escalate.**

---

## Step 4: interview the developer over the questions

Only `needs-decisions` manifests have a non-empty **`## Questions for the
developer`** section. Each item is a genuine fork the analyzer cannot
resolve — a structural divergence where the v0.3 and v0.4 readings are
both plausible and the right answer depends on the developer's backend.

Distill, don't interrogate. The manifest already groups sites that share
a single decision (`occurrences:` / `(×N)`), so ask **one question per
distinct fork**, not one per site. For each:

- Show the `coordinate`, the `file:line` locator, and the windowed
  selection context the manifest provides.
- State the two readings plainly and ask which matches their intent. Do
  not guess or pre-recommend — their REST API knowledge beats your
  priors.
- Apply their answer with the same Step 3 safety contract. If they
  choose the v0.3 reading, fortify with `$.`; if v0.4, leave it.

> Suggested prompt:
>
> > `Query.merchantTax` parses differently under v0.4 here: [context].
> > In v0.3 this read as X; in v0.4 it reads as Y. Which did you intend?
> > (This one answer covers all N occurrences.)

---

## Step 5: verify — *before* you bump the `@link`

After applying the fortifications (and any interview answers), re-run
`analyze` **while the schemas are still on their original
`connect/v0.n` link** — do **not** bump the `@link` yet. Expected state:

- Sites you fortified no longer appear (their `id` is gone from the new
  manifest).
- No-op sites may still appear — their source is byte-identical and that
  is correct.
- **No new sites** appear. A new divergence means an edit went wrong —
  revert it and escalate.

**Why the order matters — this is the load-bearing check.** `analyze`
only detects *pre-upgrade* divergence: it diffs each schema's linked
`connect/v0.n` against v0.4. The moment you bump a schema's `@link` to
v0.4, its "from" side *is* v0.4, so `analyze` has nothing left to compare
and will report `safe-to-upgrade` **whether or not you applied the
fortifications**. A post-bump re-analyze therefore proves nothing — it
will happily "pass" a migration that silently skipped every fortification
(leaving field accesses as string literals, a behavior change). Verify
*here*, on the old version, where a missed fortification still shows up.

Then run the project's build check (`cargo check`, `npm run check`,
etc.) and, if a sandbox is available, spot-check a selection or two
against real backend responses.

---

## Step 6: bump the `@link` to `connect/v0.4`

**Only after Step 5 comes back clean.** In each connector schema, update
the existing `@link(url: "https://specs.apollo.dev/connect/v0.n", …)` to
`connect/v0.4` in place; leave every other link (federation, etc.)
untouched. This is the **last** edit — it's what actually moves the
schema onto v0.4, which is exactly why it follows verification rather
than preceding it.

---

## Step 7: audit trail and summary

The timestamped manifest is a durable record of what diverged and what
was decided. Suggest committing it alongside the source edits:

    chore(connectors): v0.3 → v0.4 migration

    Driven by `connect-migrate analyze`; manifest preserved as the
    per-decision audit log.

Then summarize:

> Suggested prompt:
>
> > Migration complete. K site(s) fortified, M no-op(s) left as-is, Q
> > question(s) resolved with you. Post-apply `connect-migrate analyze`
> > reports zero unintended divergence. The manifest is preserved as the
> > audit log. Ready to commit?

Name any appropriate follow-up (run tests, deploy a canary, check
staging) explicitly rather than leaving it to the developer to remember.

---

## Manifest format

`connect-migrate analyze` writes a single markdown file, versioned via
the leading `<!-- connect-migrate manifest v2 -->` comment. Refuse to act
on a file whose version you don't recognize.

### Header (machine-readable)

Read these comments before parsing prose:

| Comment | Meaning |
|---------|---------|
| `result:` | The verdict in one token — see [Result kinds](#result-kinds). Always present. |
| `files-scanned:` | `.graphql` files visited. |
| `directives-analyzed:` | `@connect` directives that parsed cleanly under both grammars. |
| `divergent-sites:` | Total divergent tokens reported. |
| `auto-fixes:` | Sites in *Rewrites to apply*. |
| `no-ops:` | Sites in *No action needed*. |
| `questions:` | Sites in *Questions for the developer*. The number that matters: zero means a clean bill of health. |
| `upgrade:` | The source `connect/v0.n` version(s) found across the schemas → the target (`connect/v0.4`). |
| `parse-notices:` | Selections that couldn't be diffed (see *Heads up* below). |

### Title, upgrade, and scope

Directly under the H1 title:

- An **Upgrade** line names the source `connect/v0.n` version(s) detected
  across the schemas and the target — e.g. `connect/v0.2 (7 schemas) ·
  connect/v0.3 (5 schemas) → connect/v0.4`. The "from" version is read
  per schema from its `@link(url: ".../connect/v0.n")`, and each
  schema's selections are diffed at *its own* version against v0.4 — a
  v0.2 schema is compared as v0.2, not assumed to be v0.3.
- A **Scope** line names the project root, the schema count, and the
  directive count, followed by a `Schemas considered:` list of every
  `.graphql` file the run walked. Read it first: it's how you confirm
  the run covered the schemas you expected. If an expected schema isn't
  listed, the path argument was wrong — re-run before trusting the
  verdict.

### Body: three buckets

- **`## Rewrites to apply`** — one `site v2` machine block per
  fortification, followed by a human-readable bullet list. Apply these
  (Step 3).
- **`## No action needed`** — a token-frequency rollup of the no-op
  `null`/`true`/`false` sites. Informational; make no edits.
- **`## Questions for the developer`** — one entry per genuine
  ambiguity, each with a machine block, a one-line statement of the
  fork, and a windowed selection context. Empty unless `result` is
  `needs-decisions`.
- **`## After applying — switch to connect/v0.4`** — the closing
  section, with the ready-to-paste `@link(.../connect/v0.4)` line each
  migrated schema should adopt. Bump the `@link` **last** — only after
  the Step 5 verify passes on the *old* version, since once a schema is
  on v0.4 `analyze` can no longer detect a missed fortification.

### Heads up — selections not analyzed

A `## Heads up — selections not analyzed (N)` section appears only when
some `@connect` selection failed to parse, so it couldn't be diffed.
This is **non-fatal** — the rest of the manifest stands — but each entry
needs a look:

- *parses under the linked spec but not under `connect/v0.4`* — the
  selection would break on upgrade; it must be fixed before migrating.
- *parses under `connect/v0.4` but not under the linked spec* — it uses
  syntax newer than the schema declares (a latent inconsistency).
- *parses under neither* — a pre-existing syntax error, out of scope.

Surface these to the developer; don't try to auto-fix them.

### `site v2` machine block

One block per site (grouped sites carry `occurrences: N`):

| Field | Notes |
|-------|-------|
| `id` | Stable 8-hex content hash; survives line shifts. |
| `file` | Path relative to `project-root`. |
| `line`, `col` | 1-indexed start of the host `@connect` directive. **Authoritative locator.** |
| `byte_offset` | Same location as a byte index. |
| `coordinate` | `Type.field`. **Informational only** — not unique across files; never use as a locator. |
| `from` | The `connect/v0.n` spec the host schema links — the "from" side of this site's upgrade. |
| `kind` | `key_quoted_flipped_to_literal_string`, `key_flipped_to_literal_null`, `key_flipped_to_literal_bool`, `key_field_flipped_to_literal_string`, or a structural kind. |
| `text` | The token's source text (JSON-escaped). |
| `source_range` | Byte range `start..end` of the token *within the selection body*. |
| `followed_by` | `nothing`, `sub_selection`, `key_access`, `method`, `question`. |
| `recommendation` | `keep-v0.3` (fortify), `embrace-v0.4` (no-op), or `???` (a question). |
| `rewrite_to` | Present on auto-fixes: the exact replacement text for `text` (JSON-escaped). |

---

## Result kinds

- **`empty-scan`** — zero `.graphql` files visited (`files-scanned: 0`).
  Almost always a path mistake. Report the path; ask for another. Stop.
- **`nothing-to-migrate`** — files scanned, no `@connect` directives
  parsed cleanly (`directives-analyzed: 0`). Usually means no connectors
  here; but if a `## Heads up` section is present, there *were* `@connect`
  directives that failed to parse — read those before concluding. Stop.
- **`safe-to-upgrade`** — directives present, zero divergence
  (`divergent-sites: 0`). A trustworthy positive verdict, not the
  absence of one. State it; the only change is the `@link` bump
  (Step 6), done with the developer's go-ahead.
- **`safe-after-rewrites`** — divergence exists, but every site is a
  deterministic fortification or a no-op (`questions: 0`). Apply the
  rewrites; no developer decisions are required.
- **`needs-decisions`** — at least one genuinely ambiguous site
  (`questions: > 0`). Apply the rewrites, then interview the developer
  over the questions.

Each schema is diffed at its **own** linked `connect/v0.n` version
against the v0.4 target (see the `Upgrade` line), so a mixed-version
project is handled correctly. A schema already on `connect/v0.4` shows
zero divergence — it's at the target.

---

## Classification doctrine

Why the analyzer sorts sites the way it does:

- **Quoted token → fortify (`keep-v0.3`).** A quoted token was a
  `Key::Quoted` field access in v0.3 with *no* literal-string meaning
  available. Treating it as a v0.4 literal would silently change
  behavior. Always `$."…"`. The character content is irrelevant —
  `"USD"` and `"@odata.nextLink"` are handled identically.
- **Bare identifier → fortify (`keep-v0.3`).** Was a `Key::Field`
  reference in v0.3; same logic. `foo` → `$.foo`.
- **`followed_by` not `nothing` → fortify.** A literal can't have a
  field, method, or sub-selection, so a trailing `.x`, `->m()`, or
  `{ … }` proves the v0.3 field-reference reading was intended.
- **Bare `null`/`true`/`false` → no-op (`embrace-v0.4`).** A field
  literally named `null` is implausible; v0.3 resolved it to null via
  response normalization, so the output coincides with the v0.4 literal.
  Leave it.
- **Structural divergence → question (`???`).** Anything the analyzer
  can't place in the above (a sub-selection/object-literal shape flip it
  can't prove equivalent). These are the only sites that reach the
  developer.

---

## Boundary conditions

- **`legacy_object_to_lit_object`** — a cosmetic AST shape difference
  from the unification; same evaluation semantics. `analyze` does not
  emit these as sites.
- **`v04_only_accepts`** — the selection uses v0.4-only syntax (e.g. the
  `…` spread). Already committed to v0.4; nothing to migrate. Not
  emitted; if you meet one in source, leave it.
- **`v03_only_accepts`** — should never appear after the parser fix.
  Treat as a `connect-migrate` bug and file an issue.
- **Pre-existing syntax errors** — selections that parse under neither
  grammar. They appear in the `## Heads up` section; surface them for
  awareness. They predate the migration and are out of scope. Do not
  repair them.
- **Non-selection spec changes** — this tool diffs `@connect(selection:)`
  *mapping* only. Other version-to-version changes (e.g. the v0.2→v0.3
  arrow-method shape / URI-validation change, or `@connect`/`@source`
  argument changes) are **not** analyzed. For a multi-version jump
  (v0.2 → v0.4), consult the connect spec changelog for anything beyond
  selection mapping.

---

## Failure modes

- **`connect-migrate` not installed.** Run the install command, retry.
  On an unsupported-platform error from `install.sh`, surface it
  verbatim and stop; do not build from source unattended.
- **Source changed between analyze and apply.** Before editing, confirm
  each site's `id` still appears in a fresh `analyze` run. If an `id` is
  gone, the source moved under you — regenerate the manifest and restart
  rather than guess.

---

## Tone

- Read the developer's code; don't speculate. Their REST API knowledge
  beats your priors.
- Lead with the source range and the before/after. Don't bury the diff.
- Apply the mechanical rewrites confidently — they're behavior-preserving
  by construction. Reserve the developer's attention for the genuine
  questions.
- For an ambiguous site, ask. One extra message beats a behavior
  regression.
- Don't claim success unless the post-apply `analyze` reports zero
  unintended divergence.

<!-- FOLLOW-UP: this file is duplicated at
     apollographql/router:apollo-federation/src/connectors/migration/agent_guide.md
     which is embedded into the binary via `connect-migrate agent-guide`.
     Future work: decide between (a) CI-enforced hash match, (b) fetch
     this file at build time, or (c) drop the embed entirely and rely on
     the install having put SKILL.md on disk. -->
