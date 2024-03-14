#! /opt/homebrew/bin/bash

###
#  Loop for up to 10 seconds waiting for a port to become active
###

for i in {1..10}; do
    # If nc returns 0, we are ready so it's time to break
    nc -z localhost 3000 && break
    sleep 1
done
