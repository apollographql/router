### CORS updates ([PR #7853](https://github.com/apollographql/router/pull/7853))

CORS (Cross-Origin Resource Sharing) is a security mechanism implemented by web browsers that controls how web pages from one domain can access resources from another domain. It's built into browsers to enforce the "same-origin policy" with controlled exceptions.

#### What is the Same-Origin Policy?

The same-origin policy is a fundamental security concept that restricts how scripts running on one origin (combination of protocol, domain, and port) can interact with resources from another origin. For example, a script from https://example.com cannot by default make requests to https://api.different-site.com.

#### Why CORS Exists

CORS exists primarily for security reasons:

- **Protection Against Malicious Scripts**: Without CORS, any website could make requests to other sites using your browser's stored credentials (cookies, authentication tokens). This could allow malicious sites to access your private data from other services you're logged into.
- **Controlled Cross-Origin Access**: While the same-origin policy is important for security, legitimate applications often need to access resources from different origins. CORS provides a standardized way to safely allow these cross-origin requests.

#### Our current CORS implementation

Currently, we rely on [tower_http::cors](https://docs.rs/tower-http/latest/tower_http/cors/index.html). This is a problem because our users want more functionality than is provided by that crate. Specifically, users want to configure CORS differently for different origins.

#### How this PR changes things

With this PR, users may now create CORS rules for multiple sets of origins.

#### Key Features:
- **Per-origin configuration** for headers, methods, credentials, and expose headers
- **Regex-based origin matching** for flexible origin patterns
- **Fallback to global settings** when origin-specific settings are empty
- **Header mirroring** when no `allow_headers` are configured (mirrors `Access-Control-Request-Headers`)
- **Backward compatibility** with existing configurations

Configuration that used to look like this:

```yaml
cors:
  trusted_origins:
    - "https://app.mycompany.com"
    - "https://admin.mycompany.com"
  allow_headers:
    - "content-type"
    - "authorization"
  methods: [GET, POST, OPTIONS]
```

now looks like this:

```yaml
cors:
  methods: [GET, POST, OPTIONS]  # Global default
  policies:
    # Trusted origins get credentials
    - origins:
        - "https://app.mycompany.com"
        - "https://admin.mycompany.com"
      allow_credentials: true
      allow_headers: ["content-type", "authorization"]
    
    # Catch-all for untrusted origins using regex
    - match_origins: [".*"]
      allow_credentials: false
      allow_headers: ["content-type", "authorization"]
```

#### Default Configuration

The default CORS configuration remains unchanged for backward compatibility:
- Default origins: `https://studio.apollographql.com`
- Default methods: `GET`, `POST`, `OPTIONS`
- Default `allow_credentials`: `false`
### Introduce per-origin CORS policies ([PR #7853](https://github.com/apollographql/router/pull/7853))

Configuration can now specify different Cross-Origin Resource Sharing (CORS) rules for different origins using the `cors.policies` key. See the [CORS documentation](https://www.apollographql.com/docs/graphos/routing/security/cors) for details.

```yaml
cors:
  policies:
    # The default CORS options work for Studio.
    - origins: ["https://studio.apollographql.com"]
    # Specific config for trusted origins
    - match_origins: ["^https://(dev|staging|www)?\\.my-app\\.(com|fr|tn)$"]
      allow_credentials: true
      allow_headers: ["content-type", "authorization", "x-web-version"]
    # Catch-all for untrusted origins
    - origins: ["*"]
      allow_credentials: false
      allow_headers: ["content-type"]
