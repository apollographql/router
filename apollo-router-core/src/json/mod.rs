// This will be a Json document backed by bumpalo. It will be created with a maximum size and if that size and if that size is exceeded then it will fail to create new elements.
// We will not try to maintain API compatibility with serde_json and serde_json bytes as this will produce an unsatisfactory API.
// We will build in support for json path and jq transformations
