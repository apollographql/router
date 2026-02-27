### Document AWS API Gateway support for HTTP multipart subscriptions

Updated the API gateway subscriptions documentation to reflect that Amazon API Gateway now supports response streaming for REST APIs. HTTP multipart subscriptions are supported when the router is behind AWS API Gateway. Replaced the previous note that AWS API Gateway did not support streaming of HTTP data with this information, plus a link to the AWS announcement (November 2025) and a short configuration note with a link to Response transfer mode. Only the AWS section was changed; Azure, Apigee, Mulesoft, and Kong sections are unchanged.

By [@smyrick](https://github.com/smyrick) in https://github.com/apollographql/router/pull/8907
