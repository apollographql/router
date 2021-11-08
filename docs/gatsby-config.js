const themeOptions = require("gatsby-theme-apollo-docs/theme-options");

module.exports = {
  plugins: [
    {
      resolve: "gatsby-transformer-remark",
      options: {
        plugins: [
          "gatsby-remark-autolink-headers",
          "gatsby-remark-check-links",
        ],
      },
    },
    {
      resolve: "gatsby-theme-apollo-docs",
      options: {
        ...themeOptions,
        root: __dirname,
        pathPrefix: "/docs/router",
        subtitle: "Router",
        description: "A guide to the Router",
        githubRepo: "apollographql/router",
        sidebarCategories: {
          null: ["index", "configuration"],
          Quickstart: [
            "quickstart/helloworld",
            "quickstart/build",
            "quickstart/docker",
            "quickstart/nodejs",
          ],
        },
      },
    },
  ],
};
