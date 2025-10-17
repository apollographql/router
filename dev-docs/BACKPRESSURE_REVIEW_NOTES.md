# Back-Pressure Review

This PR is intended to introduce some backpressure into the router so that traffic shaping works effectively.

## Pipeline Change Motivations

### Lack of feedback

Poll::Ready(Ok(())) is a sign that a service is not going to ask the successor service if it's ready. In a pipeline that would like to exercise back-pressure this is not a good thing.

We don't exercise any back-pressure before we hit the router service. The main reason for this is a desire to put telemetry at the front of the back-pressure pipe. Currently, the router service accepts requests and unquestioningly (recent changes to the query planner mean, it's not entirely unquestioning, but ...) starts to process them. The main motivation of this change is to allow services to poll successors for readiness.

### State

Some services require state to work effectively. Creating and throwing away our pipeline for every request is both wasteful and breaks our ability to use state aware layers, such as RateLimits. The secondary motivation for this change is the ability to use standard Tower layers for rate limits or other stateful functionality.

## Review Sequencing

I've tried to group all the files which are interesting to review here. All files should be reviewed, but these notes are intended to make understanding the more complex changes a little simpler.

### Pipeline Cloning

The most complex (and probably difficult to understand changes) are in the various affected services which implement ServiceFactory. I've not fixed all instances. I've done enough to allow us to exercise effective backpressure from RouterService -> ExecutionService and then again through the SubgraphService. The reason I haven't converted all services is because it's hard/complicated and the improvements we get from these changes are probably enough to materially improve the behaviour of the router. We may be able to do the same thing to other services in 2.1 or it may have to wait until 3.0. We need a Mutex to store our single service under because the various consumers of ServiceFactory require Sync access to our various services. If we could unpick that change of responsibilities we could maybe improve this so that we don't need the Mutex, but I think it would be major open heart surgery on the router to do this. In the performance testing I've performed so far, the Mutex doesn't seem to be a hot spot since it's called once per connection for Router and Service (multiple times for subgraphs) and is a lightweight Arc clone() under the lock which should be pretty fast.

#### Router Service

There are substantial changes here to form the start of our cloneable, backpressure enabled pipeline. The primary changes are
 - Not to provide a supergraph creator to the router service. We want it to use the same supergraph service as long as it lives.
 - Only create the router create service builder once (in the constructor). We story it under a Mutex to keep it Sync and then clone it whenever we need it.

apollo-router/src/services/router/service.rs

#### Supergraph Service

We make a lot of changes here so that our downstream execution service is preserved. Again we only build a single service which we store under a mutex and lock/clone as required.

apollo-router/src/services/supergraph/service.rs

#### Subgraph Service

We do the same thing as the router service and only create a single Subgraph service for each subgraph. The complication is that we have multiple subgraphs, so we store them in a hash.

apollo-router/src/services/subgraph_service.rs

### BackPressure Preservation

In a number of places, we could have preserved backpressure, but didn't. These are fixed in this PR.

apollo-router/src/plugins/traffic_shaping/deduplication.rs
apollo-router/src/services/hickory_dns_connector.rs

### Backpressure Control

In traffic shaping we have the bulk of implementation which exercises control over our new backpressure, via load_shed.

apollo-router/src/plugins/traffic_shaping/mod.rs

### Removing Stuff

OneShotAsyncCheckpoint is a potential footgun in a backpressure pipeline. I've removed it. A number of files are impacted by now using AsyncCheckpoint which requires `buffered`.

apollo-router/src/layers/async_checkpoint.rs
apollo-router/src/layers/mod.rs
apollo-router/src/plugins/coprocessor/execution.rs
apollo-router/src/plugins/coprocessor/mod.rs
apollo-router/src/plugins/coprocessor/supergraph.rs
apollo-router/src/plugins/file_uploads/mod.rs
apollo-router/src/services/layers/allow_only_http_post_mutations.rs

### Changes because pipeline is now Cloned

apollo-router/src/plugin/test/mock/subgraph.rs
apollo-router/src/plugins/cache/entity.rs
apollo-router/src/plugins/cache/metrics.rs
apollo-router/src/plugins/cache/tests.rs
apollo-router/src/services/layers/apq.rs

### Comments on layer review

There are a number of comments in the PR relating to dev-docs/layer-inventory.md.

apollo-router/src/plugins/authorization/mod.rs
apollo-router/src/services/layers/content_negotiation.rs
apollo-router/src/services/layers/query_analysis.rs
apollo-router/src/services/layers/static_page.rs

### Changes to Snapshots

#### New Concurrency Feature Added

apollo-router/src/configuration/snapshots/apollo_router__configuration__tests__schema_generation.snap

#### Changes to error/extension codes

apollo-router/tests/integration/snapshots/integration_tests__integration__traffic_shaping__router_timeout.snap
apollo-router/tests/integration/snapshots/integration_tests__integration__traffic_shaping__subgraph_timeout.snap

### Changes because of old cruft (usually in tests)

apollo-router/src/axum_factory/tests.rs

This should probably have been deleted a long time ago. I replaced it with the new test `it_timeout_router_requests` in apollo-router/src/plugins/traffic_shaping/mod.rs

### Examples

Mock expectations have changed and OneshotAsyncCheckpoint has been removed. Examples were updated to match the changes.

### Stuff I'm not sure about NOTE: IT MAY BE LAST IN THE FILE BUT IT NEEDS MOST CAREFUL REVIEW

#### Body Limits

This change is a complicated re-factoring so that limits are maintained for a connection. The previous strategy of keeping BodyLimitControl under an Arc does not work, because cloning the pipeline would mean it is shared across all connections. The PR changes this so that limits are still shared across all requests on a single connection, but not across all connections. This enables dynamic updating of the limit to work, but I'm not 100% sure this is the right thing to do.

This requires detailed review and discussion.

apollo-router/src/plugins/limits/layer.rs
apollo-router/src/plugins/limits/mod.rs

#### Sundry stuff I'm not sure about

I don't know if we want the `mock_subgraph_service_withf_panics_should_be_reported_as_service_closed test anymore`

apollo-router/src/query_planner/tests.rs

Now that traffic shaping happens earlier in the router, some things, which probably never were graphql errors anyway, are now no longer reported as graphql errors. I think the answer is to document this in the migration guide, since they now appear as OTEL response standard errors.

apollo-router/tests/integration/traffic_shaping.rs
