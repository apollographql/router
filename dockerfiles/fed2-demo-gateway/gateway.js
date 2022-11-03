// Open Telemetry (optional)
const { ApolloOpenTelemetry } = require("supergraph-demo-opentelemetry");

if (process.env.APOLLO_OTEL_EXPORTER_TYPE) {
  new ApolloOpenTelemetry({
    type: "router",
    name: "router",
    exporter: {
      type: process.env.APOLLO_OTEL_EXPORTER_TYPE, // console, zipkin, collector
      host: process.env.APOLLO_OTEL_EXPORTER_HOST,
      port: process.env.APOLLO_OTEL_EXPORTER_PORT,
    },
  }).setupInstrumentation();
}

// Main
const { ApolloServer } = require("@apollo/server");
const {
  ApolloServerPluginUsageReporting,
} = require("@apollo/server/plugin/usageReporting");
const { startStandaloneServer } = require("@apollo/server/standalone");
const { ApolloGateway } = require("@apollo/gateway");
const { readFileSync } = require("fs");

const port = process.env.APOLLO_PORT || 4000;
const embeddedSchema =
  process.env.APOLLO_SCHEMA_CONFIG_EMBEDDED == "true" ? true : false;

const config = {};
const plugins = [];

if (embeddedSchema) {
  const supergraph = "/etc/config/supergraph.graphql";
  config["supergraphSdl"] = readFileSync(supergraph).toString();
  console.log("Starting Apollo Gateway in local mode ...");
  console.log(`Using local: ${supergraph}`);
} else {
  console.log("Starting Apollo Gateway in managed mode ...");
  plugins.push(
    ApolloServerPluginUsageReporting({
      fieldLevelInstrumentation: 0.01,
    })
  );
}

const gateway = new ApolloGateway(config);

async function startApolloServer() {
  const server = new ApolloServer({
    gateway,
    debug: true,
    // Subscriptions are unsupported but planned for a future Gateway version.
    subscriptions: false,
    plugins,
  });
  const { url } = await startStandaloneServer(server, {
    context: async ({ req }) => ({ token: req.headers.token }),
    listen: { port: 4000 },
  });

  console.log(`🚀  Server ready at ${url}`);
}

startApolloServer();
