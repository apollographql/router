/**
 * Coprocessor for http_service (router front HTTP) and service_http (outbound subgraph/connector HTTP).
 * - http_service: RouterHttpRequest, RouterHttpResponse
 * - service_http: ServiceHttpRequest, ServiceHttpResponse (outbound to subgraphs and connectors)
 */
const express = require("express");
const app = express();
const port = 3000;

app.use(express.json());

app.post("/", (req, res) => {
  const payload = req.body;
  const stage = payload.stage;

  if (stage === "RouterHttpRequest") {
    res.json({ ...payload, control: "continue" });
    return;
  }

  if (stage === "RouterHttpResponse") {
    const headers = payload.headers || {};
    headers["x-http-service-stage"] = ["response"];
    res.json({ ...payload, headers });
    return;
  }

  if (stage === "ServiceHttpRequest") {
    res.json({ ...payload, control: "continue" });
    return;
  }

  if (stage === "ServiceHttpResponse") {
    const headers = payload.headers || {};
    headers["x-service-http-stage"] = ["response"];
    res.json({ ...payload, headers });
    return;
  }

  res.json(payload);
});

app.listen(port, () => {
  console.log(`Coprocessor (http_service + service_http) running on http://127.0.0.1:${port}`);
  console.log("Start the router with the router.yaml in this directory.");
});
