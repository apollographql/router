### fix: update internal redis timeout ([PR #8863](https://github.com/apollographql/router/pull/8863))

We've found that mTLS in some environments sometimes takes longer than what our previous timeout would allow for; so, we're now waiting 10s for internal redis operations rather than the previous 5s. This also bumps up the amount of time we allow connections to live before counting them as unresponsive (also 10s, compared to the previous 5s)


By [@aaronarinder](https://github.com/aaronarinder) in https://github.com/apollographql/router/pull/8863
