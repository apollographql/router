### CORS Private Network Access support ([PR #8279](https://github.com/apollographql/router/pull/8279))

Expands CORS configuration and support for the [private network access](https://wicg.github.io/private-network-access/) (PNA) feature in CORS. To enable PNA for a CORS policy in configuration, specify the field `private_network_access`. The `private_network_access` field has two subfields, `access_id` and `field_name`, both of which are optional.

```yaml
cors:
  policies:
    - origins: ["https://studio.apollographql.com"]
      private_netword_access:
        access_id:
    - match_origins: ["^https://(dev|staging|www)?\\.my-app\\.(com|fr|tn)$"]
      private_netword_access:
        access_id: "01:23:45:67:89:0A"
        access_name: "mega-corp device"
```

By [@TylerBloom](https://github.com/TylerBloom) in https://github.com/apollographql/router/pull/8279
