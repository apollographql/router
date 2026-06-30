const express = require("express");
const app = express();
const port = 3000;

app.use(express.json());

// Maps subgraph name -> list of surrogate keys seen in that subgraph's most recent response.
// In a production system you would want a more precise key (e.g. a hash of the subgraph
// request body) so that multiple concurrent requests to the same subgraph don't collide.
let surrogateKeysBySubgraph = new Map();
// Example:
// {
//   "products": ["homepage", "feed"],
//   "reviews": ["homepage"]
// }
// --------------
// For every surrogate cache key we know the related cache keys
// Example:
// {
//   "homepage": [
//     "version:1.2:subgraph:products:type:Query:hash:af9febfa...:data:d9d84a3c...",
//     "version:1.2:subgraph:reviews:type:Query:hash:1de543db...:data:d9d84a3c..."
//   ],
//   "feed": [
//     "version:1.2:subgraph:products:type:Query:hash:af9febfa...:data:d9d84a3c..."
//   ]
// }

app.post("/", (req, res) => {
  const request = req.body;
  console.log("✉️ Got payload:");
  console.log(JSON.stringify(request, null, 2));
  switch (request.stage) {
    case "SubgraphResponse":
      request.headers["surrogate-keys"] = ["homepage, feed"]; // To simulate
      // Capture the surrogate keys returned by the subgraph, keyed by subgraph name.
      // Example:
      // surrogateKeysBySubgraph = {
      //   "products": ["homepage", "feed"]
      // }
      if (request.headers["surrogate-keys"] && request.subgraphName) {
        let keys = request.headers["surrogate-keys"]
          .join(",")
          .split(",")
          .map((k) => k.trim());

        surrogateKeysBySubgraph.set(request.subgraphName, keys);
        console.log("surrogateKeysBySubgraph", surrogateKeysBySubgraph);
      }
      break;
    case "SupergraphResponse":
      if (
        request.context &&
        request.context.entries &&
        request.context.entries["apollo::response_cache::debug_cached_keys"]
      ) {
        // debug_cached_keys is an array of cache entry objects. Each object has:
        //   key         - the Redis cache key string
        //   subgraphName - which subgraph produced this entry
        //   subgraphRequest - the GraphQL request sent to the subgraph
        //   source      - "subgraph" (freshly fetched) or "cache" (served from cache)
        //   shouldStore - whether the entry will be written to Redis
        //   ... (see response_cache debug documentation for the full schema)
        let cacheEntries =
          request.context.entries["apollo::response_cache::debug_cached_keys"];
        let mapping = {};

        for (const entry of cacheEntries) {
          const surrogateCacheKeys = surrogateKeysBySubgraph.get(
            entry.subgraphName
          );
          if (surrogateCacheKeys) {
            // Create the mapping between surrogate cache keys and effective cache keys
            // Example:
            // {
            //   "homepage": [
            //     "version:1.2:subgraph:products:type:Query:hash:af9febfa...:data:d9d84a3c..."
            //   ],
            //   "feed": [
            //     "version:1.2:subgraph:products:type:Query:hash:af9febfa...:data:d9d84a3c..."
            //   ]
            // }
            for (const surrogateKey of surrogateCacheKeys) {
              if (mapping[surrogateKey]) {
                mapping[surrogateKey].push(entry.key);
              } else {
                mapping[surrogateKey] = [entry.key];
              }
            }
          }
        }

        console.log(
          "Surrogate key -> cache key mapping:",
          JSON.stringify(mapping, null, 2)
        );
      }
      break;
    default:
      return res.json(request);
  }
  res.json(request);
});

app.listen(port, () => {
  console.log(`🚀 Coprocessor running on port ${port}`);
  console.log(
    `Run a router with the provided router.yaml configuration to test the example:`
  );
  console.log(
    `APOLLO_KEY="YOUR_APOLLO_KEY" APOLLO_GRAPH_REF="YOUR_APOLLO_GRAPH_REF" cargo run -- --configuration router.yaml`
  );
});
