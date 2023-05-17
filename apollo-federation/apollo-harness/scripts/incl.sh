#! /usr/bin/env bash

###
# Run an apollo-harness test under heaptrack.
#
# Since heaptrack is linux specific, the best way to do this is by running
# the tests in a container.
###

###
# Terminate the build and clean up the build directory
###
terminate () {
    printf "%s terminating...\n" "${1}"
    exit 1
}

###
# Advise about installation/configuration and then terminate
###
advise () {
    printf "%s\n" "${1}"
    exit 2
}

export install_conman_advice="""
The test harness executes within a container, so your machine must provide some kind of container management facility.

We support:
 - docker
 - podman

You can install/configure them by following the instructions at:

docker
------

https://docs.docker.com/engine/install/

podman
------

linux: (Figure this out for your distro. Likely to be something like 'apt install podman')

macOS: 'brew install podman'. Decide if you are all in on podman, if you are also 'brew install podman-desktop', if not 'podman machine init && podman machine start')

Note: Install/Configuring Docker/Podman could be a fairly complex task, these directions are minimal and should be enough to get you started. There's plenty of documentation on the internet if you want to fine tune your installation.
Once docker/podman is installed, please start the test again.
"""

export install_cross_advice="""
The test harness makes use of the cargo cross plugin to perform cross compiling.

You can install cross as follows:

cargo install cross --git https://github.com/cross-rs/cross

Once cross is installed, please start the test again.
"""
