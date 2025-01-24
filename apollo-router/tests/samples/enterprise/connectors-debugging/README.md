Example testing the connectors debugging extensions feature

Hardcodes a port for the http snapshot server because the port appears in the expected responses. The port is outside the range of 32768..60999 to avoid conflicts with the other tests that use ephemeral ports.