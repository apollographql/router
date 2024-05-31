const { ApolloServer } = require("@apollo/server");
const { expressMiddleware } = require("@apollo/server/express4");
const { buildSubgraphSchema } = require("@apollo/subgraph");
const {
  ApolloServerPluginDrainHttpServer,
} = require("@apollo/server/plugin/drainHttpServer");
const rateLimit = require("express-rate-limit");
const express = require("express");
const http = require("http");
const { json } = require("body-parser");
const cors = require("cors");
const { parse } = require("graphql");

const rateLimitTreshold = process.env.LIMIT || 5000;

const typeDefs = parse(`#graphql
  extend schema
    @link(url: "https://specs.apollo.dev/federation/v2.3"
          import: ["@key" "@shareable" "@external", "@provides", "@requires"])

  type Query {
    me: User
    recommendedProducts: [Product]
    topProducts(first: Int = 5): [Product]
  }

  type Mutation {
    createProduct(upc: ID!, name: String): Product
    createReview(upc: ID!, id: ID!, body: String): Review
  }

  type User @key(fields: "id") {
    id: ID!
    name: String
    username: String @shareable
    reviews: [Review]
  }

  type Product @key(fields: "upc") {
    upc: String!
    name: String
    weight: Int
    price: Int
    inStock: Boolean
    shippingEstimate: Int
    reviews: [Review]
    reviewsForAuthor(authorID: ID!): [Review]
  }

  type Review @key(fields: "id") {
    id: ID!
    body: String
    author: User
    product: Product
  }

`);

const users = [
  {
    id: "1",
    name: "Ada Lovelace",
    birthDate: "1815-12-10",
    username: "@ada",
  },
  {
    id: "2",
    name: "Alan Turing",
    birthDate: "1912-06-23",
    username: "@complete",
  },
];

const inventory = [
    { upc: "1", inStock: true },
    { upc: "2", inStock: false },
    { upc: "3", inStock: true },
    { upc: "4", inStock: false }
  ];

const products = [
    {
      upc: "1",
      name: "Table",
      price: 899,
      weight: 100,
      inStock: true,
    },
    {
      upc: "2",
      name: "Couch",
      price: 1299,
      weight: 1000,
      inStock: false,
    },
    {
      upc: "3",
      name: "Chair",
      price: 54,
      weight: 50,
      inStock: true,
    },
    {
      upc: "4",
      name: "Bed",
      price: 1000,
      weight: 1200,
      inStock: false,
    }
  ];

  const reviews = [
    {
      id: "1",
      authorID: "1",
      product: { upc: "1" },
      body: "Love it!",
    },
    {
      id: "2",
      authorID: "1",
      product: { upc: "2" },
      body: "Too expensive.",
    },
    {
      id: "3",
      authorID: "2",
      product: { upc: "3" },
      body: "Could be better.",
    },
    {
      id: "4",
      authorID: "2",
      product: { upc: "1" },
      body: "Prefer something else.",
    },
  ];

const resolvers = {
  Query: {
    me(parent, args, contextValue, info) {
      info.cacheControl.setCacheHint({maxAge: 60, scope: 'PRIVATE' });
      return users[0];
    },
    recommendedProducts(parent, args, contextValue, info) {
      info.cacheControl.setCacheHint({ maxAge: 10, scope: 'PRIVATE' });

      let products = [{ upc: "1"}, { upc: "2"}, { upc: "3"}, { upc: "4"}].sort(() => Math.random() - Math.random()).slice(0, 2);
      return products;
    },
    topProducts(parent, args, contextValue, info)  {
        info.cacheControl.setCacheHint({ maxAge: 60 });

        return products.slice(0, args.first);
    },
  },
  Mutation: {
    createProduct(_, args) {
      return {
        upc: args.upc,
        name: args.name,
      };
    },
    createReview(p, args) {
      return {
        id: args.id,
        body: args.body,
        product: { upc: args.upc },
      };
    },
  },
  User: {
    __resolveReference(object, _, info) {
      info.cacheControl.setCacheHint({ maxAge: 60 });
      return users.find((user) => user.id === object.id);
    },
    reviews(user) {
        return reviews.filter((review) => review.authorID === user.id);
      },
    numberOfReviews(user) {
        return reviews.filter((review) => review.authorID === user.id).length;
    },
    /*username(user) {
        const found = usernames.find((username) => username.id === user.id);
        return found ? found.username : null;
    },*/
  },
  Product: {
    __resolveReference(object, _, info) {
      info.cacheControl.setCacheHint({ maxAge: 60 });

      return products.find(product => product.upc === object.upc);
    },
    shippingEstimate(object) {
        // free for expensive items
        if (object.price > 1000) return 0;
        // estimate is based on weight
        return object.weight * 0.5;
    },
    reviews(product) {
        return reviews.filter((review) => review.product.upc === product.upc);
    },
    reviewsForAuthor(product, { authorID }) {
        return reviews.filter(
          (review) =>
            review.product.upc === product.upc && review.authorID === authorID
        );
    },
  },
  Review: {
    author(review) {
      const found = reviews.find(r => r.id === review.id);
      return found ? { __typename: "User", id: found.authorID } : null;
    },
  },
};

async function startApolloServer(typeDefs, resolvers) {
  // Required logic for integrating with Express
  const app = express();

  const limiter = rateLimit({
    windowMs: 60 * 60 * 1000, // 1 hour
    max: rateLimitTreshold,
  });

  const httpServer = http.createServer(app);

  const server = new ApolloServer({
    schema: buildSubgraphSchema([
      {
        typeDefs,
        resolvers,
      },
    ]),
    allowBatchedHttpRequests: true,
    plugins: [ApolloServerPluginDrainHttpServer({ httpServer })],
  });

  await server.start();
  app.use("/", cors(), json(), limiter, expressMiddleware(server));

  // Modified server startup
  const port = process.env.PORT || 4005;

  await new Promise((resolve) => httpServer.listen({ port }, resolve));
  console.log(`🚀 Accounts Server ready at http://localhost:${port}/`);
}

startApolloServer(typeDefs, resolvers);
