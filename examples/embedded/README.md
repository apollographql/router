# Embedded router

:exclamation: Here be dragons! :exclamation: 

It is possible to run the router outside the default bundled web server (Warp). Reasons to do this are:

* You have an existing web server stack that you wish to integrate with.
* You have very high performance requirements and you want to avoid the hit of dynamic configuration.  
* You have particular configuration management requirements that are not currently catered for.
* You have something highly custom that you want to do.

We don't have a compatibility matrix between the router and other libraries, so do expect incompatibilities.


