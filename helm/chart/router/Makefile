
chart_version:=$(shell cat Chart.yaml | yq '.version')

.PHONY: package
package:
	@helm package .

.PHONY: push
push:
	@helm push router-$(chart_version).tgz oci://us-east1-docker.pkg.dev/parsley-artifacts/parsley-helm