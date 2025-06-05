





Pipeline changes

New pipeline services are indented in bold.

* axum
  * **http_server**
* router
  * **bytes_server**
  * **json_server**
  * **query_parser**
* supergraph
  * **query_planning**
* query planning
  * **query_execution**  
* execution
  * **query_fetch**
* fetch - branch to connectors or subgraph

### Subgraph flow
* subgraph
  * **json_client**
  * **bytes_client**
  * **http_client**
* **http_client**

### Connectors flow
* input
* connectors
* connectors_request
  * **json_client**
  * **bytes_client**
  * **http_client**
* http_client

NOTE:
* Fetch service is not integrated yet
* No new services actually do anything except convert to and from the new shapes.
* Query preparation service is not integrated

Plan
* Inject new services into pipeline. This will initially just transform to and from service shapes. It will allow us to create new layers in the correct format.
* Refactor Fetch service to unify onto the new core fetch service. This means merging the existing connector service and the fetch service.
* Move query planner service to open core
* Investigate fasttrace and logforth

****