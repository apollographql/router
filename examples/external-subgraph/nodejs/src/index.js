const express = require("express");
const app = express();
const port = 3000;

app.use(express.json());

app.post("/", (req, res) => {
  console.log(`request headers ${JSON.stringify(req.headers, null, 2)}`);
  const request = req.body;
  console.log("📞 received request");
  console.log("JSON context:");
  console.log(JSON.stringify(request.context, null, 2));

  console.log("✉️ Got payload:");
  console.log(JSON.stringify(request.body, null, 2));

  // let's add an arbitrary header to the request
  const headers = request.headers || {};
  headers["x-my-subgraph-api-key"] = ["ThisIsATestApiKey"];
  request.headers = headers;

  // let's add a context key so that the subgraph_http_service displays the headers it's about to send!
  const context = request.context || {};
  const entries = context.entries || {};
  entries["apollo_authentication::JWT::claims"] = true;
  context.entries = entries;
  request.context = context;

  // let's mess with the uri, but only if we are about to call the reviews service
  if (request.serviceName === "reviews") {
    request.uri = "http://localhost:4042";
  }

  console.log("modified payload:");
  console.log(JSON.stringify(request, null, 2));

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
