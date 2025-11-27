### Add CORS Private Network Access support ([PR #8279](https://github.com/apollographql/router/pull/8279))

CORS configuration now supports [private network access](https://wicg.github.io/private-network-access/) (PNA). Enable PNA for a CORS policy by specifying the `private_network_access` field, which supports two optional subfields: `access_id` and `access_name`.

**Example configuration:**

```yaml
cors:
  policies:
    - origins: ["https://studio.apollographql.com"]
      private_network_access:
        access_id:
    - match_origins: ["^https://(dev|staging|www)?\\.my-app\\.(com|fr|tn)$"]
      private_network_access:
        access_id: "01:23:45:67:89:0A"
        access_name: "mega-corp device"
```

By [@TylerBloom](https://github.com/TylerBloom) in https://github.com/apollographql/router/pull/8279
