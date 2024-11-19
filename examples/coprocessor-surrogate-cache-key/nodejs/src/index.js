const express = require("express");
const app = express();
const port = 3000;

app.use(express.json());

// This is for demo purpose and will keep growing over the time
// It saves the value of surrogate cache keys returned by a subgraph request
let surrogateKeys = {};
// Example:
// {
//   "​​0e67db40-e98d-4ad7-bb60-2012fb5db504": [
//     "elections",
//     "sp500"
//   ],
//   "​​0d77db40-e98d-4ad7-bb60-2012fb5db555": [
//     "homepage"
//   ]
// }
// --------------
// For every surrogate cache key we know the related cache keys
// Example:
// {
//   "elections": [
//     "version:1.0:subgraph:reviews:type:Product:entity:4e48855987eae27208b466b941ecda5fb9b88abc03301afef6e4099a981889e9:hash:1de543dab57fde0f00247922ccc4f76d4c916ae26a89dd83cd1a62300d0cda20:data:d9d84a3c7ffc27b0190a671212f3740e5b8478e84e23825830e97822e25cf05c"
//   ],
//   "sp500": [
//     "version:1.0:subgraph:reviews:type:Product:entity:4e48855987eae27208b466b941ecda5fb9b88abc03301afef6e4099a981889e9:hash:1de543dab57fde0f00247922ccc4f76d4c916ae26a89dd83cd1a62300d0cda20:data:d9d84a3c7ffc27b0190a671212f3740e5b8478e84e23825830e97822e25cf05c"
//   ]
// }
let surrogateKeysMapping = {};

app.post("/", (req, res) => {
  const request = req.body;
  console.log("✉️ Got payload:");
  console.log(JSON.stringify(request, null, 2));
  switch (request.stage) {
    case "SubgraphResponse":
      request.headers["surrogate-keys"] = ["homepage, feed"]; // To simulate
      // Fetch the surrogate keys returned by the subgraph to create a mapping between subgraph request id and surrogate keys, to create the final mapping later
      // Example:
      // {
      //   "​​0e67db40-e98d-4ad7-bb60-2012fb5db504": [
      //     "elections",
      //     "sp500"
      //   ]
      // }
      if (request.headers["surrogate-keys"] && request.subgraphRequestId) {
        let keys = request.headers["surrogate-keys"]
          .join(",")
          .split(",")
          .map((k) => k.trim());

        surrogateKeys[`${request.subgraphRequestId}`] = keys;
        console.log("surrogateKeys", surrogateKeys);
      }
      break;
    case "SupergraphResponse":
      if (
        request.context &&
        request.context.entries &&
        request.context.entries["apollo::entity_cache::cached_keys_status"]
      ) {
        let contextEntry =
          request.context.entries["apollo::entity_cache::cached_keys_status"];
        let mapping = {};
        Object.keys(contextEntry).forEach((request_id) => {
          let cache_keys = contextEntry[`${request_id}`];
          let surrogateCachekeys = surrogateKeys[`${request_id}`];
          if (surrogateCachekeys) {
            // Create the mapping between surrogate cache keys and effective cache keys
            // Example:
            // {
            //   "elections": [
            //     "version:1.0:subgraph:reviews:type:Product:entity:4e48855987eae27208b466b941ecda5fb9b88abc03301afef6e4099a981889e9:hash:1de543dab57fde0f00247922ccc4f76d4c916ae26a89dd83cd1a62300d0cda20:data:d9d84a3c7ffc27b0190a671212f3740e5b8478e84e23825830e97822e25cf05c"
            //   ],
            //   "sp500": [
            //     "version:1.0:subgraph:reviews:type:Product:entity:4e48855987eae27208b466b941ecda5fb9b88abc03301afef6e4099a981889e9:hash:1de543dab57fde0f00247922ccc4f76d4c916ae26a89dd83cd1a62300d0cda20:data:d9d84a3c7ffc27b0190a671212f3740e5b8478e84e23825830e97822e25cf05c"
            //   ]
            // }

            surrogateCachekeys.reduce((acc, current) => {
              if (acc[`${current}`]) {
                acc[`${current}`] = acc[`${current}`].concat(cache_keys);
              } else {
                acc[`${current}`] = cache_keys;
              }

              return acc;
            }, mapping);
          }
        });

        console.log(mapping);
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
