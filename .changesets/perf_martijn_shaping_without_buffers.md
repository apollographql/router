### Apply traffic shaping on each request's own task, without buffers

Traffic shaping placed buffers in front of the router stage, of every shaped subgraph and of every connector source, so that each one's rate limit was shared and readied by one worker task, through which every request then passed. The rate and concurrency limits are now shared by the clones of each stack directly and charged where a request is called, on the request's own task, and those buffers are gone. The YAML, the per-subgraph and per-source overrides, the one counter per subgraph, source or router stage, tower's window and timer, and every status, error code and metric are unchanged: a request past a rate or concurrency limit is answered as before.

Two behaviours change. A readiness error from beneath a subgraph, connector source or router stage now fails only the request that met it, where the buffer failed every later request until the next reload. A shaped subgraph or connector source no longer sheds a request because its backpressure buffer was full; with no such buffer, requests beyond the stack's bound of 50,000 wait, as connector sources' requests already waited.

By [@martijnwalraven](https://github.com/martijnwalraven)
