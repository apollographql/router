const express = require("express");
const app = express();
const port = 3000;

app.use(express.json());

app.post("/", (req, res) => {
  console.log(`request headers ${JSON.stringify(req.headers, null, 2)}`);
  const request = req.body;
  console.log("✉️ Got payload:");
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
