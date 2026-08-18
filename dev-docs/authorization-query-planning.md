# Authorization and query planning

Scratch notes for ROUTER-1973 (separate authorization from query planning).
The before section describes `dev-v3.x` at `639c65a88`; the after section describes the
branch as implemented. File references are `file:line` on the branch.

## Before

```mermaid
sequenceDiagram
    autonumber
    participant C as Client
    participant PQ as ParseQueryLayer
    participant EX as ExtractAuthChecksLayer
    participant PL as Auth / policy plugins
    participant SG as SupergraphService
    participant CQP as CachingQueryPlanner
    participant QPS as QueryPlannerService
    participant ES as ExecutionService
    participant SUB as Subgraphs

    C->>PQ: POST /graphql
    PQ->>EX: parse, ParsedDocument in context
    Note over EX: generate_cache_metadata<br/>= what the document REQUIRES
    EX->>PL: request
    Note over PL: authentication sets JWT claims<br/>policies resolve to true or false
    PL->>SG: supergraph Request
    SG->>CQP: CachingRequest
    Note over CQP: update_cache_key<br/>= what the request was GRANTED
    CQP->>CQP: key on ORIGINAL text + metadata

    alt cache miss
        CQP->>QPS: QueryPlannerRequest
        QPS->>QPS: filter_query
        alt document emptied, or reject_unauthorized
            QPS-->>CQP: QueryPlannerContent::Response
            Note right of QPS: the planner ANSWERS the request
        else filtering succeeded
            QPS-->>CQP: QueryPlannerContent::Plan
        end
        CQP->>CQP: cache either variant
    end

    alt Response variant
        SG-->>C: 200, data null and errors
    else Plan variant
        SG->>ES: execution Request
        ES->>SUB: fetches
        Note over ES: errors, then format twice:<br/>FILTERED shape, ORIGINAL shape
        ES-->>C: 200, nulls and authorization errors
    end
```

## After

The planner plans or errors. The decision to refuse lives on the execution service.

```mermaid
sequenceDiagram
    autonumber
    participant C as Client
    participant SG as SupergraphService
    participant CQP as CachingQueryPlanner
    participant QPS as QueryPlannerService
    participant AL as AuthorizationLayer (execution)
    participant ES as ExecutionService
    participant SUB as Subgraphs

    C->>SG: request (parse and auth extraction as before)
    SG->>CQP: CachingRequest
    CQP->>CQP: key on ORIGINAL text + metadata

    alt cache miss
        CQP->>QPS: QueryPlannerRequest
        QPS->>QPS: filter_query, no reject_unauthorized input
        alt FilterResult::Emptied
            QPS-->>CQP: Plan with root None,<br/>usage reporting from the ORIGINAL query
        else Filtered or Unchanged
            QPS-->>CQP: Plan
        end
        CQP->>CQP: cache the plan
    end
    Note over CQP: usage reporting inserted for EVERY plan,<br/>so refusals reach Studio attributed

    CQP-->>SG: Plan
    SG->>AL: execution Request
    Note over AL: refuse when reject_unauthorized<br/>(held from config) and paths are present
    alt refused
        AL-->>C: 200, data null and errors,<br/>logs Authorization error once,<br/>inside the EXECUTION span
    else continue
        AL->>ES: request (authorization counter fires here)
        ES->>SUB: fetches from the filtered plan, none for an empty one
        Note over ES: errors, then format twice:<br/>FILTERED shape, ORIGINAL shape
        ES-->>C: 200, nulls and authorization errors
    end
```

Key references, on the branch:

| What | Where |
| --- | --- |
| `FilterResult` (Unchanged / Filtered / Emptied) | `plugins/authorization/mod.rs:152` |
| `filter_query`, no config-driven refusal | `plugins/authorization/mod.rs:371` |
| The layer: `checkpoint_async` ahead of the counter | `plugins/authorization/mod.rs:629` |
| Planner's `Emptied` arm, empty plan | `query_planner/query_planner_service.rs:465` |
| Single-variant `QueryPlannerContent` | `services/query_planner.rs:103` |
| Two-pass formatting (unchanged) | `services/execution/service.rs:291` |

Observable changes, all in the changesets: refusals reach Studio attributed, the
`Authorization error` event moves from `query_planning` to under `execution`, and an
emptied operation without `reject_unauthorized` returns shaped data with null roots
instead of `data: null` (the breaking changeset).

## Known gap, pinned but unresolved: multi-operation documents

`Emptied` means the *document* emptied, not the executed operation. `filter_query`
checks `filtered_doc.definitions.is_empty()` after each directive stage, and
definitions include every operation and fragment in the document, executed or not.

The consequence, measured on the branch (`orga.id` is `@authenticated`, request is
unauthenticated, directives enabled):

```graphql
# operationName: "A"
query A { orga(id: 1) { id } }
query B { currentUser { name } }
```

Filtering empties `A` and removes it from the document. `B` keeps the document
non-empty, so `filter_query` reports `Filtered`, not `Emptied`. The planner then looks
up operation `A` in a document that no longer contains it:

```
400 {"errors":[{"message":"Unknown operation named \"A\"",
     "extensions":{"code":"GRAPHQL_UNKNOWN_OPERATION_NAME"}}]}
```

Send `query A` alone and the same refusal produces
`200 {"data":null,"errors":[{...,"code":"UNAUTHORIZED_FIELD_OR_TYPE"}]}`. The same
authorization outcome yields two different statuses, two different error codes, and in
the multi-operation case tells the client their operation does not exist rather than
that it was refused.

The three `FIXME`s in `filter_query` mark the check
(`consider only filtered_doc.operations.get(key.operation_name)?`). Resolving them —
checking emptiness of the executed operation instead of the document — would fold this
case into the ordinary refusal path. That is a behaviour change: the 400 becomes the
refusal response, so it belongs to the spec-compliance follow-up, which is deciding
what the refusal response is anyway.

Pinned by `fully_filtered_operation_beside_surviving_sibling_reports_unknown_operation`
in `plugins/authorization/tests.rs`, which asserts the 400, the error code, the absent
`data` key, and that no subgraph was contacted.

### Cross-operation effects, measured

Unauthorized paths accumulate document-wide, so a sibling operation affects the
executed one. With `A` fully authorized and executed, and `B` asking for an
`@authenticated` field:

```
reject_unauthorized on:  200 {"data":null, "errors":[{...,"path":["orga","id"]}]}
reject_unauthorized off: 200 {"data":{"currentUser":{"name":"Ada"}},
                              "errors":[{..., no path}]}
```

With `reject_unauthorized`, the router refuses the fully authorized operation because
the sibling's paths are indistinguishable from the executed operation's, and the error
cites a path from an operation nobody ran. Fail-closed, so it denies rather than
leaks. Without it, `A` executes and returns its data, and `B`'s error survives with
its path truncated away: the path matches nothing in `A`'s shape.

No shape of the multi-operation case leaks data. Filtering removes unauthorized
fields from the document before planning, so no plan ever contains a fetch for them,
whichever operation executes; the plan cache keys on document text, operation name,
and authorization metadata, so entries never cross operations or grant sets.

For whoever scopes the emptiness check per-operation: scoping `paths` to the executed
operation without scoping the execution layer's `reject_unauthorized` check turns the
fail-closed refusal above into an execution. Both behaviours are pinned by
`authorized_operation_beside_unauthorized_sibling_is_refused` and
`..._executes`.

## Emptied operations run the normal pipeline

An emptied operation carries no marker. The planner returns a plan with `root: None`,
execution fetches nothing, and response formatting against the original operation
shapes the result, exactly as for a partial filter whose surviving selections are all
statically `@skip`ped:

```
without reject_unauthorized: 200 {"data":{"orga":null}, "errors":[{...,"path":["orga","id"]}]}
with reject_unauthorized:    200 {"data":null,          "errors":[{...,"path":["orga","id"]}]}
```

The `reject_unauthorized` response comes from the execution-service layer, which holds
the flag from configuration and answers before execution.

A refusal marker on the plan was considered and rejected. It would preserve
`data: null` for the emptied-without-reject case, and that is its entire effect: the
shaped response discloses nothing the errors do not already name, subgraphs are
unreachable either way (`root: None` has no fetch nodes), and the cache keys plans by
authorization state with or without a marker. The marker also cannot be derived from
the plan (a rootless plan equally describes an all-`@skip`ped partial filter, and
`filtered_query` absence equally describes `dry_run`), so it would have to be carried
as a serialized field for one byte-level difference. The shaped response is also the
field-error reading of the GraphQL spec; the remaining open question, field error
versus request error (4xx, no `data` key), belongs to the spec-compliance follow-up.

The one wrinkle: with `errors.response: disabled`, an emptied operation returns shaped
nulls with no errors, indistinguishable from every root field genuinely being null.
Operators choosing `disabled` have opted out of the signal.

## Outstanding: `data: null` versus `data` absent

The router answers a `reject_unauthorized` refusal with HTTP 200 and `"data": null`.
The GraphQL specification defines two error classes, and the refusal fits neither
shape the router sends:

> "A *request error* is an error raised during a *request* which results in no response
> data." ... "If a request error is raised, the *response* must be a *request error
> result*. The `data` entry in this map must not be present."

A refusal happens before execution and produces no response data, so it is a request
error: `data` absent, not `data: null`. The GraphQL-over-HTTP specification then binds
the status to the body for `application/graphql-response+json`:

> "If the GraphQL response contains the data entry and it is not null, then the server
> MUST reply with a `2xx` status code." ... "If the GraphQL response does not contain
> the data entry then the server MUST reply with an appropriate `4xx` or `5xx` status
> code."

Resolving this carries three coupled decisions:

1. Status by content type. Data-absent with 200 violates a MUST for
   `application/graphql-response+json`; the legacy `application/json` media type keeps
   200 regardless. The response shape therefore depends on the negotiated content
   type, which `ClientRequestAccepts` already tracks.
2. Error shape. The spec attaches `path` to errors "associated to a particular field
   in the GraphQL result"; a request error has no result, so the per-path
   `UNAUTHORIZED_FIELD_OR_TYPE` errors need restructuring or lose their paths.
3. `errors.response: disabled`. The spec requires non-empty `errors` when `data` is
   absent, so a request-error refusal with suppressed errors is an invalid response;
   the option has to override the shape or be rejected for this combination.

`apollo-errors` models per-error HTTP status and per-format rendering, which fits
decision 1; it does not yet model multi-error responses with paths or the
absent-versus-null `data` distinction, which live on the response rather than the
error. `returns_http_200` and `rejection_sends_null_data` hold the current shape and
flip when this resolves.

## Still open elsewhere

Keying the plan cache on the filtered query text (making `CacheKeyMetadata` redundant
in the key, deduplicating plans across grant sets that filter identically) remains
unimplemented and belongs with the cache-key work, not this ticket.
