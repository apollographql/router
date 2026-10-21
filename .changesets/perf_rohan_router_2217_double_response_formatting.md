### Format responses once when authorization filtered nothing

The router formatted almost every response twice. To decide whether authorization had filtered an operation, it compared the client's query text with the router's re-printed copy of the query, and the two almost never match. The second formatting pass is only needed when authorization removes fields from an operation, and the router now runs it only then.

Cached query plans also no longer hold a second copy of their operation's selections. Responses are unchanged.

By [@rohan-b99](https://github.com/rohan-b99)
