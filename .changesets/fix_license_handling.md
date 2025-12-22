### Fix: Proper handling of licenses in a warning state

We allowed licenses in a warning state to bypass enforcement because we weren't returning an error, only the limits. This was happening, I think, because there's middleware handling expired licenses but not licenses in a warning state. So, we assumed that there'd be same kind of handling for licenses in a warning state. Alas, there's not.

We now error out if there are restricted features in use.

By [@aaronarinder](https://github.com/aaronarinder) in https://github.com/apollographql/router/pull/8768
