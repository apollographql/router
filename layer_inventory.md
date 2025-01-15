This is ordered from the point of view of a request to the router, starting at the outer boundary and going "deeper" into the layer onion.

## Router service
The router service consists of some layers in "front" of the service "proper", and of several layers *inside the router service*, which we appear to call manually.

I suspect that this is bad and that we should try to make all these layers part of a straightforward tower service stack.

Front (`RouterCreator`):
- StaticPageLayer
  - If configured, responds to any request that accepts a "text/html" response (*at all*, regardless of preference), with a fixed HTML response.
  - It is Clone!
  - This must occur before content negotiation, which rejects "Accept: text/html" requests.
- **content negotiation**: RouterLayer
  - It is Clone!
  - This layer rejects requests with invalid Accept or Content-Type headers.

Plugin layers happen here or in between somewhere, TBD.

Proper (`RouterService`):
- Batching
  - This is not a layer but the code can sort of be understood conceptually like one. Maybe it could, should be a layer?
  - Splits apart the incoming request into multiple requests that go through the rest of the pipeline, and puts the responses back together.
- Persisted queries: Expansion
  - This expands requests that use persisted query IDs, and rejects freeform GraphQL requests if not allowed per router configuration.
  - This is *not* a real layer right now. I suspect it should be.
  - **It is not Clone**, but it looks easy to make it Clone: we can just derive it on this layer and on the ManifestPoller type it contains (which already uses Arc internally)
- APQs
  - Clients can submit a hash+query body to add a query to the cache. Afterward, clients can use only the hash, and this layer will populate the query string in the request body before query analysis.
  - This is *not* a real layer right now. I suspect it should be.
  - **It is not Clone**. I think it's nothing that a little Arc can't solve, but I didn't trace it deeply enough to be sure.
- Query analysis
  - This does query parsing and validation and schema-aware hashing,
    and field/enum usage tracking for apollo studio.
  - This is *not* a real layer right now. I suspect it should be.
  - It is Clone!
  - This includes an explicit call to the AuthorizationPlugin. I suspect the AuthorizationPlugin should instead add its own layer for this, but chances are there was a good reason to do it this way, like not having precise enough hook-points. Still, maybe it can be a separate layer that we add manually.
  - Query analysis also exposes a method that is used by the query planner service, which could just as well be a standalone function in my opinion.
- Persisted queries: Safelisting
  - This is *not* a real layer right now. I suspect it should be.
  - For requests *not* using persisted queries, this layer checks incoming GraphQL documents against the "free-form GraphQL behaviour" configuration (essentially: safelisting), and rejects requests that are not allowed.
  - For Clone-ness, see "Persisted queries: Expansion"
- Straggler bits, not part of sub-layers. I think some of these should be normal layers, and some of them should be just a `.map_response()` layer in the service stack.
  - It does something with the `Vary` header.
  - It adjusts the status code to 499 if the request was canceled.
  - It does various JSON serialization bits.
  - It does various telemetry bits such as counting errors.
  - It appears to do the *exact same thing* as the content negotiation SupergraphLayer to populate the Content-Type header.

## Supergraph service

- **content negotiation**: SupergraphLayer
  - It is Clone!
  - This layer sets the Content-Type header on the response.
- AllowOnlyHttpPostMutationsLayer is the final step before going into the supergraph service proper.
  - **It is not Clone** today but it can trivially be derived.
