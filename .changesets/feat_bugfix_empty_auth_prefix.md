### Add support for empty auth prefixes ([Issue #2909](https://github.com/apollographql/router/issues/2909))

This updates the `authentication.jwt` plugin to support empty prefixes for the JWT header. Some companies use prefix-less headers; previously, the authentication plugin would reject requests even with an empty header explicitly set, such as: 

```yml 
authentication:
  jwt:
    header_value_prefix: ""
```

This change enables the plugin to support this.

By [@lleadbet](https://github.com/lleadbet) in https://github.com/apollographql/router/pull/3206
