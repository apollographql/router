
chart_version:=$(shell cat Chart.yaml | yq '.version')

.PHONY: auth
auth:
	@gcloud auth print-access-token \
	| helm registry login -u oauth2accesstoken \
	--password-stdin https://us-east1-docker.pkg.dev/

.PHONY: package
package:
	@helm package .

.PHONY: push
push:
	@helm push router-$(chart_version).tgz oci://us-east1-docker.pkg.dev/parsley-artifacts/parsley-helm