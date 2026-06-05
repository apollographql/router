use crate::{Chapter, Exercise, Question};

pub fn all() -> Vec<Chapter> {
    vec![
        ch01(),
        ch02(),
        ch03(),
        ch04(),
        ch05(),
        ch06(),
        ch07(),
        ch08(),
        ch09(),
        ch10(),
        ch11(),
        ch12(),
    ]
}

fn ch01() -> Chapter {
    Chapter {
        number: 1,
        title: "The Big Picture",
        tagline: "What Apollo Router is, why it exists, and what it replaces.",
        exercises: vec![
            Exercise {
                title: "Supergraphs and Subgraphs",
                reading: r#"## What Is a Supergraph?

Imagine your company has dozens of backend teams. The
orders team owns an Orders service, the catalog team
owns a Products service, the accounts team owns a
Users service. Each runs its own GraphQL API.

A client app — say, a mobile app — needs data from all
three to render a single screen. Without coordination,
the app must call three separate APIs, stitch the data
together itself, and know about every internal service.
That's a bad deal for frontend teams.

Federation solves this. Each backend team publishes a
**subgraph** — their slice of the overall schema. A
composition step merges all the subgraphs into one
unified schema called the **supergraph**. Clients see
a single API and have no idea how many services are
behind it.

## Where the Router Fits

The **router** is the runtime entry point for the
supergraph. It is the single process that:

1. Accepts every incoming GraphQL request from clients
2. Builds a **query plan** — a recipe for which
   subgraphs to call, and in what order
3. Fans those calls out to the relevant subgraphs
4. Assembles the results into one response

Think of it like an API gateway, but one that speaks
GraphQL natively and understands the relationships
between your subgraph schemas.

## The Key File

The router's Rust library surface is defined in
`apollo-router/src/lib.rs`. Its module-level doc
comment lists the building blocks available to
plugin authors and operators."#,
                questions: vec![
                    Question::MultipleChoice {
                        stem: "In a federated supergraph, what is the router's primary architectural role?",
                        options: [
                            "It stores the GraphQL schema and serves it to subgraphs on demand.",
                            "It is the single entry point that receives all client queries, plans them, and fans them out to subgraphs.",
                            "It compiles subgraph schemas into a supergraph during the build step.",
                            "It acts as a load balancer that round-robins requests across identical subgraph replicas.",
                        ],
                        answer: 1,
                        explanation: "The router is the runtime entry point — it sits between clients and subgraphs, builds a query plan from the supergraph schema, and orchestrates subgraph fetches. Schema composition happens separately (via Rover/GraphOS), and subgraphs are not identical replicas but distinct services owned by different teams.",
                    },
                    Question::CodeFind {
                        prompt: r#"Open apollo-router/src/lib.rs. The top-level module doc comment lists several modules of interest to plugin authors. Which module is described as containing "high level building blocks for a federated GraphQL router"?"#,
                        file_hint: "apollo-router/src/lib.rs",
                        accepted: &["self", "`self`", "apollo_router", "self (apollo_router)"],
                        hint: "Read the doc comment at the very top of lib.rs — it lists several modules and their purposes.",
                        explanation: "The lib.rs doc comment says `self` (i.e., the `apollo_router` crate root itself) contains high-level building blocks. The other modules listed — `graphql`, `layers`, `plugin`, `services` — are more focused sub-areas for plugin development.",
                    },
                ],
                engineer_note: Some("apollo-router/src/lib.rs is the public API surface of the router library. The module hierarchy declared here (plugin, services, layers, graphql) maps directly onto the extension points available to Rhai scripts, native plugins, and coprocessors."),
            },
            Exercise {
                title: "Problems the Router Solves",
                reading: r#"## One Choke Point, Many Benefits

Because every client request passes through the router,
it becomes a natural place to add cross-cutting
concerns — behavior that applies to all traffic, not
just one subgraph.

## Performance

The router builds an optimal **query plan** for each
request. Independent subgraph fetches run in parallel.
Results are merged in the correct order. Response
caching means repeated queries can be answered without
hitting a subgraph at all.

## Security

Auth logic added to the router runs before any request
reaches a subgraph. That means even if a subgraph
forgets to check a JWT, the router already rejected
the unauthorized request. You get defense-in-depth
without every team having to implement it.

## Observability

Metrics, distributed traces, and structured logs are
emitted for every request. Because all traffic flows
through one binary, you get a consistent, complete
picture of your API's behavior — latency, error rates,
cache hit ratios — without instrumenting each subgraph
individually.

## Operations

The router supports **hot-reload**: drop a new config
file and it picks up the change without restarting.
GraphOS can push a new supergraph schema over Uplink
and the router will adopt it live. Zero-downtime
deploys become the default, not the exception.

## Design Principles

The README's "Design Principles" section summarizes
the philosophy: Correctness, Reliability, Safe
Experimentation, and Usability. Every feature decision
is measured against these four values."#,
                questions: vec![
                    Question::MultipleChoice {
                        stem: "A security team wants to enforce JWT validation for every GraphQL request without requiring each subgraph team to implement it. Where should this logic live?",
                        options: [
                            "In each subgraph, because only the subgraph knows which fields require auth.",
                            "In the client application, so the token is validated before the request is even sent.",
                            "In the router, so it runs once at the edge before any subgraph is called.",
                            "In a separate sidecar process deployed alongside each subgraph.",
                        ],
                        answer: 2,
                        explanation: "The router is the ideal place for auth because all traffic passes through it before reaching any subgraph. This avoids duplicating auth logic across teams and ensures that even a subgraph that omits the check is protected. Client-side validation is easily bypassed; sidecar-per-subgraph is operationally expensive.",
                    },
                    Question::MultipleChoice {
                        stem: "The router's README lists four top-level design principles. Which of the following is NOT one of them?",
                        options: [
                            "Correctness",
                            "Reliability",
                            "Performance",
                            "Usability",
                        ],
                        answer: 2,
                        explanation: "The four principles in the README are Correctness, Reliability, Safe Experimentation, and Usability. Performance matters deeply to the team, but it falls under Reliability (\"predictable latency, RAM and CPU usage, scalability\") rather than being its own top-level principle.",
                    },
                ],
                engineer_note: None,
            },
            Exercise {
                title: "Router vs. the Alternatives",
                reading: r#"## There Are Always Alternatives

Before choosing Apollo Router, teams typically consider
three other options. Understanding why each falls short
explains why the router exists at all.

## Apollo Gateway (JavaScript)

Gateway is the router's direct predecessor — it does
the same job (federated query planning and subgraph
routing) but is written in JavaScript/TypeScript.
It works, but it is slower, consumes more memory, and
lacks several features added to the Rust router. Most
teams migrating to Router see significant latency and
resource improvements without code changes.

## General-Purpose Proxies (Nginx, Envoy)

These tools are excellent at routing HTTP traffic.
They know nothing about GraphQL. They cannot parse a
query to decide which subgraphs to call, cannot merge
partial responses, and cannot enforce field-level
auth rules. You would have to bolt all of that on
yourself — re-implementing what the router already
provides.

## No Gateway: Clients Call Subgraphs Directly

This is the simplest option and the first one teams
usually try. It works until it doesn't:

- Frontend teams must know every internal service URL
- Adding a new subgraph requires updating every client
- There is no single place to add auth, metrics,
  or rate limiting
- Internal service topology leaks into public APIs

The router restores **encapsulation**: clients know
only one URL, and backend teams can reorganize their
services without breaking clients.

## The Frontier: Executable and Startup

The router's startup logic lives in
`apollo-router/src/executable.rs`. That file is a
good place to see how the binary bootstraps — CLI
argument parsing, config loading, and server startup
all flow from there."#,
                questions: vec![
                    Question::MultipleChoice {
                        stem: "A startup has frontend teams calling subgraph services directly. What is the most significant architectural problem this creates as the system grows?",
                        options: [
                            "GraphQL queries become slower because there is no query planner to parallelize fetches.",
                            "Frontend teams become coupled to internal service topology — any backend reorganization risks breaking clients.",
                            "Subgraphs cannot be written in different programming languages when clients call them directly.",
                            "Without a router, subgraphs cannot use Apollo Federation directives.",
                        ],
                        answer: 1,
                        explanation: "The deepest problem with clients calling subgraphs directly is broken encapsulation. Every client must know every service URL. When backend teams split, merge, or rename a service, every client must update. The router provides a stable, single URL surface so backend teams can evolve freely without coordinating with every client team.",
                    },
                    Question::Reflection {
                        prompt: "Think about a frontend team and a backend team at your organization (or a hypothetical one). How does introducing a router between them change what each team needs to know about the other? What does each team gain, and what — if anything — do they give up?",
                        key_points: &[
                            "Frontend teams only need to know one endpoint URL and the supergraph schema — not which subgraph owns which field.",
                            "Backend teams can reorganize, rename, or split their subgraph without breaking clients, as long as the schema contract is maintained.",
                            "The router team (or platform team) takes on responsibility for the router's availability — a new operational dependency.",
                            "Schema changes still require coordination — the supergraph schema is the contract both sides must agree on.",
                        ],
                    },
                ],
                engineer_note: Some("apollo-router/src/executable.rs is the startup entry point. It wires together CLI argument parsing (clap), config loading, and the RouterHttpServer. Tracing and telemetry are initialized here before the server starts accepting connections — worth reading if you want to understand how the binary bootstraps end-to-end."),
            },
        ],
    }
}

fn ch02() -> Chapter {
    Chapter {
        number: 2,
        title: "The Request Lifecycle",
        tagline: "Every request travels the same four-stage pipeline — understanding each stop unlocks the whole system.",
        exercises: vec![
            Exercise {
                title: "The Four Stages",
                reading: r#"## What Happens When a Request Arrives?

When a client sends a GraphQL query to Apollo Router,
that request doesn't jump straight to your subgraphs.
It passes through four distinct stages, in order,
every single time.

## Router Stage
The request arrives as raw HTTP — bytes and headers.
Router doesn't know it's GraphQL yet. This is the
outermost boundary: rate limiting, auth tokens, and
request logging all fit naturally here.

## Supergraph Stage
Now the payload is parsed as GraphQL. The operation is
validated against your supergraph schema and a query
plan is created. This plan describes which subgraphs
to call and in what order.

## Execution Stage
The query plan runs. Router dispatches HTTP calls to
one or more subgraphs, potentially in parallel. This
stage sees the whole picture — all subgraph fetches
for this one client request.

## Subgraph Stage
Each individual subgraph call is its own Subgraph
stage invocation. If your query needs data from three
subgraphs, the Subgraph stage runs three times — once
per fetch. Plugins here see only one subgraph call at
a time.

## The Key Insight
These stages are deterministic and sequential. A
request always moves forward through them; it never
skips or reverses. Understanding this order tells you
exactly where to hook in for any given task."#,
                questions: vec![
                    Question::MultipleChoice {
                        stem: "A client sends a query that needs data from three subgraphs. How many times does the Subgraph stage run for that single request?",
                        options: [
                            "Once — Router batches all subgraph calls into a single stage invocation",
                            "Twice — once for planning, once for execution",
                            "Three times — once per subgraph fetch",
                            "Four times — once per pipeline stage",
                        ],
                        answer: 2,
                        explanation: "The Subgraph stage is invoked once per individual subgraph HTTP call. A query touching three subgraphs produces three separate Subgraph stage invocations. This matters for plugins: a Subgraph-stage plugin sees one call at a time, not the full execution picture.",
                    },
                    Question::MultipleChoice {
                        stem: "At which stage does Apollo Router first become aware that the incoming request is a GraphQL operation?",
                        options: [
                            "Router stage — it parses GraphQL immediately on arrival",
                            "Supergraph stage — this is where the payload is parsed and validated",
                            "Execution stage — parsing happens just before subgraph dispatch",
                            "Subgraph stage — each subgraph re-parses the relevant fragment",
                        ],
                        answer: 1,
                        explanation: "The Router stage only sees raw HTTP bytes and headers — no GraphQL awareness yet. The Supergraph stage is where the body is parsed as GraphQL, validated against the schema, and a query plan is built. Plugins that need to inspect or modify the parsed operation must operate at Supergraph or later.",
                    },
                ],
                engineer_note: None,
            },
            Exercise {
                title: "Choosing the Right Stage",
                reading: r#"## Earlier vs. Later: A Real Trade-off

Every stage in the pipeline offers a different
balance of control and context. Picking the right
stage for your plugin is one of the most important
design decisions you'll make.

## Earlier = More Control, Less Context
At the Router stage you can reject a request before
any GraphQL work happens. That's cheap and powerful.
But you can't read the operation name or inspect
field selections — GraphQL hasn't been parsed yet.

## Later = More Context, Narrower Scope
At the Subgraph stage you can see exactly which
subgraph is being called and modify that one HTTP
request. But you can't see the other subgraph calls
happening in parallel, and the main response has
already started forming.

## A Practical Rule of Thumb
Use the earliest stage that gives you enough
information to do your job.

Validating a JWT in a header? -> Router stage.
No need to parse GraphQL first.

Adding a custom field to every subgraph request? ->
Subgraph stage. That's the only place you can see
the per-subgraph HTTP call.

Enforcing field-level authorization? -> Supergraph
or Execution stage. You need to know which fields
were requested.

## Why This Matters
Plugins at early stages run for every request —
keep them fast. Plugins at late stages may run
multiple times per request (once per subgraph)."#,
                questions: vec![
                    Question::MultipleChoice {
                        stem: "Your team wants to validate a JWT from the Authorization header and reject unauthorized requests as early as possible. Which stage is the best fit?",
                        options: [
                            "Router stage — JWT is in the HTTP header, no GraphQL parsing needed",
                            "Supergraph stage — you need the parsed operation to check permissions",
                            "Execution stage — auth should happen just before subgraph dispatch",
                            "Subgraph stage — each subgraph should validate its own auth",
                        ],
                        answer: 0,
                        explanation: "JWT validation only requires HTTP headers, which are fully available at the Router stage. Waiting until Supergraph or later wastes CPU parsing GraphQL for requests that will be rejected anyway. The Router stage is the right \"earliest stage with enough context\" choice here.",
                    },
                    Question::MultipleChoice {
                        stem: "A plugin needs to inject a tenant-specific header into every outbound subgraph HTTP request. Which stage must it use?",
                        options: [
                            "Router stage — modify headers once before any processing begins",
                            "Supergraph stage — headers can be forwarded during query planning",
                            "Execution stage — this is where subgraph requests are assembled",
                            "Subgraph stage — this is the only stage where per-subgraph HTTP requests exist",
                        ],
                        answer: 3,
                        explanation: "Individual subgraph HTTP requests only exist at the Subgraph stage. Earlier stages haven't created them yet. A plugin that needs to modify an outbound subgraph request — headers, body, URL — must operate at the Subgraph stage, and it will run once per subgraph call.",
                    },
                ],
                engineer_note: None,
            },
            Exercise {
                title: "Tower Services Under the Hood",
                reading: r#"## What Is a Tower Service?

Each stage in the router pipeline is implemented as
a Tower service. Tower is a Rust library that defines
a single core trait:

    trait Service<Request> {
        fn call(&mut self, req: Request)
            -> Future<Output = Response>;
    }

A service takes a request and asynchronously returns
a response. That's it. Simple, but composable.

## Middleware = Wrapping Services

Tower's power comes from layering. A middleware wraps
an inner service — it can inspect or modify the
request before passing it down, and inspect or modify
the response on the way back up. In Apollo Router,
plugins work exactly this way.

Think of it like Express middleware in Node.js or
Rack middleware in Rails, but typed and async. Each
plugin you write is a layer that wraps one of the
four stage services.

## Why This Design?

The Tower model gives you two things:

1. Composability — stages can be wrapped
   independently. An auth plugin wraps the Router
   service; a tracing plugin wraps all four. They
   don't need to know about each other.

2. Testability — because each stage is just a
   service, you can test it in isolation by calling
   it directly with a fabricated request.

## The Pipeline as a Stack

Visualize the pipeline as nested layers:
outer plugins -> Router -> Supergraph -> Execution
-> Subgraph -> actual HTTP. Each layer can short-
circuit (return early) or pass through to the next."#,
                questions: vec![
                    Question::MultipleChoice {
                        stem: "In the Tower service model used by Apollo Router, what does it mean for a plugin to be \"middleware\"?",
                        options: [
                            "The plugin runs in a separate process and communicates over gRPC",
                            "The plugin wraps a stage service, seeing the request before and the response after the inner service runs",
                            "The plugin is registered in a middleware table and called by name at runtime",
                            "The plugin replaces the stage service entirely, taking full ownership of request handling",
                        ],
                        answer: 1,
                        explanation: "Middleware in Tower wraps an inner service. The plugin's `call` method receives the request, can modify it, then invokes the inner service, and can modify the response before returning it. This is the classic middleware pattern: before/after logic around a next() call. The inner service is not replaced — it is wrapped.",
                    },
                    Question::CodeFind {
                        prompt: "The router uses a `PipelineStep` enum to label which stage a span or event belongs to. Find this enum in the services module. What are the four variants?",
                        file_hint: "apollo-router/src/services/mod.rs",
                        accepted: &[
                            "PipelineStep",
                            "RouterRequest",
                            "SupergraphRequest",
                            "ExecutionRequest",
                            "SubgraphRequest",
                        ],
                        hint: "Search for `enum PipelineStep` around line 55 of services/mod.rs.",
                        explanation: "The `PipelineStep` enum in services/mod.rs has four variants: RouterRequest, SupergraphRequest, ExecutionRequest, and SubgraphRequest — one per pipeline stage. It is used to tag telemetry spans so that traces show which stage generated each event.",
                    },
                ],
                engineer_note: Some("services/mod.rs line ~55: `pub(crate) enum PipelineStep` — four variants map 1:1 to the service request types re-exported just above it. The `From<PipelineStep> for opentelemetry::Value` impl below it is how stage names appear in your traces."),
            },
            Exercise {
                title: "Context: The Shared Scratchpad",
                reading: r#"## A Request Carries Its Own State

As a request moves through the four stages, plugins
often need to share information with each other.
An auth plugin validates a token at the Router stage
— how does an authorization plugin at the Supergraph
stage know who the user is?

The answer is Context.

## What Context Is

Every request carries a `Context` object from the
moment it arrives until the response leaves. Think
of it as a typed key-value map attached to the
request — a scratchpad that any plugin at any stage
can read from or write to.

    // Write something at the Router stage:
    context.insert("user_id", user_id)?;

    // Read it at the Supergraph stage:
    let uid = context.get::<_, String>("user_id")?;

## Why Not Just Use Headers?

Headers travel outbound to subgraphs unless you
explicitly forward them. Context is internal — it
lives only inside the router process and is never
sent to subgraphs unless you explicitly copy values
out. That makes it safe for sensitive data like
decoded token claims.

## Context Is Shared, Not Cloned

All stages in a single request share the *same*
Context instance. Writes at one stage are
immediately visible to later stages. This makes it
the canonical communication channel between plugins
in a request's lifetime."#,
                questions: vec![
                    Question::CodeFind {
                        prompt: "Find the `Context` struct definition in the context module. What are the names of two public methods for reading and writing values?",
                        file_hint: "apollo-router/src/context/mod.rs",
                        accepted: &[
                            "insert",
                            "get",
                            "upsert",
                            "Context",
                        ],
                        hint: "Look around line 73 for the struct, and lines 141-204 for the impl block with get/insert/upsert.",
                        explanation: "The `Context` struct (defined around line 73) has `get` and `insert` as its primary read/write methods, plus `upsert` for atomic read-modify-write. All three accept generic key and value types, making Context a typed map rather than a plain string map.",
                    },
                    Question::MultipleChoice {
                        stem: "Why is the Context object a better choice than HTTP headers for passing decoded JWT claims between pipeline stages?",
                        options: [
                            "Context is faster because it uses shared memory, while headers are copied on each stage transition",
                            "Context is internal to the router process and never forwarded to subgraphs unless explicitly copied, keeping sensitive data safe",
                            "HTTP headers are read-only after the Router stage, so only Context can be written to later",
                            "Subgraphs reject requests that contain custom headers, so Context is the only option",
                        ],
                        answer: 1,
                        explanation: "Context lives only inside the router process. It is never automatically sent to subgraphs. HTTP headers, by contrast, can be forwarded to subgraphs (intentionally or accidentally). Storing decoded token claims in Context keeps them internal, which is the right security boundary.",
                    },
                ],
                engineer_note: Some("context/mod.rs lines 141-220: the `get`, `insert`, and `upsert` methods use serde_json under the hood — values are serialized to JSON for storage and deserialized on read. This means any `Serialize + DeserializeOwned` type works as a context value, but it also means there is a (usually negligible) serialization cost per access."),
            },
        ],
    }
}

fn ch03() -> Chapter {
    Chapter {
        number: 3,
        title: "Configuration",
        tagline: "One YAML file to rule them all.",
        exercises: vec![
            Exercise {
                title: "The router.yaml File",
                reading: r#"## Everything Starts Here

The Apollo Router is configured through a single YAML file,
conventionally named router.yaml. Every feature the router
supports — CORS, authentication, telemetry, rate limiting,
caching — has a corresponding section in that file.

## Top-Level Keys

Each major capability gets its own top-level key:

    supergraph:
      listen: 0.0.0.0:4000
    cors:
      allow_any_origin: true
    telemetry:
      exporters:
        tracing:
          common:
            service_name: my-router

Plugins (custom or built-in) live under a plugins key,
each namespaced by their plugin identifier:

    plugins:
      apollo.telemetry:
        ...

## Strongly Typed and Validated Early

The router deserializes its config into a strongly typed
Rust struct (Configuration) at startup. If you provide a
field that doesn't exist, use the wrong type, or omit a
required field, the router refuses to start and prints an
exact error message pointing to the problem.

This "fail fast" design means you catch mistakes before
any traffic is served — no silent misconfiguration in
production.

## JSON Schema for Tooling

The router generates a JSON Schema from the same Rust
types. IDEs can use this for autocompletion and inline
validation. CI pipelines can lint your config against the
schema before deploying."#,
                questions: vec![
                    Question::MultipleChoice {
                        stem: "What happens when you start the router with a config file that contains a misspelled field name?",
                        options: [
                            "The router ignores the unknown field and starts normally",
                            "The router starts but logs a warning about the unknown field",
                            "The router refuses to start and prints an error identifying the bad field",
                            "The router applies the closest matching field name as a best guess",
                        ],
                        answer: 2,
                        explanation: "The router uses strongly typed deserialization — unknown or incorrectly typed fields cause an immediate startup failure with a descriptive error. This fail-fast behavior prevents silent misconfiguration from reaching production. Options A and D would mask errors; option B would still let bad config run.",
                    },
                    Question::CodeFind {
                        prompt: "Find the main Configuration struct that holds all router config. What file is it in, and what derive macros does it use?",
                        file_hint: "apollo-router/src/configuration/mod.rs",
                        accepted: &["mod.rs", "configuration/mod.rs", "src/configuration/mod.rs"],
                        hint: "Look for `pub struct Configuration` — it's around line 151 in configuration/mod.rs.",
                        explanation: "The Configuration struct in apollo-router/src/configuration/mod.rs is the root of all router config. It derives JsonSchema (for tooling), Serialize, and uses the Derivative crate for Debug. Every top-level YAML key corresponds to a named field on this struct.",
                    },
                ],
                engineer_note: Some("The Configuration struct at line 151 of apollo-router/src/configuration/mod.rs is worth a close read — each field maps directly to a top-level YAML key, and the #[serde(default)] attributes reveal which sections are optional. The JsonSchema derive is what powers `generate_config_schema()` in configuration/schema.rs."),
            },
            Exercise {
                title: "Hot Reload",
                reading: r#"## Changing Config Without Restarting

One of the router's most practical features is hot reload:
when you edit router.yaml while the router is running, it
detects the change and applies the new configuration
without dropping connections or restarting the process.

This matters in production. Restarting a router means a
gap in availability and a cold query-plan cache. Hot reload
avoids both.

## How It Works

The router's state machine (state_machine.rs) watches the
config file for filesystem events. When a change is
detected, it transitions to a "Reloading" state:

1. The new config is parsed and validated.
2. If valid, plugins are re-initialized with both the old
   and the new config (so they can diff and react).
3. Traffic continues on the old config until the reload
   succeeds — there's no downtime window.
4. If the new config is invalid, the router stays on the
   old config and logs an error.

## What Requires a Full Restart

Not everything can be hot-reloaded. Changes to the
listening address (the port and IP the router binds to)
require a full process restart, because rebinding a TCP
socket mid-flight isn't safe.

Most other changes — telemetry, authentication settings,
CORS policy, plugin behavior — are hot-reloaded.

## Retries on Failure

The reload section of router.yaml lets you configure how
many times the router retries a failed reload before
giving up and staying on the previous configuration."#,
                questions: vec![
                    Question::MultipleChoice {
                        stem: "You change the `supergraph.listen` address in router.yaml while the router is running. What happens?",
                        options: [
                            "The router hot-reloads and immediately starts listening on the new address",
                            "The router logs a warning and ignores the address change until next restart",
                            "The change requires a full process restart to take effect",
                            "The router starts listening on both the old and new addresses simultaneously",
                        ],
                        answer: 2,
                        explanation: "Changing the listening address requires a full process restart because rebinding a TCP socket while serving traffic is not safe. Most config changes are hot-reloadable, but the listen address is a fundamental network binding that must be established at startup. The other options describe behaviors the router does not implement.",
                    },
                    Question::MultipleChoice {
                        stem: "While a hot reload is in progress and the new config is being validated, what happens to incoming requests?",
                        options: [
                            "They are queued and held until the reload completes",
                            "They are rejected with a 503 Service Unavailable response",
                            "They continue to be served by the previous (committed) configuration",
                            "They are served by the new configuration immediately, before validation finishes",
                        ],
                        answer: 2,
                        explanation: "The router keeps serving traffic on the committed (old) configuration while a reload is pending. Only after the new config is fully validated and plugins are re-initialized does the router switch over. This zero-downtime approach means a bad config file doesn't take down the router — it just logs an error and stays on the last known good config.",
                    },
                ],
                engineer_note: Some("In apollo-router/src/state_machine.rs, the Reloading variant holds a PendingReload struct. The state transition logic around line 283 shows that any new Configuration object unconditionally triggers a reload (there's no equality check). The plugin re-init path passes PluginInit with both old and new config, letting plugins handle stateful transitions gracefully."),
            },
            Exercise {
                title: "Environment Variables and Migrations",
                reading: r#"## Secrets Don't Belong in Config Files

Config files often end up in version control. That's fine
for most settings, but not for secrets like API keys,
database passwords, or authentication endpoints.

The router supports environment variable expansion using
the ${env.VARIABLE_NAME} syntax anywhere in a YAML value:

    authentication:
      jwt:
        jwks_url: "${env.AUTH_SERVER_URL}/jwks.json"

At startup (and on hot reload), the router substitutes
the actual value from the environment before parsing.
If the variable is not set, the router fails fast rather
than silently using an empty string.

The env. prefix is intentional — it's a namespacing
convention that keeps the syntax unambiguous and
extensible (other expansion modes could be added later).

## Configuration Migrations

The router's config schema evolves between versions.
Fields get renamed, moved, or removed as the team learns
what works. To avoid breaking existing deployments, the
router ships with a migration system.

There are 50+ migration files in:

    apollo-router/src/configuration/migrations/

Each YAML file describes how to transform an old config
key into the new equivalent. When the router encounters
a deprecated key, it warns you and tells you exactly what
the new path is.

You can also run the router with --upgrade-config to
automatically rewrite your config file to the current
schema — useful when jumping several versions at once."#,
                questions: vec![
                    Question::MultipleChoice {
                        stem: "In router.yaml, you write `api_key: \"${env.APOLLO_KEY}\"` but forget to set the APOLLO_KEY environment variable before starting the router. What happens?",
                        options: [
                            "The router starts with an empty string for the api_key value",
                            "The router starts and logs a warning about the missing variable",
                            "The router uses the literal string \"${env.APOLLO_KEY}\" as the value",
                            "The router fails to start and reports that the variable could not be expanded",
                        ],
                        answer: 3,
                        explanation: "The router's expansion logic fails fast when a referenced environment variable is not set, rather than silently using an empty or literal value. This prevents subtle runtime failures where an empty key passes config validation but fails at the point of use. Options A, B, and C all describe silent failure modes that would make production debugging much harder.",
                    },
                    Question::Reflection {
                        prompt: "Why is it better to use ${env.SECRET_KEY} in router.yaml rather than putting the secret value directly in the file? Think about at least two different scenarios where this matters.",
                        key_points: &[
                            "Config files stored in version control would expose secrets to everyone with repo access — and to tools like GitHub secret scanning.",
                            "Environment variables can be rotated or injected by secret management systems (Vault, AWS Secrets Manager, Kubernetes secrets) without touching the config file.",
                            "The same router.yaml can be used across environments (dev/staging/prod) with different secrets injected per environment.",
                            "Audit trails: secret management systems can log every access to a credential, which is impossible if the secret is embedded in a file.",
                        ],
                    },
                ],
                engineer_note: None,
            },
        ],
    }
}

fn ch04() -> Chapter {
    Chapter {
        number: 4,
        title: "The Plugin System",
        tagline: "How to extend the router — from config to live traffic",
        exercises: vec![
            Exercise {
                title: "What Is a Plugin?",
                reading: r#"## The Big Picture

A plugin is a Rust struct that hooks into the router's
request pipeline. Each plugin can intercept traffic at
one or more of the four pipeline stages, transform
requests and responses, and optionally expose its own
HTTP endpoints.

You can think of plugins as middleware in other
frameworks — except each plugin wraps an entire
stage of the pipeline, not just a single handler.

## The Plugin Trait

Every plugin implements the Plugin trait, which lives in:

    apollo-router/src/plugin/mod.rs

The trait has one required associated type and one
required method:

    type Config: JsonSchema + DeserializeOwned + Send;
    async fn new(init: PluginInit<Self::Config>) -> Result<Self, BoxError>;

The Config type maps directly to a section of the
router's YAML configuration. When the router starts
(or reloads), it deserializes your config section and
passes it to new().

All four service hooks — router_service(),
supergraph_service(), execution_service(), and
subgraph_service() — have default implementations
that simply return the service unchanged. You only
override the hooks you care about."#,
                questions: vec![
                    Question::MultipleChoice {
                        stem: "When is a plugin's `new()` method called?",
                        options: [
                            "Once per incoming request",
                            "Once at router startup, and again on each config reload",
                            "Once per subgraph in the query plan",
                            "Only when the plugin's config section is non-empty",
                        ],
                        answer: 1,
                        explanation: "new() is called once at startup and once on each hot reload, not per-request. This makes plugins cheap at request time — all expensive setup (HTTP clients, connection pools, key caches) happens in new(). Options A and C would make plugins prohibitively expensive. Option D is wrong: a missing config section simply uses the type's Default value or causes an error.",
                    },
                    Question::CodeFind {
                        prompt: "Find the Plugin trait definition. What is the name of the associated type every Plugin must declare?",
                        file_hint: "apollo-router/src/plugin/mod.rs",
                        accepted: &["Config"],
                        hint: "Look for `pub trait Plugin` and the `type` keyword inside it.",
                        explanation: "The Plugin trait requires `type Config: JsonSchema + DeserializeOwned + Send`. This associated type tells the router how to deserialize the plugin's YAML config section and how to generate a JSON Schema for documentation and validation.",
                    },
                ],
                engineer_note: Some("The trait is split into Plugin (stable), PluginUnstable, and PluginPrivate. Blanket impls wire them together: every Plugin automatically satisfies PluginUnstable, and every PluginUnstable automatically satisfies PluginPrivate. apollo-router/src/plugin/mod.rs lines 503-650."),
            },
            Exercise {
                title: "PluginInit — What a Plugin Gets at Startup",
                reading: r#"## The Startup Package

When the router calls your plugin's new() it passes a
single argument: PluginInit<Self::Config>. Think of it
as a care package containing everything a plugin might
need to initialize itself.

## Key Fields

    pub config: T

Your plugin's own typed config, already deserialized
from YAML.

    pub supergraph_sdl: Arc<String>

The full supergraph schema as SDL text. Useful if your
plugin needs to inspect type definitions at startup.

    subgraph_schemas: Arc<HashMap<String, Arc<Valid<Schema>>>>

Parsed schemas for each subgraph, keyed by name.
Handy for plugins that need to understand what fields
each subgraph owns.

    notify: Notify<String, graphql::Response>

A pub/sub channel used by the subscription system.
Most plugins ignore this.

    license: Arc<LicenseState>

The router's current license state, including any
feature restrictions.

## Previous Config on Reload

There is also a previous_config field (not public)
that carries the prior config on a hot reload. The
router uses this internally to allow plugins to
detect what changed and migrate state gracefully."#,
                questions: vec![
                    Question::MultipleChoice {
                        stem: "A plugin wants to scan every type in the supergraph schema at startup. Which PluginInit field gives it the full schema as raw SDL text?",
                        options: [
                            "subgraph_schemas",
                            "supergraph_sdl",
                            "config",
                            "license",
                        ],
                        answer: 1,
                        explanation: "supergraph_sdl is an Arc<String> containing the full supergraph SDL. subgraph_schemas provides parsed per-subgraph schemas but not the merged supergraph. config only has the plugin's own YAML section. license carries licensing state, not schema data.",
                    },
                    Question::CodeFind {
                        prompt: "Open the PluginInit struct definition. Which field tells the plugin whether it is starting fresh or reloading a changed config?",
                        file_hint: "apollo-router/src/plugin/mod.rs",
                        accepted: &["previous_config"],
                        hint: "Look at the fields of `pub struct PluginInit<T>` — one of them is `Option<T>`.",
                        explanation: "previous_config: Option<T> is Some when the router is reloading due to a config change, and None on the very first startup. A plugin can compare old and new configs to decide whether to rebuild expensive resources like connection pools.",
                    },
                ],
                engineer_note: None,
            },
            Exercise {
                title: "The Four Service Hooks",
                reading: r#"## Four Stages, Four Hooks

The router pipeline has four stages, and the Plugin
trait exposes one hook for each:

    router_service()       — raw HTTP request/response
    supergraph_service()   — GraphQL request/response
    execution_service()    — query plan execution
    subgraph_service()     — per-subgraph HTTP calls

Each hook receives a BoxService for that stage and
returns a (possibly wrapped) BoxService. The default
implementation returns the service unchanged.

## Choosing the Right Hook

router_service is the outermost layer. It sees raw
HTTP before any GraphQL parsing. Good for: auth checks
that don't need the operation body, rate limiting,
request ID injection.

supergraph_service sees a parsed GraphQL request.
Good for: operation-level policies, response shaping.

execution_service fires once a query plan exists.
Good for: blocking queries by plan shape.

subgraph_service fires once per subgraph call —
potentially multiple times per request. It also
receives the subgraph name, letting you apply
different logic to different subgraphs. Good for:
adding auth headers to specific upstreams.

## The Onion Model

Plugins wrap services like layers of an onion. The
order plugins appear in your config YAML determines
the wrapping order. Earlier plugins are outermost —
they intercept requests first and see responses last."#,
                questions: vec![
                    Question::MultipleChoice {
                        stem: "A plugin needs to add a custom Authorization header to calls going to the `inventory` subgraph, but NOT to any other subgraph. Which hook is the right choice?",
                        options: [
                            "router_service, and filter by URL inside the hook",
                            "supergraph_service, and inspect the operation fields",
                            "subgraph_service, using the subgraph_name parameter",
                            "execution_service, and read the query plan nodes",
                        ],
                        answer: 2,
                        explanation: "subgraph_service receives the subgraph name as its first parameter, making per-subgraph logic straightforward. router_service and supergraph_service fire before the router knows which subgraphs will be called. execution_service has access to the query plan but doesn't intercept the actual HTTP calls to subgraphs.",
                    },
                    Question::MultipleChoice {
                        stem: "In the onion model, if Plugin A is listed before Plugin B in the YAML config, which statement is true?",
                        options: [
                            "Plugin A sees the request after Plugin B",
                            "Plugin A sees the request before Plugin B, and sees the response after Plugin B",
                            "Plugin B sees both the request and response before Plugin A",
                            "Order in YAML has no effect; plugins run alphabetically",
                        ],
                        answer: 1,
                        explanation: "Earlier plugins are outermost wrappers. Plugin A intercepts the request first (before B), and because it's the outer layer it also sees the response last (after B has already processed it). This mirrors how Tower's ServiceBuilder stacks layers.",
                    },
                ],
                engineer_note: None,
            },
            Exercise {
                title: "Registering a Plugin",
                reading: r#"## The register_plugin! Macro

Writing a Plugin impl is only half the job — you also
need to tell the router the plugin exists. The
register_plugin! macro handles this:

    register_plugin!("my_company", "my_plugin", MyPlugin);

This registers the plugin under the name
"my_company.my_plugin". That name is the key users
put in their YAML config:

    plugins:
      my_company.my_plugin:
        some_setting: true

Apollo's own built-in plugins use "apollo" as the
group (e.g., "apollo.csrf", "apollo.rhai").

## How Registration Actually Works

The macro uses the `linkme` crate, which provides a
`distributed_slice` — a list that is assembled at
link time, not at runtime. Each call to
register_plugin! adds a Lazy<PluginFactory> entry to
a global PLUGINS slice. When the router boots, it
iterates PLUGINS to discover everything available.

No runtime registration calls, no central registry
file to edit. The linker does the work.

## Naming Convention

Plugin names use "scope.name" format. Scope is
typically a domain or company name. This prevents
name collisions when multiple plugins are loaded.
Apollo's experimental features use "experimental"
as the scope."#,
                questions: vec![
                    Question::CodeFind {
                        prompt: "Find a call to register_plugin! in a built-in plugin. What group and name does the `forbid_mutations` plugin register under?",
                        file_hint: "apollo-router/src/plugins/forbid_mutations.rs",
                        accepted: &["apollo", "forbid_mutations", "apollo.forbid_mutations"],
                        hint: "Search for `register_plugin!` near the bottom of the file.",
                        explanation: "The macro call is `register_plugin!(\"apollo\", \"forbid_mutations\", ForbidMutations)`. This registers the plugin as \"apollo.forbid_mutations\", which is the key users write in their router YAML. The third argument is the Rust type that implements the Plugin trait.",
                    },
                    Question::MultipleChoice {
                        stem: "The `linkme` crate's `distributed_slice` used by register_plugin! collects plugin factories at which point?",
                        options: [
                            "The first time a GraphQL request arrives",
                            "When the router reads its YAML configuration file",
                            "At program link time, before main() runs",
                            "When the plugin's new() method is first called",
                        ],
                        answer: 2,
                        explanation: "distributed_slice is a linker-level feature: the linker merges all the individual plugin entries into the PLUGINS slice before the program even starts. This means plugin discovery has zero runtime cost and no central registry to maintain. Options A, B, and D all describe runtime events.",
                    },
                ],
                engineer_note: Some("The PLUGINS distributed_slice is declared at apollo-router/src/plugin/mod.rs line 64. The register_plugin! macro (line 843) emits a #[linkme::distributed_slice(PLUGINS)] static for each plugin. The linkme crate uses linker sections (similar to how C++ static initializers work) to collect all entries without any runtime coordination."),
            },
            Exercise {
                title: "A Real Plugin: forbid_mutations",
                reading: r#"## Reading a Complete Plugin

The forbid_mutations plugin is one of the simplest
built-in plugins — perfect for seeing all the pieces
together in one file.

    apollo-router/src/plugins/forbid_mutations.rs

## Structure

The plugin struct holds just one field:

    struct ForbidMutations { forbid: bool }

Its config is a newtype wrapper around bool:

    struct ForbidMutationsConfig(bool);

The impl Plugin block declares Config = ForbidMutationsConfig,
constructs the struct from init.config.0, and overrides
only execution_service — the one stage where the query
plan is available to inspect.

## The Service Hook

Inside execution_service, the plugin uses
ServiceBuilder::new().checkpoint(...) to short-circuit
requests that contain mutations when forbid is true.
The checkpoint pattern lets a layer either continue
the request (ControlFlow::Continue) or break out
with an early response (ControlFlow::Break).

If forbid is false, the plugin returns the service
unchanged — exactly the same as the default impl.

## activate()

The Plugin trait also has an activate() hook (default:
no-op). It is called after all plugins finish new()
and are about to go live. It's useful for side effects
that must happen only once the full set of plugins is
ready — for example, starting a background task that
depends on another plugin being initialized."#,
                questions: vec![
                    Question::MultipleChoice {
                        stem: "The forbid_mutations plugin only overrides `execution_service`, not `supergraph_service`. Why is `execution_service` the right stage for this check?",
                        options: [
                            "execution_service is faster than supergraph_service",
                            "The query plan — which identifies mutation operations — is only available at the execution stage",
                            "supergraph_service cannot return early HTTP errors",
                            "Mutations are only visible after subgraph responses are merged",
                        ],
                        answer: 1,
                        explanation: "By the time execution_service runs, the router has parsed the operation and built a query plan. The ExecutionRequest exposes query_plan.contains_mutations(), making it trivial to detect mutation operations. At the supergraph_service stage the query plan doesn't exist yet. Options C and D are incorrect — supergraph_service can return errors, and mutations are a property of the incoming request, not the merged response.",
                    },
                    Question::MultipleChoice {
                        stem: "What does the `activate()` hook let a plugin do that `new()` cannot?",
                        options: [
                            "Read the supergraph SDL",
                            "Deserialize its YAML config",
                            "Take action only after all plugins have finished initializing",
                            "Register additional HTTP endpoints",
                        ],
                        answer: 2,
                        explanation: "activate() is called after every plugin's new() has completed successfully, signaling that the full plugin set is live. This is useful when a plugin needs to interact with another plugin or start background work that assumes all plugins are ready. new() runs per-plugin during the initialization phase, before any other plugin is guaranteed to be ready. SDL and config are both available in new() already. HTTP endpoints are registered via web_endpoints(), not activate().",
                    },
                ],
                engineer_note: None,
            },
        ],
    }
}

fn ch05() -> Chapter {
    Chapter {
        number: 5,
        title: "Customization Options",
        tagline: "Three ways to teach the router new tricks — pick the right tool for the job.",
        exercises: vec![
            Exercise {
                title: "The Three Paths",
                reading: r#"## Why Three Options?

One size rarely fits all. Apollo Router offers three
distinct ways to add custom logic, each targeting a
different set of tradeoffs: how fast it runs, who can
write it, and when changes take effect.

## Rhai Scripts

Rhai is a lightweight scripting language with syntax
that looks a lot like Rust. Scripts live on disk and
the router watches them — change a file and the router
picks it up without a restart. No recompile required.

The catch: Rhai is sandboxed. You can't use async/
await, can't import external crates, and can't make
network calls directly. It's ideal for lightweight
per-request logic like header manipulation or simple
request validation.

## Coprocessors

A coprocessor is your own HTTP service. The router
calls it at whichever pipeline stages you configure,
passing a JSON payload describing the current request
or response. Your service replies with mutations to
apply — add a header, reject the request, rewrite the
body.

The upside: write it in any language your team knows.
The downside: every call adds a network round-trip.

## Native Rust Plugins

Native plugins are compiled directly into the router
binary. They have zero per-request overhead and full
access to the Rust ecosystem, including async/await.
The tradeoff is that they require forking the router
or contributing code upstream — changes need a
recompile and a new binary deployment."#,
                questions: vec![
                    Question::MultipleChoice {
                        stem: "A team needs to add custom auth logic to the router. They write Python, not Rust, and they already have an auth service running. Which customization option fits best?",
                        options: [
                            "Native Rust plugin — zero overhead is always preferred",
                            "Rhai script — it's the simplest option",
                            "Coprocessor — the team writes Python and the auth service already exists",
                            "No customization needed — use Apollo's built-in auth",
                        ],
                        answer: 2,
                        explanation: "A coprocessor is the right fit here: the team doesn't write Rust, and because the logic needs to hit an external auth service anyway, the extra HTTP round-trip cost is already baked into the design. Native plugins require Rust and a recompile. Rhai is sandboxed and can't make network calls directly.",
                    },
                    Question::MultipleChoice {
                        stem: "Which of the following is a hard limitation of Rhai scripts in the router?",
                        options: [
                            "They can only run at the router stage, not subgraph stages",
                            "They cannot read request headers",
                            "They do not support async operations or external crate imports",
                            "They require a router restart to change configuration",
                        ],
                        answer: 2,
                        explanation: "Rhai runs in a sandboxed environment with no async support and no access to external crates. This keeps scripts safe and fast, but means anything requiring I/O or third-party libraries must move to a coprocessor or native plugin. Rhai scripts can run at all four pipeline stages and can read headers. Hot-reload means no restart is needed for script changes.",
                    },
                ],
                engineer_note: None,
            },
            Exercise {
                title: "Rhai Hot-Reload and the Script Lifecycle",
                reading: r#"## What Hot-Reload Means

Hot-reload means the router detects changes to your
Rhai script files and reloads them automatically —
no process restart, no traffic disruption. This makes
Rhai attractive for logic that needs to change
frequently: feature flags, header rewrites, or quick
fixes during an incident.

## How It Works

The router configuration points to a directory of
Rhai scripts:

    rhai:
      scripts: ./scripts/
      main: main.rhai

The router watches that directory. When a file
changes, it recompiles the script and starts using
the new version for subsequent requests. In-flight
requests finish with the old version.

## What Scripts Can Do

A Rhai script implements callback functions that the
router calls at each pipeline stage:

    fn supergraph_request(request) {
      request.headers["x-custom"] = "hello";
      request
    }

The script receives a request or response object,
can read and modify it, and returns the (possibly
modified) object. If it throws, the router can be
configured to fail open or fail closed.

## The Right Use Cases

Rhai shines for: adding or stripping headers,
rejecting requests that match a pattern, injecting
values into the request context, and lightweight
transformations that your team wants to control
without a deploy pipeline.

It is the wrong tool when you need to call out to
another service, do heavy computation, or use a
library not already embedded in the router."#,
                questions: vec![
                    Question::MultipleChoice {
                        stem: "A security engineer wants to add a header-stripping rule to the router and needs it to take effect within seconds of pushing a file change, without a router restart. Which option supports this workflow?",
                        options: [
                            "Native Rust plugin — fastest execution",
                            "Rhai script — hot-reloadable without restart",
                            "Coprocessor — most flexible language support",
                            "Router YAML config — headers are always configured there",
                        ],
                        answer: 1,
                        explanation: "Rhai scripts are hot-reloadable: the router watches the scripts directory and picks up changes automatically. Native plugins require a recompile and redeploy. Coprocessors are a separate service with their own deployment cycle. While some header behavior is configurable in YAML, arbitrary header-stripping logic requires a plugin of some kind.",
                    },
                    Question::CodeFind {
                        prompt: "Find the Rhai plugin's config struct. What field specifies the directory where Rhai scripts are loaded from?",
                        file_hint: "apollo-router/src/plugins/rhai/mod.rs",
                        accepted: &["scripts", "scripts: Option<PathBuf>"],
                        hint: "Look for `pub(crate) struct Conf` near the top of the file.",
                        explanation: "The `Conf` struct in rhai/mod.rs has a `scripts` field of type `Option<PathBuf>`. This is the directory the router watches for `.rhai` files. The companion field `main` names the entry-point file within that directory.",
                    },
                ],
                engineer_note: Some("apollo-router/src/plugins/rhai/mod.rs — the `execute` function around line 794 is where a compiled Rhai AST is actually called per-request. The `RhaiService` wrapper and the `map_request` / `map_response` helpers show how Rhai callbacks slot into the Tower service model."),
            },
            Exercise {
                title: "Coprocessors: Outsourcing Pipeline Logic",
                reading: r#"## The Coprocessor Model

A coprocessor is a plain HTTP service you run
alongside the router. You tell the router which
pipeline stages should call out to it:

    coprocessors:
      - url: http://localhost:8081
        router:
          request:
            headers: true
            body: false
        supergraph:
          response:
            body: true

At each configured stage, the router serializes the
relevant parts of the request or response into a JSON
payload, POSTs it to your service, and waits for a
response before continuing.

## What Your Service Can Return

Your service returns a JSON object describing what
should change. It can:
- Add, modify, or remove HTTP headers
- Replace the request or response body
- Return a `control: "break"` to short-circuit the
  pipeline and send an immediate response

This gives coprocessors the power to reject requests,
add authentication context, or transform responses —
all without touching the router binary.

## The Latency Tradeoff

Every coprocessor call is a synchronous HTTP
round-trip inside the request lifecycle. If your
coprocessor takes 10ms to respond, every request
passing through that stage pays 10ms. This is the
main reason to prefer Rhai for pure in-process logic.

The tradeoff flips when your logic already needs an
external call — for example, checking a token against
an auth service. In that case the coprocessor round-
trip doesn't add a new dependency; it just moves where
that call happens.

## Real-World Config

The `Conf` struct in the coprocessor plugin defines
the full configuration surface: a base URL, per-stage
overrides, timeout, and which fields to include in
each payload."#,
                questions: vec![
                    Question::CodeFind {
                        prompt: "Open the coprocessor plugin's main config struct. What is the name of the field that sets the default URL for all coprocessor calls?",
                        file_hint: "apollo-router/src/plugins/coprocessor/mod.rs",
                        accepted: &["url", "url: String"],
                        hint: "Search for `struct Conf` — it's annotated with `#[schemars(rename = \"CoprocessorConfig\")]`.",
                        explanation: "The `Conf` struct has a `url: String` field that serves as the default endpoint for all pipeline stages. Individual stages can override this with their own `url` field, but if they don't, the router uses the top-level `url`. This lets you point all stages at one service while routing specific stages elsewhere.",
                    },
                    Question::MultipleChoice {
                        stem: "A coprocessor is configured on the supergraph request stage. A downstream service it depends on goes down. What is the most important operational concern?",
                        options: [
                            "The router binary will crash and need to be restarted",
                            "All requests through that stage will be blocked until the coprocessor responds or times out",
                            "The router will automatically fall back to the Rhai script for that stage",
                            "Only POST requests will be affected; GET requests bypass coprocessors",
                        ],
                        answer: 1,
                        explanation: "Because the coprocessor call is synchronous within the request lifecycle, a slow or unresponsive coprocessor blocks every request at that stage until the configured timeout is reached. This is why the `timeout` field in `Conf` matters — and why coprocessors introduce a new reliability dependency. There is no automatic Rhai fallback; all three customization options are independent.",
                    },
                ],
                engineer_note: Some("apollo-router/src/plugins/coprocessor/mod.rs — the `Conf` struct around line 431 and `RouterStage` around line 667 show how per-stage configuration maps to the actual service wrapping. The `Externalizable` type in apollo-router/src/services/external.rs defines the JSON contract your coprocessor service must speak."),
            },
        ],
    }
}

fn ch06() -> Chapter {
    Chapter {
        number: 6,
        title: "Observability",
        tagline: "See inside every request with metrics, traces, and logs.",
        exercises: vec![
            Exercise {
                title: "The Three Pillars",
                reading: r#"## What Is Observability?

Observability means you can answer "what is my system doing
right now?" without guessing. For a router sitting in front
of multiple subgraphs, that's essential.

Apollo Router has first-class support for all three pillars
of observability via OpenTelemetry (OTEL):

  - Metrics — numeric measurements over time (counters,
    histograms). Answer questions like "how many requests
    per second?" or "what's the p99 latency?"
  - Traces — a record of one request's journey through
    the system. Answer "why was that operation slow?"
  - Logs — structured text events for debugging and audit
    trails.

## Where They Live

All three are configured under the telemetry section of
router.yaml:

    telemetry:
      exporters:
        metrics:
          prometheus:
            enabled: true
        tracing:
          otlp:
            endpoint: http://jaeger:4317

The router ships with exporters for OTLP (the OpenTelemetry
standard), Prometheus, Datadog, Jaeger, and Zipkin.

## Why It Matters

Without observability you're flying blind. With it, you can
detect N+1 subgraph calls, catch cache regressions, prove
SLA compliance, and root-cause incidents before customers
notice."#,
                questions: vec![
                    Question::MultipleChoice {
                        stem: "Which of the following best describes the difference between a metric and a trace?",
                        options: [
                            "Metrics measure memory; traces measure CPU.",
                            "A metric aggregates many requests into a number; a trace records the full journey of one request.",
                            "Traces are for errors only; metrics cover successful requests.",
                            "They are two names for the same thing in OpenTelemetry.",
                        ],
                        answer: 1,
                        explanation: "Metrics collapse many events into aggregate numbers (counts, histograms) that are cheap to store and query at scale. A trace captures the full, detailed journey of a single request — useful for debugging but expensive to keep for every request. Options A and C describe fictional distinctions; option D is incorrect because OTEL explicitly models them as separate signals.",
                    },
                    Question::MultipleChoice {
                        stem: "Where in router.yaml do you configure all three observability exporters?",
                        options: [
                            "Under the plugins section, one plugin per signal.",
                            "Under the telemetry section.",
                            "Under the supergraph section.",
                            "In a separate otel.yaml file that router.yaml references.",
                        ],
                        answer: 1,
                        explanation: "All telemetry configuration — metrics exporters, tracing exporters, logging format — lives under the top-level telemetry key in router.yaml. This keeps observability config in one predictable place. There is no separate otel.yaml file, and observability is not spread across the plugins section.",
                    },
                ],
                engineer_note: None,
            },
            Exercise {
                title: "Key Metrics to Know",
                reading: r#"## Built-In Metrics

The router emits a rich set of metrics out of the box. The
most important ones to know:

  apollo.router.operations
    Counts executed GraphQL operations. Carry attributes
    like graphql.operation.type and graphql.operation.name
    so you can filter by query vs. mutation.

  http.server.request.duration
    Histogram of router-level HTTP request latency. This
    is the number clients actually experience.

  apollo.router.operations.entity.cache
    Entity cache hit/miss rate. Watch this after enabling
    or tuning the entity cache.

  apollo.router.operations.fetch.duration
    Time spent on subgraph fetch operations — useful for
    spotting slow downstream services.

## Exporting to Prometheus

Prometheus is a popular open-source metrics backend that
works by scraping (pulling) metrics from targets on a
schedule, rather than having targets push data.

When you enable Prometheus in router.yaml, the router
exposes a /metrics HTTP endpoint. Your Prometheus server
scrapes that URL periodically and stores the data.

    telemetry:
      exporters:
        metrics:
          prometheus:
            enabled: true
            path: /metrics   # default

## Finding Metric Names in the Code

Metric names are defined as string constants throughout
the codebase. The file
apollo-router/src/plugins/telemetry/config_new/instruments.rs
is a good starting point."#,
                questions: vec![
                    Question::MultipleChoice {
                        stem: "When Prometheus is enabled, how does it collect metrics from the router?",
                        options: [
                            "The router pushes metrics to Prometheus on every request.",
                            "The router writes metrics to a file that Prometheus reads.",
                            "Prometheus scrapes (pulls) a /metrics endpoint exposed by the router.",
                            "The router streams metrics to Prometheus over a WebSocket.",
                        ],
                        answer: 2,
                        explanation: "Prometheus uses a pull model: it periodically sends an HTTP GET to the /metrics endpoint and collects the current metric snapshot. The router does not push data or write files. This pull model is a deliberate Prometheus design choice — it lets the scraper control timing and makes the router stateless with respect to metrics delivery.",
                    },
                    Question::CodeFind {
                        prompt: "In apollo-router/src/plugins/telemetry/config_new/instruments.rs, what is the string value of the constant HTTP_SERVER_REQUEST_DURATION_METRIC?",
                        file_hint: "apollo-router/src/plugins/telemetry/config_new/instruments.rs",
                        accepted: &["http.server.request.duration"],
                        hint: "Search for HTTP_SERVER_REQUEST_DURATION_METRIC with grep or open the file near line 116.",
                        explanation: "The constant is defined as \"http.server.request.duration\" — following the OpenTelemetry semantic conventions for HTTP server metrics. Using a shared constant (rather than a raw string repeated everywhere) ensures the metric name stays consistent across the codebase and is easy to find and update.",
                    },
                ],
                engineer_note: Some("apollo-router/src/metrics/mod.rs has a detailed doc comment explaining the metrics infrastructure and how to add a new metric — worth reading before instrumenting new code."),
            },
            Exercise {
                title: "Distributed Tracing",
                reading: r#"## What Is a Trace?

A trace is a tree of timed operations called spans. When a
GraphQL request enters the router, it creates a root span.
As the request flows through the pipeline, child spans are
created for each stage:

  router       — top-level HTTP handling
  supergraph   — GraphQL parsing and validation
  query_planning — building the execution plan
  execution    — coordinating subgraph calls
  subgraph     — one span per subgraph fetch

You can view this tree in any tracing backend (Jaeger,
Zipkin, Datadog APM, etc.) to see exactly where time was
spent.

## Trace Context Propagation

For a trace to be useful across services, each outbound
request must carry trace context — the trace ID and span
ID — so the receiving service can attach its own spans to
the same tree.

By default the router uses the W3C Trace Context standard
(https://www.w3.org/TR/trace-context/). This is the
industry default and uses two headers:

  traceparent  — carries trace ID, span ID, and flags
  tracestate   — optional vendor-specific metadata

Other formats (Jaeger, Datadog, Zipkin, AWS X-Ray) can be
enabled in the propagation section of router.yaml. Multiple
formats can be active at once.

## Sampling

Recording every span has cost. In high-traffic production
systems, trace sampling lets you capture a representative
fraction of requests. The router supports a configurable
sampling rate under telemetry.exporters.tracing.common."#,
                questions: vec![
                    Question::MultipleChoice {
                        stem: "Which header does the W3C Trace Context standard use to carry the trace ID and span ID between services?",
                        options: [
                            "X-B3-TraceId",
                            "X-Trace-Id",
                            "traceparent",
                            "uber-trace-id",
                        ],
                        answer: 2,
                        explanation: "The W3C Trace Context spec (https://www.w3.org/TR/trace-context/) defines the traceparent header, which encodes the trace ID, parent span ID, and sampling flags in a single value. X-B3-TraceId is from the older Zipkin B3 format; uber-trace-id is the Jaeger format; X-Trace-Id is not a standard header. The router defaults to W3C Trace Context because it is the vendor-neutral standard.",
                    },
                    Question::CodeFind {
                        prompt: "In apollo-router/src/plugins/telemetry/consts.rs, what is the string value of the constant EXECUTION_SPAN_NAME?",
                        file_hint: "apollo-router/src/plugins/telemetry/consts.rs",
                        accepted: &["execution"],
                        hint: "Open the file and look for EXECUTION_SPAN_NAME. It is near the other *_SPAN_NAME constants.",
                        explanation: "EXECUTION_SPAN_NAME is \"execution\" — the span created when the router coordinates all the subgraph fetch calls needed to fulfill a request. Knowing these span names lets you write precise queries in your tracing backend (e.g., filter for slow execution spans to find requests with expensive query plans).",
                    },
                ],
                engineer_note: Some("apollo-router/src/plugins/telemetry/consts.rs defines BUILT_IN_SPAN_NAMES, an array listing every span name the router creates. It is the canonical reference for what shows up in a trace."),
            },
        ],
    }
}

fn ch07() -> Chapter {
    Chapter {
        number: 7,
        title: "Backpressure & Load Management",
        tagline: "Fail fast at the edge, not deep in the stack.",
        exercises: vec![
            Exercise {
                title: "Why Backpressure Matters",
                reading: r#"## The Cascade Problem

When traffic spikes, the naive outcome is a cascade:
the router gets flooded, forwards everything to the
subgraphs, which also get flooded, and everything
fails together. This is especially painful in a
GraphQL federation setup because one client query
can fan out into N subgraph calls.

## Fail Fast at the Edge

The right strategy is to reject or slow down requests
at the router before the damage propagates. This is
called backpressure — the router pushes back against
incoming load rather than passing it downstream.

Think of it like a queue at a coffee shop. It is
better to tell the tenth customer "we are at capacity,
come back in five minutes" than to take every order,
run out of supplies, and fail to deliver any coffee.

## Apollo Router's Toolkit

The router gives you several tools for this:

- Traffic shaping: rate limits, timeouts, retries,
  deduplication — all per subgraph
- Demand control: reject expensive queries before
  they touch subgraphs
- Request limits: structural caps on query depth,
  aliases, and body size

Each layer catches a different class of problem.
Together they let you tune the router's behaviour
under load without changing your subgraph code."#,
                questions: vec![
                    Question::MultipleChoice {
                        stem: "A single GraphQL query fans out to 8 subgraph calls. The router starts rejecting requests at its rate limit. Which best describes the benefit of this?",
                        options: [
                            "Each subgraph can independently decide whether to serve the request",
                            "The router absorbs the spike and shields 8 subgraphs from traffic they cannot handle",
                            "Rejected requests are automatically retried with exponential backoff",
                            "Subgraphs receive the requests but respond with cached data instead",
                        ],
                        answer: 1,
                        explanation: "The router sits in front of all subgraphs, so rate limiting at the router multiplies the protection — one rejection at the edge prevents 8 subgraph calls. Options A and D describe subgraph-side behaviour, not the router's role. Option C (automatic retry) is a separate traffic-shaping feature, not the benefit of rejection.",
                    },
                    Question::MultipleChoice {
                        stem: "Which statement best describes why GraphQL federation makes backpressure more important than in a single REST API?",
                        options: [
                            "Federation uses a binary protocol that is harder to rate-limit",
                            "Each federated subgraph has its own independent rate limiter that ignores the router",
                            "One client query can generate N subgraph requests, so overload amplifies across the mesh",
                            "Federation caches all responses, so backpressure only matters on cache misses",
                        ],
                        answer: 2,
                        explanation: "Federation's query planning splits one operation into multiple subgraph fetches, so a traffic spike at the router becomes a larger spike across every subgraph involved. The other options describe things that are either false or unrelated to the amplification effect.",
                    },
                ],
                engineer_note: None,
            },
            Exercise {
                title: "Traffic Shaping Plugin",
                reading: r#"## What Traffic Shaping Does

The traffic shaping plugin lives at
apollo-router/src/plugins/traffic_shaping/
and wraps outbound subgraph calls with several
protective layers, all configurable per subgraph.

## Rate Limiting

You can cap the number of requests per second the
router sends to a subgraph. Excess requests receive
an HTTP 429 response immediately. Configuration:

    traffic_shaping:
      subgraphs:
        products:
          global_rate_limit:
            capacity: 500
            interval: 1s

## Timeouts

Set the maximum time the router waits for a subgraph
response. Requests that exceed the timeout return an
error to the client quickly rather than hanging.

## Retries

Transient failures (network blips, 5xx responses) can
be retried automatically. Jitter is added to spread
retries over time and avoid a "thundering herd" — the
situation where every caller retries at exactly the
same moment and amplifies the problem.

## Deduplication

If multiple in-flight requests to a subgraph are
identical, the router can send only one and fan the
single response back to all waiters. This is safe only
for read operations (queries), never for mutations
which have side effects. The implementation uses a
shared wait-map keyed on the request content."#,
                questions: vec![
                    Question::MultipleChoice {
                        stem: "Query deduplication in the traffic shaping plugin is intentionally disabled for mutations. Why?",
                        options: [
                            "Mutations use a different network protocol that does not support deduplication",
                            "Mutations have side effects, so two identical mutations must both execute — collapsing them would silently drop work",
                            "Mutations are always cached by the router, making deduplication redundant",
                            "The deduplication key does not include the mutation body, so matches would be unreliable",
                        ],
                        answer: 1,
                        explanation: "Deduplication works by sending one request and sharing its response with all identical waiters. For queries (reads) this is safe because the result is the same. For mutations, each call is intended to cause a side effect — a write, a charge, an email sent — so collapsing two calls into one would silently lose one of those effects. Options A, C, and D are all false.",
                    },
                    Question::CodeFind {
                        prompt: "The traffic shaping plugin defines a top-level configuration struct that holds per-router, per-subgraph, and per-connector shaping options. What is the name of that struct?",
                        file_hint: "apollo-router/src/plugins/traffic_shaping/mod.rs",
                        accepted: &["Config", "TrafficShapingConfig"],
                        hint: "Look for a pub(crate) struct near the top of mod.rs that has fields named `router`, `all`, `subgraphs`, and `connector`. Its schemars rename gives away the public-facing name.",
                        explanation: "The struct is named `Config` in code (with a schemars rename of `TrafficShapingConfig` for JSON Schema output). It holds an optional `RouterShaping`, an optional `all` SubgraphShaping that acts as a default, and a per-name `subgraphs` HashMap for overrides.",
                    },
                ],
                engineer_note: Some("apollo-router/src/plugins/traffic_shaping/deduplication.rs — the QueryDeduplicationService uses a broadcast channel wait-map: the first caller executes the subgraph request; late arrivals subscribe to the broadcast and receive the same response when it resolves."),
            },
            Exercise {
                title: "Demand Control & Request Limits",
                reading: r#"## Two Complementary Defences

The router has two plugins that reject requests
before they reach subgraphs, and they catch
different problems.

## Demand Control (plugins/demand_control/)

Demand control assigns a numeric cost to each
incoming GraphQL operation based on static analysis
of the query plan — no subgraph calls are made. The
cost model accounts for field weights and estimated
list sizes (e.g., a field returning a list of 100
items costs more than a scalar field).

If the estimated cost exceeds a configured maximum,
the router immediately returns an error. This stops
deeply nested list queries and expensive unions
from ever reaching subgraphs. The check happens
after parsing but before execution.

    demand_control:
      enabled: true
      mode: enforce
      strategy:
        static_estimated:
          list_size: 10
          max: 1000

## Request Limits (plugins/limits/)

Request limits are structural. They fire even earlier
— some checks happen before GraphQL parsing — and
catch abuse patterns like:

- Oversized HTTP request bodies (http_max_request_bytes)
- Operations that are too deeply nested (max_depth)
- Too many aliases in one query (max_aliases)
- Too many root fields (max_root_fields)

These do not know anything about cost or semantics.
They are simple numerical caps on the shape of the
request. Use them as the first line of defence."#,
                questions: vec![
                    Question::MultipleChoice {
                        stem: "A client sends a request body that is 50 MB. Which protection rejects it first, before any GraphQL parsing occurs?",
                        options: [
                            "Demand control, which estimates the cost of parsing a large body",
                            "The rate limiter, which counts bytes instead of requests",
                            "The http_max_request_bytes limit in the limits plugin, which rejects oversized bodies at the network layer",
                            "The max_depth limit, which detects deep nesting caused by large payloads",
                        ],
                        answer: 2,
                        explanation: "http_max_request_bytes in the limits plugin caps the raw HTTP body before parsing. Demand control (option A) requires a parsed and planned query to estimate cost. The rate limiter (option B) counts requests, not bytes. max_depth (option D) is a structural limit on the parsed query, not the body size.",
                    },
                    Question::Reflection {
                        prompt: "A user submits a deeply-nested query that asks for products, each with reviews, each with reviewer profiles, each with their order history — totalling an estimated cost of 5,000 against a configured max of 1,000. Walk through what happens to that request and what the user experiences.",
                        key_points: &[
                            "The query is parsed and a query plan is generated",
                            "Demand control runs static cost estimation on the plan before any subgraph calls are made",
                            "The estimated cost (5,000) exceeds the configured maximum (1,000)",
                            "The router returns a GraphQL error immediately — no subgraph is contacted",
                            "The user sees an error response (not a timeout), typically with an extension code indicating the cost limit was exceeded",
                            "Subgraphs are fully protected; they receive zero traffic from this request",
                        ],
                    },
                ],
                engineer_note: Some("apollo-router/src/plugins/demand_control/cost_calculator/static_cost.rs contains the StaticCostCalculator. It walks the query plan tree, multiplying field costs by estimated list sizes at each level, which is how deeply-nested list queries produce exponentially higher costs."),
            },
        ],
    }
}

fn ch08() -> Chapter {
    Chapter {
        number: 8,
        title: "Security",
        tagline: "Authentication, authorization, CORS, and CSRF — the router's front line.",
        exercises: vec![
            Exercise {
                title: "Authentication: Who Are You?",
                reading: r#"## The Router as Gatekeeper

The router is the single entry point for every GraphQL
request. That makes it the ideal place to verify identity
— before any query planning or subgraph fan-out happens.

## JWT Authentication

The most common mechanism is JSON Web Tokens (JWTs).
A JWT is a signed, self-contained token that carries
claims — facts about the caller such as user ID, roles,
or scopes. The caller puts it in the Authorization header:

    Authorization: Bearer eyJhbGci...

The router's authentication plugin fetches a JSON Web
Key Set (JWKS) from your auth server. A JWKS is just a
published set of public keys your auth server uses to
sign tokens. The router caches this key set and uses it
to verify every incoming token's signature.

## What Happens After Verification

On success, the decoded JWT claims are stored in the
request context under the key:

    apollo::authentication::jwt_claims

Downstream plugins and subgraphs can read these claims.
On failure, the router returns a 401 Unauthorized and
the request never reaches query planning.

## API Key Auth

For simpler cases, the router also supports API key
authentication: compare a header value against a list
of allowed keys. No cryptography required — just a
fast lookup. Useful for machine-to-machine traffic
where full JWT infrastructure is overkill."#,
                questions: vec![
                    Question::MultipleChoice {
                        stem: "At which stage of the router pipeline does JWT authentication run?",
                        options: [
                            "After query planning, so subgraphs can opt out",
                            "At the Router service stage — before GraphQL parsing begins",
                            "At the Execution service stage, per subgraph request",
                            "In the Supergraph service, after the operation is validated",
                        ],
                        answer: 1,
                        explanation: "The authentication plugin hooks into the Router service (router_service), which is the very first stage in the pipeline — before the request is parsed as GraphQL. This means an invalid token is rejected with a 401 before any query planning work is done, which is both correct and efficient. The other stages run later and assume identity has already been established.",
                    },
                    Question::CodeFind {
                        prompt: "The JWT claims are stored in the request context using a constant key string. What is that constant's value?",
                        file_hint: "apollo-router/src/plugins/authentication/mod.rs",
                        accepted: &[
                            "apollo::authentication::jwt_claims",
                            "APOLLO_AUTHENTICATION_JWT_CLAIMS",
                        ],
                        hint: "Look near the top of mod.rs for a pub(crate) const declaration.",
                        explanation: "The constant APOLLO_AUTHENTICATION_JWT_CLAIMS holds the string \"apollo::authentication::jwt_claims\". This is the context key under which decoded JWT claims are stored after successful verification. Other plugins — including the authorization plugin — read from this key to make access decisions downstream.",
                    },
                ],
                engineer_note: Some("apollo-router/src/plugins/authentication/mod.rs — the authenticate() function (line ~470) is the core JWT verification path; router_service() (line ~239) is where the middleware is wired in. The jwks.rs module handles key set fetching and caching."),
            },
            Exercise {
                title: "Authorization: What Can You Do?",
                reading: r#"## From Identity to Permission

Authentication answers "who are you?" Authorization
answers "what are you allowed to do?" The router handles
both, but they are separate plugins with separate jobs.

## Schema-Driven Authorization

The router's authorization model is unusual: policies
live in the GraphQL schema itself, not in application
code. You annotate fields and types with federation
directives:

    type Query {
      publicFeed: [Post]
      adminDashboard: Stats @authenticated
      exportData: File @requiresScopes(scopes: [["export"]])
    }

@authenticated means "the caller must have a valid JWT."
@requiresScopes means "the JWT must contain these OAuth
scopes." These directives are defined in the Apollo
Federation specification — they travel with your schema.

## Two Enforcement Modes

When the router detects an unauthorized access attempt
it can behave in one of two ways:

1. Reject — return a 401/403 and refuse the whole request
2. Filter — strip the unauthorized fields from the
   response and return partial data for the rest

The filter mode is especially powerful: a single query
can return public data to unauthenticated users while
silently omitting fields they aren't allowed to see.

## No Custom Code for Common Cases

For the vast majority of authorization needs, you write
directives in SDL (Schema Definition Language) and
configure the plugin in router.yaml. No Rhai scripts,
no custom plugins, no resolver-level checks needed."#,
                questions: vec![
                    Question::MultipleChoice {
                        stem: "Where are @requiresScopes and @authenticated directives defined?",
                        options: [
                            "In the router.yaml configuration file, under authorization:",
                            "In the client application, as part of the GraphQL query",
                            "In the GraphQL schema (SDL), as federation directives on fields and types",
                            "In each subgraph's resolver code, checked at runtime",
                        ],
                        answer: 2,
                        explanation: "@requiresScopes and @authenticated are federation directives applied directly to types and fields in the GraphQL schema SDL. This is what \"schema-driven authorization\" means — the policy is co-located with the type definition, not scattered across resolvers or config files. Clients don't control these directives, and subgraph resolver code doesn't need to re-implement them.",
                    },
                    Question::MultipleChoice {
                        stem: "When the authorization plugin is set to \"filter\" mode and a user lacks the required scope for one field, what happens?",
                        options: [
                            "The entire request is rejected with a 403 Forbidden",
                            "The unauthorized field is returned as null with no indication",
                            "The unauthorized field is stripped from the response; other fields are returned normally",
                            "The query is re-planned to route around the restricted subgraph",
                        ],
                        answer: 2,
                        explanation: "In filter mode, the router removes unauthorized fields from the query before execution and from the response before returning it to the client. The rest of the query succeeds normally. This lets a single schema serve both authenticated and unauthenticated users — the same query returns more or less data depending on the caller's scopes.",
                    },
                ],
                engineer_note: Some("apollo-router/src/plugins/authorization/scopes.rs contains ScopeFilteringVisitor, which walks the query AST and removes fields whose required scopes are absent from the JWT claims. The parallel authenticated.rs does the same for @authenticated fields."),
            },
            Exercise {
                title: "CORS and CSRF: Browser Security",
                reading: r#"## What CORS Actually Does

CORS stands for Cross-Origin Resource Sharing. It is a
browser security mechanism — the keyword is browser.

When JavaScript on https://app.example.com makes a
request to https://api.example.com, the browser first
asks the API server: "are you okay with requests from
app.example.com?" The server answers via response
headers like Access-Control-Allow-Origin.

The router handles this under the cors: key in
router.yaml. You can allow all origins with a wildcard,
list specific trusted origins, or configure which HTTP
methods and headers are accepted.

## The Critical Gotcha

CORS does not protect your API from server-to-server
requests. A backend service, curl, or any HTTP client
that is not a browser will simply ignore CORS headers
entirely. CORS is purely a browser enforcement
mechanism. If you need to restrict non-browser access,
you need authentication.

## CSRF Protection

CSRF (Cross-Site Request Forgery) is a different attack:
a malicious website tricks a user's browser into making
a request to your API using the user's existing cookies
or credentials — without the user knowing.

The router's CSRF protection uses the "custom request
header" pattern. A simple HTML form can submit
application/x-www-form-urlencoded without any preflight.
But a request with Content-Type: application/json or
any custom header (like Apollo-Require-Preflight) must
go through a CORS preflight — which a cross-site
attacker cannot forge. The router rejects requests that
look like they came from a simple cross-site form."#,
                questions: vec![
                    Question::MultipleChoice {
                        stem: "A security engineer says \"we have CORS configured, so we're protected from unauthorized API access.\" What is wrong with this statement?",
                        options: [
                            "Nothing — CORS fully protects the API from all unauthorized callers",
                            "CORS only protects GET requests; POST requests bypass it",
                            "CORS is enforced by browsers only; server-to-server and CLI tools ignore it entirely",
                            "CORS protects against read access but not write access",
                        ],
                        answer: 2,
                        explanation: "CORS is a browser-side enforcement mechanism. The browser checks CORS headers and refuses to expose the response to JavaScript from disallowed origins. But any HTTP client that is not a browser — curl, Postman, a backend service — simply ignores these headers and gets the full response. To protect against non-browser callers, you need authentication (JWTs or API keys).",
                    },
                    Question::CodeFind {
                        prompt: "The CSRF plugin checks whether a request is 'preflighted' before allowing it through. Find the function that performs this check.",
                        file_hint: "apollo-router/src/plugins/csrf/mod.rs",
                        accepted: &[
                            "is_preflighted",
                            "fn is_preflighted",
                        ],
                        hint: "Look for a standalone function (not a method) that takes a router::Request and a slice of required headers.",
                        explanation: "The is_preflighted() function (around line 147 in csrf/mod.rs) determines whether a request would have triggered a CORS preflight — meaning it has a non-simple Content-Type or a custom header. Requests that are not preflighted look like they could have come from a plain HTML form on any origin, so the CSRF plugin rejects them. This is the 'custom request header' CSRF defense pattern.",
                    },
                ],
                engineer_note: None,
            },
        ],
    }
}

fn ch09() -> Chapter {
    Chapter {
        number: 9,
        title: "GraphOS & Enterprise Features",
        tagline: "Schema delivery, persisted queries, and the license model",
        exercises: vec![
            Exercise {
                title: "The Schema Uplink",
                reading: r#"## How the Router Gets Its Schema

In a local setup, you point the router at a
supergraph SDL file on disk. In a managed
federation setup with GraphOS, you never touch
that file directly — the router fetches its
schema automatically from Apollo's cloud.

## The Uplink

The "uplink" is a hosted API at
uplink.api.apollographql.com. The router
authenticates with two environment variables:

    APOLLO_KEY=service:my-graph:abc123
    APOLLO_GRAPH_REF=my-graph@production

With those set, the router polls the uplink on
startup and then continuously in the background.
When a developer pushes a new subgraph schema
to GraphOS, GraphOS recomposes the supergraph
and the router picks up the new SDL on the next
poll — no restart required.

## What the Uplink Delivers

The uplink is not just for schemas. The same
channel delivers:

- The composed supergraph SDL
- Persisted query manifests
- License status and feature gates

The response includes a `min_delay_seconds`
field that tells the router how long to wait
before polling again. This lets Apollo tune
poll frequency server-side without a router
release.

## Fallback Endpoints

The router ships with two uplink endpoints
(GCP and AWS) configured as fallbacks. If the
first URL fails, the router tries the next.
You can inspect this in
apollo-router/src/uplink/mod.rs."#,
                questions: vec![
                    Question::MultipleChoice {
                        stem: "In a managed federation setup, how does the router receive an updated supergraph schema after a developer pushes a new subgraph?",
                        options: [
                            "The router watches a local SDL file for changes using filesystem events",
                            "GraphOS pushes the new schema directly to the router via a webhook",
                            "The router polls the uplink endpoint and receives the new SDL on the next successful poll",
                            "An operator must restart the router process to load the new schema",
                        ],
                        answer: 2,
                        explanation: "The router continuously polls the uplink endpoint. When GraphOS recomposes the supergraph after a subgraph push, the new SDL becomes available at the uplink and the router picks it up on its next poll without any restart. Webhooks and filesystem watches are not used; restarts are explicitly not required.",
                    },
                    Question::CodeFind {
                        prompt: "The uplink module defines two default endpoint URLs (GCP and AWS). What are they? Look for the constants GCP_URL and AWS_URL.",
                        file_hint: "apollo-router/src/uplink/mod.rs",
                        accepted: &[
                            "https://uplink.api.apollographql.com",
                            "https://aws.uplink.api.apollographql.com",
                            "uplink.api.apollographql.com",
                            "aws.uplink.api.apollographql.com",
                        ],
                        hint: "Search for `const GCP_URL` near the top of the file.",
                        explanation: "The router ships with GCP (uplink.api.apollographql.com) and AWS (aws.uplink.api.apollographql.com) fallback endpoints. If the primary endpoint is unreachable, the router automatically tries the secondary, giving resilience against regional outages.",
                    },
                ],
                engineer_note: Some("apollo-router/src/uplink/schema_stream.rs contains the SupergraphSdlQuery GraphQL client and the From impl that maps UplinkResponse variants (New / Unchanged / Error) into schema state changes. The min_delay_seconds field in the response is the key to understanding how Apollo controls poll frequency remotely."),
            },
            Exercise {
                title: "Persisted Queries",
                reading: r#"## Sending Queries by ID

Normally a GraphQL client sends the full query
text on every request:

    { "query": "query GetUser { user { name } }" }

Persisted queries (PQ) flip this around. The
client sends a short hash ID instead:

    { "extensions": { "persistedQuery": {
        "sha256Hash": "abc123..." } } }

The router looks up the full query text from a
manifest it received via the uplink.

## Why This Matters

**Smaller payloads.** Hash IDs are tiny compared
to complex queries. This matters for mobile
clients on slow connections.

**Per-operation controls.** Because every
operation has a stable ID, you can set rate
limits or monitor costs per operation ID rather
than parsing raw query text each time.

**Security — safelisting.** This is the big one.
In safelisting mode the router *rejects any
operation not in the manifest*. A client cannot
send an arbitrary query — only operations your
team explicitly registered are allowed. This
closes off a whole class of abuse.

## Community vs. Enterprise

Basic persisted query support (APQ — Automatic
Persisted Queries) is available to everyone.
Safelisting — the strict "reject unknown
operations" mode — is an enterprise feature
that requires a GraphOS subscription and a
valid license delivered via the uplink."#,
                questions: vec![
                    Question::MultipleChoice {
                        stem: "What is the primary security benefit of enabling persisted query safelisting on the router?",
                        options: [
                            "It encrypts query payloads in transit between client and router",
                            "It prevents clients from executing arbitrary queries not registered in the manifest",
                            "It automatically rate-limits all GraphQL operations to 100 requests per second",
                            "It validates that query variables match the expected GraphQL input types",
                        ],
                        answer: 1,
                        explanation: "Safelisting means the router will only execute operations whose hash appears in the approved manifest. Any query not registered — including exploratory or malicious queries — is rejected outright. Encryption, rate limiting, and variable validation are separate concerns that safelisting does not address.",
                    },
                    Question::MultipleChoice {
                        stem: "A client sends a request with only a persisted query hash ID (no query text). The router receives it but cannot find that hash in its manifest. In safelisting mode, what happens?",
                        options: [
                            "The router asks the client to resend the full query text",
                            "The router forwards the request to the subgraph to resolve it",
                            "The router rejects the request with an error",
                            "The router falls back to fetching the query from the GraphOS registry",
                        ],
                        answer: 2,
                        explanation: "In safelisting mode the manifest is the complete list of allowed operations. If the hash is not found, the operation is unknown and the router rejects it — there is no fallback to asking for query text or fetching it remotely. This strict rejection is exactly what makes safelisting a security control.",
                    },
                ],
                engineer_note: Some("The persisted queries manifest is fetched via apollo-router/src/uplink/persisted_queries_manifest_stream.rs using the same uplink polling infrastructure as the schema. The plugin that enforces safelisting lives in apollo-router/src/plugins/persisted_queries/."),
            },
            Exercise {
                title: "Community vs. Enterprise Licensing",
                reading: r#"## Two Tiers, One Binary

Apollo Router ships as a single binary under
the Apache 2.0 license. There is no separate
enterprise build. Instead, certain features
are gated behind a license check that the
router performs against data delivered by the
uplink.

## Community (Apache 2.0)

Available to everyone, no account required:

- Full query planning and execution
- Custom plugins and Rhai scripting
- Coprocessors
- OpenTelemetry, metrics, logging
- Basic persisted queries (APQ)
- Subgraph authentication (header passing)

## Enterprise (GraphOS Subscription)

Requires a valid license from Apollo:

- Persisted query safelisting
- Authorization with @requiresScopes and
  @authenticated directives
- Demand control (cost limiting)
- Managed federation with GraphOS

## Graceful Degradation

If a router running enterprise features loses
its license (e.g., subscription lapses), it
does not crash. Enterprise features are
quietly disabled and the router continues
serving traffic with community capabilities.

The license itself is a signed JWT delivered
via the uplink, not a local file. The router
periodically re-validates it. The enforcement
logic lives in
apollo-router/src/uplink/license_enforcement.rs."#,
                questions: vec![
                    Question::MultipleChoice {
                        stem: "If a router's enterprise license expires, what does the router do?",
                        options: [
                            "It shuts down immediately to prevent unlicensed use",
                            "It continues running but disables enterprise-only features",
                            "It switches to a read-only mode, returning cached responses only",
                            "It sends an alert to GraphOS and waits for license renewal before resuming",
                        ],
                        answer: 1,
                        explanation: "The router is designed for graceful degradation. When the license expires, enterprise features like safelisting and @requiresScopes authorization are disabled, but core query routing continues uninterrupted. This avoids an expired subscription causing a production outage.",
                    },
                    Question::CodeFind {
                        prompt: "The license enforcement logic lives in the uplink module. Which file specifically handles checking whether the current license permits a given feature?",
                        file_hint: "apollo-router/src/uplink/license_enforcement.rs",
                        accepted: &[
                            "license_enforcement.rs",
                            "apollo-router/src/uplink/license_enforcement.rs",
                            "src/uplink/license_enforcement.rs",
                        ],
                        hint: "Look inside apollo-router/src/uplink/ for a file whose name suggests it enforces license rules.",
                        explanation: "license_enforcement.rs contains the logic that reads the license JWT state and determines which enterprise features are permitted. It is separate from license_stream.rs (which handles fetching) and feature_gate_enforcement.rs (which handles feature flags), giving each concern its own file.",
                    },
                ],
                engineer_note: None,
            },
        ],
    }
}

fn ch10() -> Chapter {
    Chapter {
        number: 10,
        title: "Releases & LTS Policy",
        tagline: "When minors ship, when patches ship, what LTS means, and how to get code in.",
        exercises: vec![
            Exercise {
                title: "What Triggers Each Release Type",
                reading: r#"## Three Types of Releases

Apollo Router uses Semantic Versioning (x.y.z):

    2  .  15  .  3
    ^     ^    ^
    |     |    patch: bug fixes only
    |     minor: new features, backward-compatible
    major: breaking changes — requires team agreement

## When Does a Minor Ship?

A minor release (y bump) ships when the `.changesets/`
directory contains at least one `feat_` file. Minor
releases add new capabilities — new config options, new
plugin hooks, new built-in behaviors.

All new features land on `dev` first. Once the team
decides to cut a release, the changeset files are
scanned: any `feat_` present means the next release is
a minor. Minor releases ship roughly every one to two
weeks.

## When Does a Patch Ship?

A patch release (z bump) ships when there are only
`fix_`, `maint_`, `docs_`, `config_`, or `exp_` files
in `.changesets/` — no `feat_`. Patches fix bugs and
make internal improvements without adding features.

Patches can be cut as needed, especially for security
fixes, outside the normal schedule. A security patch
might ship the same week a minor shipped.

## When Does a Major Ship?

Only with explicit agreement from core team members.
The `breaking_` changeset prefix signals breaking
changes, but the version bump requires a deliberate
decision — not just accumulating changesets. Router is
currently in v2.x and major bumps are rare.

## The Development Branch

All work merges to `dev`. Releases are cut from `dev`.
Each release gets its own branch (e.g., `2.15.x`) used
for backporting patches to that line."#,
                questions: vec![
                    Question::MultipleChoice {
                        stem: "You merged a PR with changeset file `fix_query_plan_cache.md` to dev. The next release has no feat_ changesets. What kind of release will ship?",
                        options: [
                            "Minor — any merged changeset triggers a minor",
                            "Patch — only fix_, maint_, docs_, config_, exp_ files present",
                            "Major — cache changes are always breaking",
                            "No release — patches require explicit scheduling",
                        ],
                        answer: 1,
                        explanation: "The version bump is determined by the *highest* changeset prefix present. fix_ maps to a patch bump. Without a feat_ or breaking_ file, the next release is a patch. Patch releases can ship on the normal schedule or be accelerated for important fixes.",
                    },
                    Question::MultipleChoice {
                        stem: "A PM asks: 'When will my feat_ PR land in a release that operators can install?' What is the right answer?",
                        options: [
                            "Immediately — feat_ PRs trigger a release as soon as they merge",
                            "In the next patch, which ships daily",
                            "In the next minor release, which ships roughly every one to two weeks after feat_ changesets accumulate",
                            "Only in the next major version",
                        ],
                        answer: 2,
                        explanation: "feat_ changesets flow to the next minor release. The team cuts minors roughly every one to two weeks as changes accumulate on dev. The PM can track the .changesets/ directory or CHANGELOG drafts to see when a feature is expected to ship.",
                    },
                ],
                engineer_note: Some(r#"RELEASE_CHECKLIST.md §'Pick a version' has the exact decision logic the release engineer uses. The `cargo xtask release status` command shows what's in-flight. `cargo xtask changeset create` is the right tool to generate your changeset file — it pre-fills from your PR title and body."#),
            },
            Exercise {
                title: "Getting Your Code Into a Release",
                reading: r#"## Every Change Needs a Changeset

Before a PR can ship, it needs a corresponding file in
the `.changesets/` directory. This is how changes get
into the CHANGELOG and how the release tooling knows
what version bump to apply.

## Creating a Changeset

    cargo xtask changeset create

Run this from the repo root. It prompts you for the
type of change and pre-fills a template from your PR
title and body. Commit the generated file with your PR.

## Choosing the Right Prefix

    feat_     New user-visible feature         -> minor
    fix_      Bug fix                          -> patch
    breaking_ Breaking change                  -> major (needs agreement)
    config_   New configuration option         -> patch
    maint_    Internal refactor, no user impact -> patch
    docs_     Documentation only               -> patch
    exp_      Experimental/unstable feature    -> patch

The file body must include: a brief title sentence, a
description, a link to the GitHub issue, and a link to
the PR.

## Targeting a Patch for an LTS Line

If a bug fix needs to go into an LTS line (e.g.,
`2.10.x`) rather than just the latest, that requires a
separate backport PR against the LTS branch. The process:
open a PR against `2.10.x`, include the fix and its
changeset, and get it reviewed. The LTS branch then
gets its own patch release (e.g., 2.10.9).

This is distinct from merging to `dev`. A fix on `dev`
does not automatically appear in older LTS lines."#,
                questions: vec![
                    Question::MultipleChoice {
                        stem: "An engineer fixes a critical authentication bypass bug. The fix should ship to both the current latest release line AND the active LTS line (2.10.x). How many PRs does this require?",
                        options: [
                            "One PR to dev — the release tooling backports automatically",
                            "Two PRs — one to dev (for the latest line) and one to the 2.10.x branch (for LTS)",
                            "Three PRs — dev, main, and the LTS branch",
                            "One PR to main — main is always the target for security fixes",
                        ],
                        answer: 1,
                        explanation: "Backporting to an LTS line is manual. A fix merged to dev goes into the next release from dev. To also land it on 2.10.x, a separate PR must target that branch. Security fixes are common candidates for this double-PR workflow.",
                    },
                    Question::CodeFind {
                        prompt: "Open .changesets/README.md. What prefix should you use for a change that is a new user-visible configuration option (not a fix, not a new feature per se, just a new knob)?",
                        file_hint: ".changesets/README.md",
                        accepted: &["config_", "config"],
                        hint: "The README lists a prefix specifically for new configuration options, distinct from feat_ and fix_.",
                        explanation: "The `config_` prefix is for new configuration options. It produces a patch bump, because adding a config option with a sensible default is backward-compatible — existing deployments are unaffected. Use `feat_` for features that are more behaviorally significant.",
                    },
                ],
                engineer_note: None,
            },
            Exercise {
                title: "LTS Lines and Upgrade Decisions",
                reading: r#"## What Is an LTS Line?

An LTS (Long-Term Support) line is a specific x.y
version — for example, `2.10.x` — that the team
commits to patching beyond the normal support window.

## Normal Support vs LTS

Without LTS: a version is supported until the next
version ships. If you are on 2.14.2 and 2.15.0 ships,
the team's focus moves to 2.15.x. 2.14.x gets no more
patches.

With LTS: the x.y line continues to receive security
patches and critical bug fixes even as the tip of
development moves forward. You might be on 2.10.8
while the latest is 2.17.0, and still receive important
security fixes on 2.10.x.

## Who Should Use LTS?

Pin to an LTS line if your team cannot upgrade
frequently — large enterprises, regulated environments,
or slow change-management cycles. You get stability
and critical fixes without tracking the latest minor.

Upgrade regularly if you want new features and can
accept the operational cost. You stay close to the
leading edge and avoid accumulating upgrade debt.

## Planning Around LTS

LTS designations are called out in release notes and
in RELEASE_CHECKLIST.md. When planning feature work,
consider: does this need to ship on an active LTS line,
or just on the latest? The answer affects whether a
backport PR is needed.

## Upgrading Across Multiple Minors

When jumping several minor versions, read CHANGELOG.md
for each release in the range. Deprecation notices in
earlier releases often become removals in later ones.
Never skip the CHANGELOG when crossing a boundary of
more than a couple of minors."#,
                questions: vec![
                    Question::MultipleChoice {
                        stem: "What does an LTS designation guarantee for a Router release line like 2.10.x?",
                        options: [
                            "New features will be backported to that line indefinitely",
                            "Security and critical bug fixes will continue to be backported even after newer versions ship",
                            "That line will never reach end-of-life",
                            "All releases on that line are free of breaking config changes",
                        ],
                        answer: 1,
                        explanation: "LTS commits to security and critical bug fix backports for a specific x.y line. It does not promise new features, indefinite support, or guaranteed freedom from behavioral changes. Non-LTS lines are unsupported once the next version ships.",
                    },
                    Question::Reflection {
                        prompt: "A customer is running Router 2.10.4 (LTS) in production. The latest is 2.15.1. They want the new demand-control feature (shipped in 2.13.0) but their change-management process is slow. What would you advise?",
                        key_points: &[
                            "2.10.x is LTS — they are still receiving critical/security patches, so staying put is defensible for stability",
                            "Demand control is a feature, not a fix — it will not be backported to 2.10.x LTS",
                            "To get demand control, they must upgrade to at least 2.13.0",
                            "Recommend reading the CHANGELOG for every release between 2.10.4 and their target version",
                            "A staged upgrade (e.g., 2.10 -> 2.12 -> 2.15) reduces the diff size at each step and surfaces issues incrementally",
                        ],
                    },
                ],
                engineer_note: None,
            },
        ],
    }
}

fn ch11() -> Chapter {
    Chapter {
        number: 11,
        title: "Testing the Router",
        tagline: "In-memory harnesses, mock subgraphs, and integration suites.",
        exercises: vec![
            Exercise {
                title: "The TestHarness",
                reading: r#"## What Is a Test Harness?

A test harness is scaffolding that lets you run code
under controlled conditions without the full production
setup. The Router ships one built in.

`TestHarness` lives in
`apollo-router/src/test_harness.rs`. It runs the
complete router pipeline — query planning, plugin
middleware, execution — entirely in memory. No HTTP
server is started. No real network calls happen.

## Building One

The harness uses the builder pattern:

    TestHarness::builder()
        .configuration_json(config)?
        .build_router()
        .await?

You call `.builder()` to get a fresh instance, chain
configuration and plugin options, then `.build_router()`
(or `.build_supergraph()`) to produce a Tower service
you can send requests through.

## Subgraph Calls

By default the harness never makes real network calls to
subgraphs. Instead it uses "canned" responses — static
data baked into the test schema. If you need custom
subgraph responses you can supply a `MockSubgraph`.

This design means plugin unit tests run fast (no Docker,
no external services) and are deterministic. The
integration test suite — in `apollo-router/tests/
integration/` — is where real network traffic is tested.

## Sending a Request

Once built, the harness is a Tower service. Use
`.oneshot(request)` to send a single request and get a
response back, then inspect the body or headers."#,
                questions: vec![
                    Question::MultipleChoice {
                        stem: "By default, what does TestHarness do when the router needs a subgraph response?",
                        options: [
                            "It sends real HTTP requests to any subgraph URL in the schema",
                            "It returns empty data for every subgraph call",
                            "It uses canned (static) responses and never makes network calls",
                            "It panics unless you provide a live subgraph URL",
                        ],
                        answer: 2,
                        explanation: "TestHarness is explicitly documented as never making network requests to subgraphs unless `.with_subgraph_network_requests()` is called. It falls back to canned static data, keeping tests fast and offline. Option A describes normal production behavior; option B is wrong because canned responses contain real-looking data; option D is wrong — the harness works fine without any URLs.",
                    },
                    Question::CodeFind {
                        prompt: "In apollo-router/src/test_harness.rs, what is the name of the method you call on TestHarness to start the builder pattern (i.e., get a new blank harness)?",
                        file_hint: "apollo-router/src/test_harness.rs",
                        accepted: &["builder", "builder()"],
                        hint: "Look for a pub fn that returns Self and takes no arguments on the TestHarness impl block.",
                        explanation: "The `builder()` associated function (line ~106) constructs a TestHarness with all fields set to None/false defaults. It follows the builder pattern — each subsequent method call refines the harness before the final `.build_router()` or `.build_supergraph()` call assembles the actual Tower service.",
                    },
                ],
                engineer_note: Some("apollo-router/src/test_harness.rs — study `extra_plugin()` and `router_hook()` / `supergraph_hook()` / `subgraph_hook()` methods. These let you inject either a fully-instantiated Plugin struct or a bare closure, giving you two clean patterns for testing plugin behavior without touching configuration YAML."),
            },
            Exercise {
                title: "Writing a Plugin Test",
                reading: r#"## The Typical Pattern

Testing a plugin follows a short recipe:

1. Register the plugin (usually via `register_plugin!`
   in a `#[cfg(test)]` block or using
   `.extra_plugin()` on the harness).
2. Build a `TestHarness` with your plugin loaded.
3. Send a GraphQL operation.
4. Assert on the response body or headers.

## Registering a Plugin for Tests

When you want to test an in-crate plugin without
publishing it to the global registry, the harness's
`.extra_plugin(my_plugin_instance)` method is the
easiest path. It accepts any value that implements
the `Plugin` trait:

    let harness = TestHarness::builder()
        .extra_plugin(MyPlugin::new(config))
        .build_supergraph()
        .await?;

## Asserting on Responses

Responses come back as a `graphql::Response` struct.
You can check `response.data`, `response.errors`, or
inspect HTTP-level headers through the service response
wrapper.

## Snapshot Testing with insta

Many router tests use the `insta` crate for snapshot
assertions. Instead of hand-writing expected JSON, you
call `insta::assert_json_snapshot!(response.data)`.
On first run insta writes the snapshot file. On future
runs it compares against the saved file — any change
fails the test until you explicitly accept the new
snapshot with `cargo insta review`.

Snapshot files live next to the test file in a
`snapshots/` directory and are committed to git."#,
                questions: vec![
                    Question::MultipleChoice {
                        stem: "When you use `insta::assert_json_snapshot!` in a router plugin test for the first time, what happens?",
                        options: [
                            "The test fails immediately because no snapshot file exists yet",
                            "Insta writes a new snapshot file and the test passes; future runs compare against it",
                            "Insta sends the snapshot to a remote review server for approval",
                            "The test is silently skipped until you create the snapshot file manually",
                        ],
                        answer: 1,
                        explanation: "On the first run insta creates a `.snap` file in the snapshots/ directory next to your test and the test passes. Subsequent runs diff the output against that file. If the output changes the test fails and you run `cargo insta review` to accept or reject the change. Options A, C, and D all misrepresent insta's behavior — it is designed to make the first-run experience frictionless.",
                    },
                    Question::MultipleChoice {
                        stem: "You want to test a plugin that adds a custom response header. Which TestHarness method lets you inject your already-constructed plugin instance without touching any YAML configuration?",
                        options: [
                            ".configuration_json() with the plugin name in the plugins block",
                            ".extra_plugin(my_plugin_instance)",
                            ".with_subgraph_network_requests()",
                            ".schema() with the plugin embedded as a directive",
                        ],
                        answer: 1,
                        explanation: "`.extra_plugin()` accepts any value implementing `Plugin` and appends it to the pipeline after configuration-driven plugins. This is the idiomatic way to inject a pre-constructed instance in tests. `.configuration_json()` works but requires serializable config. `.with_subgraph_network_requests()` controls network behavior, not plugins. GraphQL schemas do not embed plugin code.",
                    },
                ],
                engineer_note: None,
            },
            Exercise {
                title: "Integration Tests",
                reading: r#"## Beyond Unit Tests

Plugin unit tests with TestHarness are fast and cover
most logic. But some behaviors only emerge end-to-end:
cache warming across Redis restarts, distributed trace
propagation through real Zipkin, auth flows that touch
a live upstream.

For these, the router has a dedicated integration test
suite at `apollo-router/tests/integration/`.

## How They Work

Integration tests spin up real external services via
Docker Compose. Common dependencies include:

- Redis — for entity cache and query deduplication
- Zipkin or Datadog agent — for tracing assertions
- A mock subgraph container

To run the suite:

    cargo nextest run --test integration_tests

The `DEVELOPMENT.md` file in the repo root documents
the full Docker Compose setup and any environment
variables needed.

## Enterprise-Gated Tests

Some integration tests exercise features that require a
GraphOS license. These tests call a helper — typically
`graph_os_enabled()` — and skip themselves when the
`TEST_APOLLO_KEY` environment variable is not set.
This means the CI job for community contributors skips
enterprise tests automatically; internal CI sets the
key and runs everything.

## When to Write Each Kind

Use TestHarness for plugin logic, header manipulation,
and response shaping — anything you can verify without
external state. Use integration tests for persistence,
telemetry pipelines, and multi-service interactions
where mocks would give false confidence."#,
                questions: vec![
                    Question::MultipleChoice {
                        stem: "A new integration test exercises the entity cache backed by Redis. A developer without Docker installed tries to run it. What is the most likely outcome?",
                        options: [
                            "The test compiles but panics with a connection refused error at runtime",
                            "The test is automatically skipped because the test harness detects missing Docker",
                            "The router falls back to an in-memory cache so the test still passes",
                            "The test refuses to compile unless a Redis feature flag is enabled",
                        ],
                        answer: 0,
                        explanation: "Integration tests connect to real services. If Redis is not running the connection attempt fails at runtime — typically with a connection refused or timeout error — and the test panics or errors. The harness does not auto-detect Docker. There is no transparent in-memory fallback for Redis in integration tests. Compilation is not affected by runtime service availability.",
                    },
                    Question::CodeFind {
                        prompt: "Look inside apollo-router/tests/integration/. What is the name of the Rust source file that contains integration tests specifically for entity caching behavior?",
                        file_hint: "apollo-router/tests/integration/",
                        accepted: &["entity_cache.rs", "entity_cache"],
                        hint: "List the files in apollo-router/tests/integration/ and look for one whose name matches the caching feature being tested.",
                        explanation: "apollo-router/tests/integration/entity_cache.rs contains integration tests for the entity cache feature. These tests spin up Redis and real subgraph mocks to verify cache hit/miss behavior, TTL expiry, and invalidation — things that cannot be meaningfully tested with in-memory mocks alone.",
                    },
                ],
                engineer_note: None,
            },
        ],
    }
}

fn ch12() -> Chapter {
    Chapter {
        number: 12,
        title: "Apollo Connectors",
        tagline: "Expose REST APIs as subgraphs — no separate server required.",
        exercises: vec![
            Exercise {
                title: "What Are Connectors?",
                reading: r#"## The Problem Connectors Solve

Building a federated subgraph normally means running a
GraphQL server. Your team writes resolvers, maps fields
to database calls or REST responses, and operates
another service. That's real work for every API you
want to include in the graph.

Apollo Connectors are a shortcut for REST APIs: instead
of writing a subgraph server, you describe the REST
mapping directly in the schema using the `@connect`
directive. The router handles the HTTP calls itself.

## How It Works

A connector-enabled subgraph schema looks like this:

    type Query {
      product(id: ID!): Product
        @connect(
          source: "products-api"
          http: { GET: "/products/{$args.id}" }
          selection: "id name price"
        )
    }

The `@connect` directive tells the router:
- Which HTTP source to call (defined separately)
- Which endpoint and method to use
- Which JSON fields from the response to map to
  GraphQL fields (the `selection`)

The router executes these calls directly during query
plan execution — the same way it calls subgraphs, but
without a GraphQL server in between.

## What You Still Need

Connectors require:
1. A REST API that the router can reach
2. A schema with `@connect` directives describing
   the mapping
3. The connectors plugin enabled (it is by default)

You do NOT need a separate subgraph server, a GraphQL
wrapper, or any additional deployment.

## The Tradeoff

Connectors are excellent for simple field-to-endpoint
mappings. Complex logic — joins across multiple REST
calls, custom authorization per field, stateful
behavior — is better handled in a real subgraph server.
Connectors and traditional subgraphs can coexist in the
same supergraph."#,
                questions: vec![
                    Question::MultipleChoice {
                        stem: "A team wants to expose their existing REST product catalog API in the graph. They have no Rust or Node.js engineers available. What is the right tool?",
                        options: [
                            "Write a native Rust plugin that makes HTTP calls to the REST API from within the router",
                            "Use Apollo Connectors — define @connect directives in the schema, no subgraph server needed",
                            "Write a coprocessor in Python that proxies REST responses as GraphQL",
                            "Manually convert the REST API to GraphQL by implementing resolvers in the router binary",
                        ],
                        answer: 1,
                        explanation: "Connectors are designed for exactly this use case: expose a REST API in the graph without writing a server. The team only needs to write a schema with @connect directives pointing at REST endpoints. A native plugin requires Rust and recompilation; a coprocessor is operationally heavier; neither is necessary here.",
                    },
                    Question::MultipleChoice {
                        stem: "A connector's `selection` field maps a REST response to GraphQL fields. What does it do when the REST response contains a JSON field not listed in the selection?",
                        options: [
                            "The router panics and rejects the response",
                            "The field is included in the GraphQL response anyway",
                            "The unmapped field is silently ignored — only selected fields are returned",
                            "The router logs a warning and retries the request",
                        ],
                        answer: 2,
                        explanation: "The selection acts as a projection — it describes exactly which fields from the JSON response to include in the GraphQL response. Fields present in the REST response but absent from the selection are ignored. This is by design: the GraphQL schema is the contract, not the REST response shape.",
                    },
                ],
                engineer_note: Some("The connector plugin lives in apollo-router/src/plugins/connectors/. The core HTTP call logic is in make_requests.rs and handle_responses.rs. The @connect directive is defined and validated in the apollo-federation crate — the router's query planner generates connector-specific plan nodes that the connectors plugin executes."),
            },
            Exercise {
                title: "Connectors vs Subgraphs vs Coprocessors",
                reading: r#"## Three Ways to Bring Data Into the Graph

By now you have seen three extension mechanisms. This
exercise puts them side by side.

## Connectors

Best for: REST APIs with predictable field mappings.
Language: schema directives — no code.
Hot-reload: yes, schema changes are picked up via uplink.
Latency overhead: none beyond the REST call itself.
Limits: no complex logic, no async fan-out per field,
no access to the full plugin API.

## Coprocessors

Best for: logic in any language, or logic that calls
external services the router can't reach natively.
Language: anything with an HTTP server.
Hot-reload: partial — the coprocessor URL is config.
Latency overhead: one extra HTTP round-trip per stage
where the coprocessor is invoked.
Limits: the coprocessor's availability becomes a
dependency of the router's availability.

## Native Rust Plugins

Best for: high-performance, high-volume logic compiled
into the binary.
Language: Rust only.
Hot-reload: no — requires recompilation and redeploy.
Latency overhead: essentially zero (in-process).
Limits: requires Rust expertise, a fork or contribution
to the router repo, and a redeploy for every change.

## The Decision Tree

1. Is this mapping a REST API to GraphQL fields with
   simple selection logic?
   -> Connectors.

2. Is this cross-cutting logic (auth, rate-limit, logging)
   your team can write in Rust?
   -> Native plugin.

3. Is this logic in a language you already have, or does
   it need to call an external system?
   -> Coprocessor.

4. Is this a one-off Rhai script for lightweight per-
   request transformation?
   -> Rhai (see Chapter 5)."#,
                questions: vec![
                    Question::MultipleChoice {
                        stem: "Your team needs to add per-request audit logging that writes to an internal compliance service. The compliance service has a REST API and the team writes Python. Which approach fits best?",
                        options: [
                            "Connectors — use @connect to map the compliance REST API into the graph",
                            "Native Rust plugin — write the HTTP call in Rust for maximum performance",
                            "Coprocessor — write a Python HTTP service that the router calls at the Router stage",
                            "Rhai script — write a script that calls the compliance REST API from within the router",
                        ],
                        answer: 2,
                        explanation: "Connectors are for exposing REST APIs as GraphQL data, not for side effects like audit logging. A native Rust plugin requires Rust expertise and recompilation. Rhai scripts are sandboxed and cannot make arbitrary async HTTP calls. A Python coprocessor is the right fit: the team uses Python, it calls the compliance REST API, and it integrates via the coprocessor hook at the Router stage.",
                    },
                    Question::CodeFind {
                        prompt: "Open apollo-router/src/plugins/connectors/plugin.rs. What is the name of the struct that implements the Plugin trait for the connectors feature?",
                        file_hint: "apollo-router/src/plugins/connectors/plugin.rs",
                        accepted: &["Connectors", "connectors"],
                        hint: "Look for a struct definition followed by `impl Plugin for ...`.",
                        explanation: "The `Connectors` struct implements the Plugin trait. It intercepts the execution_service hook to inject connector plan execution — replacing what would normally be subgraph HTTP calls with direct REST calls driven by the @connect directive annotations in the query plan.",
                    },
                ],
                engineer_note: Some("The connectors plugin intercepts at the execution stage via execution_service(). When the query planner encounters a @connect-annotated field, it generates a ConnectorPlanNode rather than a FetchNode. The connectors plugin recognizes these nodes and dispatches HTTP requests directly, bypassing the normal SubgraphService pipeline."),
            },
        ],
    }
}
