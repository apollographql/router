# RouterHttp Cleanup Plan (PR #8925 Follow-up)

This plan captures issues found during principal engineer review of PR #8925 (`[feat] New top level Router HTTP hook`). Items are ordered by priority. Implement them in order, since some later items depend on earlier ones.

---

## Context

PR #8925 adds `router_http_service` as a new pipeline stage (raw HTTP layer) sitting above the existing `Router` pipeline. It introduces:
- `RouterHttpGate` — dispatches static vs. full-pipeline requests
- `RouterHttpStage` / `RouterHttpRequestConf` / `RouterHttpResponseConf` — coprocessor config structs
- `process_router_http_request_stage` / `process_router_http_response_stage` — coprocessor processing fns
- Migration of `LicenseEnforcement` from `router_service` to `router_http_service`

The code works correctly but has several design issues for long-term maintainability.

---

## Issue 1 — `RouterHttpGate::poll_ready` violates Tower's service contract (MEDIUM)

**File:** `apollo-router/src/services/router/service.rs`

**Problem:**
The current `poll_ready` polls *both* `static_only` and `full_pipeline` on every call, even though only one is ever used per request. This violates Tower's contract (poll the service you will call, then call it once) and wastes buffer slots in the underlying `tower::Buffer`, degrading throughput under load.

```rust
// CURRENT (wrong): always polls both
fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
    let static_ready = self.static_only.poll_ready(cx);
    let full_ready = self.full_pipeline.poll_ready(cx);
    match (static_ready, full_ready) {
        (Ready(Ok(())), Ready(Ok(()))) => Ready(Ok(())),
        ...
    }
}
```

**Fix:**
Track which path was chosen during `poll_ready` so `call` uses the already-polled service.

```rust
struct RouterHttpGate {
    static_only: router::BoxService,
    full_pipeline: router::BoxService,
    static_page_enabled: bool,
    ready_path: Option<GatePath>, // NEW
}

#[derive(Clone, Copy)]
enum GatePath {
    Static,
    Full,
}

impl tower::Service<router::Request> for RouterHttpGate {
    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        // We don't know the request yet, so poll full_pipeline as the common case.
        // Static requests are rare (browser landing page only); if static_page_enabled
        // is false the static path is never taken. Poll both only when static is enabled.
        if self.static_page_enabled {
            // Poll full first; if not ready, return Pending.
            // If full is ready, also poll static so both are available.
            // Once both ready, we pick per-request in call().
            match self.full_pipeline.poll_ready(cx) {
                Poll::Pending => return Poll::Pending,
                Poll::Ready(Err(e)) => return Poll::Ready(Err(e)),
                Poll::Ready(Ok(())) => {}
            }
            match self.static_only.poll_ready(cx) {
                Poll::Pending => return Poll::Pending,
                Poll::Ready(Err(e)) => return Poll::Ready(Err(e)),
                Poll::Ready(Ok(())) => {}
            }
        } else {
            // Static page disabled — only poll the full pipeline.
            match self.full_pipeline.poll_ready(cx) {
                Poll::Ready(Ok(())) => {}
                other => return other,
            }
        }
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, req: router::Request) -> Self::Future {
        if self.static_page_enabled && is_static_landing_request(&req) {
            self.static_only.call(req)
        } else {
            self.full_pipeline.call(req)
        }
    }
}
```

**Note:** The `ready_path` field added to the struct in the sketch above is not strictly required with this approach (we always poll both when static is enabled), but the key change is: when `static_page_enabled == false`, skip polling `static_only` entirely. This avoids wasting a Buffer slot on the static service for every GraphQL request.

---

## Issue 2 — `RouterHttpStage`, `RouterRequestConf`, and processing fns are massively duplicated (HIGH, most impactful)

**Files:**
- `apollo-router/src/plugins/coprocessor/mod.rs`

**Problem:**
`RouterHttpStage::as_service`, `RouterHttpRequestConf`, `RouterHttpResponseConf`, `process_router_http_request_stage`, and `process_router_http_response_stage` are near-carbon-copies of the existing `RouterStage`, `RouterRequestConf`, `RouterResponseConf`, `process_router_request_stage`, and `process_router_response_stage`. This is 300+ lines of duplicated logic that will diverge over time.

The only meaningful differences are:
1. The `PipelineStep` variant used (`RouterHttpRequest` vs `RouterRequest`)
2. Config structs (`RouterHttpRequestConf` vs `RouterRequestConf`)
3. The boot-time validation behavior (error vs warn on deprecated context)

**Fix:**
Extract a shared generic implementation. The key insight is that both `RouterHttpStage` and `RouterStage` call the same `Externalizable::router_builder()` and share the same logic for building payloads, calling coprocessors, handling `Control::Break`, and updating headers/context.

Approach:
1. Create a `RouterCoprocessorRequestConfig` trait capturing the shared interface:
   ```rust
   trait RouterCoprocessorRequestConfig: Clone + Default + PartialEq {
       fn condition(&mut self) -> Option<&mut Condition<RouterSelector>>;
       fn headers(&self) -> bool;
       fn context(&self) -> &ContextConf;
       fn body(&self) -> bool;
       fn sdl(&self) -> bool;
       fn path(&self) -> bool;
       fn method(&self) -> bool;
       fn url(&self) -> Option<&str>;
       fn stage() -> PipelineStep;
   }
   ```

2. Implement the trait for both `RouterRequestConf` and `RouterHttpRequestConf`.

3. Replace both `process_router_http_request_stage` and `process_router_request_stage` with a single generic:
   ```rust
   async fn process_router_request_stage_generic<C, Cfg: RouterCoprocessorRequestConfig>(
       http_client: C,
       coprocessor_url: String,
       sdl: Arc<String>,
       request: router::Request,
       request_config: Cfg,
       response_validation: bool,
       executed: &mut bool,
   ) -> Result<ControlFlow<router::Response, router::Request>, BoxError>
   ```

4. Similarly unify the response stage processing.

5. The config structs themselves (`RouterHttpRequestConf` vs `RouterRequestConf`) can remain separate (they differ in boot-time validation behavior), but the processing functions don't need to know about that difference.

**Migration path:**
- Keep the existing public struct names to avoid breaking changes
- Add `#[deprecated]` aliases if needed
- Add a comment on both structs: "see RouterCoprocessorRequestConfig for shared behavior"

---

## Issue 3 — Dead code in deprecated context path of `process_router_http_request_stage` (LOW)

**File:** `apollo-router/src/plugins/coprocessor/mod.rs`

**Problem:**
Boot-time validation (in `PluginPrivate::new`) already rejects `ContextConf::Deprecated(true)` for `router_http`:
```rust
if matches!(init.config.router_http.request.context, ContextConf::Deprecated(true)) {
    return Err("...not supported...".into());
}
```

But inside `process_router_http_request_stage`, the code still handles `Deprecated(true)`:
```rust
if matches!(
    &request_config.context,
    ContextConf::NewContextConf(NewContextConf::Deprecated) | ContextConf::Deprecated(true)
) {
    key = context_key_from_deprecated(key);
}
```

The `Deprecated(true)` arm can never execute. This confuses readers into thinking deprecated context might work at runtime.

**Fix:**
Remove the `| ContextConf::Deprecated(true)` arm from both the request and response context update loops inside `process_router_http_request_stage` and `process_router_http_response_stage`. If Issue 2 is addressed first (generic processing fn), this simplification flows naturally from the trait implementation for `RouterHttpRequestConf`.

---

## Issue 4 — `_response_validation` is unused in `process_router_http_response_stage` (LOW)

**File:** `apollo-router/src/plugins/coprocessor/mod.rs`

**Problem:**
The parameter is named `_response_validation` (underscore prefix = intentionally unused) but there's no comment explaining why. The request stage uses `response_validation` to call `deserialize_coprocessor_response`. The response stage silently ignores it, which is a behavior difference with no explanation.

**Fix:**
Either:
a. Add a comment: `// Response validation is not applicable to response stage payloads (no GraphQL body to validate)`, OR
b. Remove the parameter entirely from `process_router_http_response_stage` (and `RouterHttpStage::as_service` which passes it through). This matches what the code actually does and removes the misleading parameter.

Option (b) is cleaner. Check whether `process_router_response_stage` (the non-HTTP variant) also ignores it — if so, this is a systemic cleanup.

---

## Issue 5 — `RouterHttpStage` missing `additionalProperties: false` in JSON schema (LOW)

**File:** `apollo-router/src/configuration/snapshots/apollo_router__configuration__tests__schema_generation.snap`

**Problem:**
The schema for `RouterHttpStage` does not have `"additionalProperties": false`, unlike `RouterHttpRequestConf` and `RouterHttpResponseConf` which both have it (via `#[serde(deny_unknown_fields)]`). This means users can add unknown keys at the stage wrapper level without a config validation error.

Compare:
- `RouterHttpRequestConf` → `#[serde(deny_unknown_fields)]` ✓
- `RouterHttpResponseConf` → `#[serde(deny_unknown_fields)]` ✓
- `RouterHttpStage` → **no `deny_unknown_fields`** ✗

**Fix:**
Add `#[serde(deny_unknown_fields)]` to `RouterHttpStage`. Then regenerate the snapshot:
```
cargo test -p apollo-router schema_generation -- --nocapture
```
and commit the updated snapshot.

Also check `RouterStage` for the same omission and fix both together.

---

## Issue 6 — `empty router_service` on `LicenseEnforcement` should be removed (LOW)

**File:** `apollo-router/src/plugins/license_enforcement/mod.rs`

**Problem:**
After moving logic to `router_http_service`, the `router_service` hook is left as a no-op with a comment:
```rust
fn router_service(&self, service: router::BoxService) -> router::BoxService {
    // License enforcement moved to router_http_service - returns service unchanged
    service
}
```

The default implementation of `router_service` in the `PluginPrivate` trait already returns `service` unchanged. Overriding it with identical behavior adds noise.

**Fix:**
Delete the `router_service` override entirely from `LicenseEnforcement`. The trait default handles it. No behavior change.

---

## Issue 7 — `buffered()` in `RouterHttpStage::as_service` needs a comment or removal (MEDIUM)

**File:** `apollo-router/src/plugins/coprocessor/mod.rs`

**Problem:**
```rust
.buffered() // XXX: Added during backpressure fixing
```

An `XXX` comment on a `buffered()` call in a hot path is a red flag. This adds latency and memory overhead on every coprocessor call. The underlying issue that required this fix should be documented, or the call should be removed if no longer needed.

**Fix:**
Investigate why `buffered()` was added:
1. Check if the same `buffered()` exists in `RouterStage::as_service` (the existing non-HTTP variant). If yes, this is consistent and just needs the comment improved.
2. If this was added to fix a specific bug/panic related to `poll_ready` not being called before `call`, document the root cause.
3. Replace `// XXX: Added during backpressure fixing` with a comment explaining: *what* problem it solves, *why* buffering solves it, and *when* it's safe to remove.

---

## Issue 8 — Changeset references wrong PR number (TRIVIAL)

**File:** `.changesets/feat_router_http.md`

**Problem:**
Line 9 reads:
```
By [@smyrick](https://github.com/smyrick) in https://github.com/apollographql/router/pull/8904
```
Should be `8925`.

**Fix:**
```diff
-By [@smyrick](https://github.com/smyrick) in https://github.com/apollographql/router/pull/8904
+By [@smyrick](https://github.com/smyrick) in https://github.com/apollographql/router/pull/8925
```

---

## Recommended Implementation Order

1. **Issue 8** — trivial changeset fix, do first to keep history clean
2. **Issue 6** — remove dead `router_service` override, zero-risk
3. **Issue 3** — remove dead deprecated context arm in processing fn
4. **Issue 5** — add `deny_unknown_fields` to `RouterHttpStage`, regenerate snapshot
5. **Issue 4** — clean up `_response_validation` (decide: remove param or add comment)
6. **Issue 7** — investigate `buffered()` and improve comment
7. **Issue 1** — fix `RouterHttpGate::poll_ready` Tower contract violation
8. **Issue 2** — large refactor: unify duplicate coprocessor stage processing fns (do last, most risk)

---

## Files Primarily Affected

| File | Issues |
|------|--------|
| `apollo-router/src/services/router/service.rs` | Issue 1 |
| `apollo-router/src/plugins/coprocessor/mod.rs` | Issues 2, 3, 4, 7 |
| `apollo-router/src/plugins/license_enforcement/mod.rs` | Issue 6 |
| `apollo-router/src/configuration/snapshots/...schema_generation.snap` | Issue 5 |
| `.changesets/feat_router_http.md` | Issue 8 |

## Testing After Changes

For each fix, run:
```bash
# Unit tests for coprocessor
cargo test -p apollo-router plugins::coprocessor

# Unit tests for license enforcement
cargo test -p apollo-router plugins::license_enforcement

# Schema snapshot regeneration (Issue 5 only)
cargo test -p apollo-router configuration::tests::schema_generation -- --nocapture

# Full router test suite
cargo test -p apollo-router

# Integration tests
cargo test -p apollo-router --test '*'
```
