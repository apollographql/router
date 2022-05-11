import { ApolloServer, gql } from 'apollo-server-express';
import { ApolloServerPluginDrainHttpServer } from 'apollo-server-core';
import { buildFederatedSchema } from '@apollo/federation';


import express from 'express';
import http from 'http';
const { Tags, FORMAT_HTTP_HEADERS } = require('opentracing');

var initTracerFromEnv = require('jaeger-client').initTracerFromEnv;

// See schema https://github.com/jaegertracing/jaeger-client-node/blob/master/src/configuration.js#L37
var config = {
  serviceName: 'accounts',
};
var options = {};
var tracer = initTracerFromEnv(config, options);

const typeDefs = gql`
  extend type Query {
    me: User
  }

  type User @key(fields: "id") {
    id: ID!
    name: String
    username: String
  }
`;

const resolvers = {
  Query: {
    me() {
      return users[0];
    }
  },
  User: {
    __resolveReference(object) {
      return users.find(user => user.id === object.id);
    }
  }
};


const users = [
  {
    id: "1",
    name: "Ada Lovelace",
    birthDate: "1815-12-10",
    username: "@ada"
  },
  {
    id: "2",
    name: "Alan Turing",
    birthDate: "1912-06-23",
    username: "@complete"
  }
];

async function startApolloServer(typeDefs, resolvers) {
  const app = express();

  app.use(function jaegerExpressMiddleware(req, res, next) {
    const parentSpanContext = tracer.extract(FORMAT_HTTP_HEADERS, req.headers)
    const span = tracer.startSpan('http_server', {
        childOf: parentSpanContext,
        tags: {[Tags.SPAN_KIND]: Tags.SPAN_KIND_RPC_SERVER}
    });

    setTimeout(() => {
      span.log({
        statusCode: "200",
        objectId: "42"
      });
    }, 1);

    setTimeout(() => {
      span.finish();
    }, 2);

    next();
  });

  const httpServer = http.createServer(app);

  // Same ApolloServer initialization as before, plus the drain plugin.
  const server = new ApolloServer({
    schema: buildFederatedSchema([
      {
        typeDefs,
        resolvers
      }
    ]),
    csrfPrevention: true,
    plugins: [ApolloServerPluginDrainHttpServer({ httpServer })],
  });

  // More required logic for integrating with Express
  await server.start();
  server.applyMiddleware({
    app,

    // By default, apollo-server hosts its GraphQL endpoint at the
    // server root. However, *other* Apollo Server packages host it at
    // /graphql. Optionally provide this to match apollo-server.
    path: '/'
  });

  // Modified server startup
  await new Promise<void>(resolve => httpServer.listen({ port: 4001 }, resolve));
  console.log(`🚀 Server ready at http://localhost:4001${server.graphqlPath}`);
}

console.log("starting")
startApolloServer(typeDefs, resolvers)