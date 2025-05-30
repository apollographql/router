### Ensure that file uploads work correctly with Rhai scripts ([PR #7559](https://github.com/apollographql/router/pull/7559))

If a Rhai script was invoked during File Upload processing, then the "Content-Type" of the Request was not preserved correctly. This would cause a File Upload to fail.

The error message would be something like:

```
"message": "invalid multipart request: Content-Type is not multipart/form-data",
```

This is now fixed.

By [@garypen](https://github.com/garypen) in https://github.com/apollographql/router/pull/7559
