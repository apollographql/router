### Take advantage of improvement in the http crate for cloning ([PR #7559](https://github.com/apollographql/router/pull/7559))

The router depends upon the `http` crate. Prior to version 1.0, that crate did not support cloning HTTP requests or responses. This meant that in Router 1.x we could not properly clone Requests or Responses and worked around this by ignoring any Extensions on either the Request or the Response when we were cloning them.

This had the unfortunate effect of causing hard to debug interactions between Router components. For example, Rhai Header Manipulation (which clones both Requests and Responses) and File Uploading which sets an HTTP Extension and relies on its continued existence to allow File Uploads to operate correctly. The Rhai Header manipulation mean that the Extensions required by a File Upload were lost even though the Rhai code was not directly manipulating the associated Extensions.

In `http` crate version 1.0, this was fixed, but we didn't take advantage of that fix until now. We now support the correct cloning of both Requests and Responses, including Extensions.

By [@garypen](https://github.com/garypen) in https://github.com/apollographql/router/pull/7559