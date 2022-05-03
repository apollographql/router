# Embedded router

:exclamation: Here be dragons! :exclamation: 

It is possible to run the router outside the default bundled web server (Axum). 

## Reasons to avoid this

* Router APIs are not stable. You will effectively be making a fork of the Router.
* You will lose hot-reload, telemetry, configuration and Apollo Studio support.
* Managing lifecycle is hard, re-creating a good configuration and validation experience will be lots of work.
* We will be looking at increasing the capability of the plugin system over time.
* We will be looking to support multiple deployment platforms over time.

## Reasons to consider this

* You have an existing web server stack that you wish to integrate with.
* You have particular configuration management requirements that are not currently catered for.
* You have something highly custom that you want to do and are prepared to go it alone.

