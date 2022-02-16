###
# Note: This makefile make use of the cargo get plugin.
#
# Please make sure it is available. (If not, install as cargo install cargo-get)
###
SHELL=/bin/bash
MAKEFLAGS += -j2

%-router clean-router: TGT = router
%-router: VERSION = $(shell cargo get --root=apollo-router version)

%-spaceport: TGT = spaceport
%-spaceport: VERSION = $(shell cargo get --root=apollo-spaceport version)

run-router:
	docker run -it --rm -p 4000:4000 -v `pwd`/config:/config -v /Users/garypen/dev/router/../router-perf-testing:/router-perf-testing $(TGT):$(VERSION) -c /config/usage_graph.yml -s /router-perf-testing/local-config/supergraph.graphql

run-spaceport:
	docker run -it --rm -p 51005:51005 $(TGT):$(VERSION)

build: build-router build-spaceport

clean: clean-router clean-spaceport

build-%:
	docker build -t $(TGT):$(VERSION) --no-cache -f dockerfiles/$(TGT)/Dockerfile .

clean-%:
	- docker rmi $(TGT):$(VERSION)

