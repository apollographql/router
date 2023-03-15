### Do not fail JWT plugin init on JWKS download ([Issue #2747](https://github.com/apollographql/router/issues/2747))

JWKS download can fail sometimes, but it can be temporary, so it should not fail plugin initialization. We still try to download them during initialization, to make a reasonable effort of starting a router with all the JWKS, but if one download fails we still go on

By [@Geal](https://github.com/Geal) in https://github.com/apollographql/router/pull/2754