This is ordered from the point of view of a request to the router, starting at the outer boundary and going "deeper" into the layer onion.

## Router service
The router service consists of some layers in "front" of the service "proper", and of several layers *inside the router service*, which we appear to call manually.

I suspect that this is bad and that we should try to make these layers part of a normal tower service stack.

Front (RouterCreator):
- StaticPageLayer
  - If configured, responds to any request that accepts a "text/html" response (*at all*, regardless of preference), with a fixed HTML response.
  - It is Clone!
  - This must occur before content negotiation, which rejects "Accept: text/html" requests.
- **content negotiation**: RouterLayer
  - It is Clone!
  - This layer rejects requests with invalid Accept or Content-Type headers.

Proper (RouterService):
- Batching
  - This is not a layer but the code can sort of be understood conceptually like one.
  - Splits apart the incoming request into multiple requests that go through the rest of the pipeline, and puts the responses back together.
- Persisted queries, part 1
- APQs
- Query analysis
  - This does query parsing and validation and schema-aware hashing,
    and field/enum usage tracking for apollo studio.
  - This is *not* a real layer right now. I suspect it should be.
  - This includes an explicit call to the AuthorizationPlugin. I suspect the AuthorizationPlugin should instead add its own layer for this, but chances are there was a good reason to do it this way, like not having precise enough hook-points. Still, maybe it can be a separate layer that we add manually.
  - Query analysis also exposes a method that is used by the query planner service, which could just as well be a standalone function in my opinion.
- Persisted queries, part 2
- Straggler bits, not part of sub-layers:
  - It does something with the `Vary` header.
  - It adjusts the status code to 499 if the request was canceled.
  - It does various JSON serialization bits.
  - It does various telemetry bits such as counting errors.
  - It appears to do the exact same thing as the content negotiation SupergraphLayer to populate the Content-Type header. **We should definitely get rid of this**.

## Supergraph service

- **content negotiation**: SupergraphLayer
  - It is Clone!
  - This layer sets the Content-Type header on the response.
- AllowOnlyHttpPostMutationsLayer is the final step before going into the supergraph service proper.
  - It is not Clone today but it can trivially be derived.
