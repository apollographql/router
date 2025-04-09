### Update Dockerfile exec script to use `#!/bin/bash` instead of `#!/usr/bin/env bash` ([Issue #3517](https://github.com/apollographql/router/issues/3517))

For users of Google Cloud Platform (GCP) Cloud Run platform, using the router's default Docker image was not possible due to an error that would occur during startup: 

```sh
"/usr/bin/env: 'bash ': No such file or directory"
```

To avoid this issue, we've changed the script to use `#!/bin/bash` instead of `#!/usr/bin/env bash`, as we use a fixed Linux distribution in Docker which has the Bash binary located there. 

By [@lleadbet](https://github.com/lleadbet) in https://github.com/apollographql/router/pull/7198
