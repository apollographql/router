Fix @cost directive to accept float/string weights (Fixes #7186)

The router’s @cost directive originally only accepted integer weight arguments. This prevented partial or fractional cost values and was inconsistent with the GraphQL cost‑directive proposal. As part of cost‑governance improvements, the router now accepts floating‑point and numeric string values for the directive’s weight.

With this change, @cost may be written as @cost(weight: 0.5) or @cost(weight: "0.5") and the router treats both as a floating‑point value. Backwards‑compatibility is preserved for integer weights. The GraphQL schema has been updated to advertise Float as the argument type, and the weight is stored in an f64 internally. Validation ensures the value is non‑negative and finite. Unit tests verify that fractional weights reduce the calculated field cost accordingly.

By @ashokk1990 in https://github.com/apollographql/router/pull/9483
