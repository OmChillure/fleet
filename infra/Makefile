# Agent Fleet Kubernetes install helpers.
# Run from repo root: `make -C infra …` or `make -f infra/Makefile …`

ROOT := $(abspath $(dir $(lastword $(MAKEFILE_LIST)))/..)
IMAGE_API ?= agent-fleet-api:local
IMAGE_UI ?= agent-fleet-ui:local
IMAGE_OP ?= agent-fleet-operator:local
KUSTOMIZE ?= $(ROOT)/infra
KIND_CLUSTER ?= agent-fleet
K3D_CLUSTER ?= agent-fleet
# Required by Dockerfiles; baked into /health.git_sha and UI /fleet-git-sha.txt.
FLEET_GIT_SHA ?= $(shell cd $(ROOT) && bash scripts/tree-id.sh)

.PHONY: print-crd crd images kind-create kind-load apply apply-kind k3d-create k3d-load apply-k3d sample status uninstall k3d-delete

print-crd:
	cd $(ROOT) && cargo run -p fleet-operator --quiet -- print-crd

crd: print-crd
	cd $(ROOT) && cargo run -p fleet-operator --quiet -- print-crd > infra/crds/fleetsandboxes.yaml

images:
	test -n "$(FLEET_GIT_SHA)"
	docker build --build-arg FLEET_GIT_SHA=$(FLEET_GIT_SHA) -t $(IMAGE_API) --label fleet.git_sha=$(FLEET_GIT_SHA) -f $(ROOT)/Dockerfile $(ROOT)
	docker build --build-arg FLEET_GIT_SHA=$(FLEET_GIT_SHA) -t $(IMAGE_UI) --label fleet.git_sha=$(FLEET_GIT_SHA) -f $(ROOT)/Dockerfile.ui $(ROOT)
	docker build --build-arg FLEET_GIT_SHA=$(FLEET_GIT_SHA) -t $(IMAGE_OP) --label fleet.git_sha=$(FLEET_GIT_SHA) -f $(ROOT)/Dockerfile.operator $(ROOT)

kind-create:
	kind create cluster --name $(KIND_CLUSTER) --config $(ROOT)/infra/kind.yaml

kind-load: images
	kind load docker-image $(IMAGE_API) $(IMAGE_UI) $(IMAGE_OP) --name $(KIND_CLUSTER)

apply:
	kubectl apply -k $(KUSTOMIZE)

apply-kind:
	kubectl apply -k $(ROOT)/infra/overlays/kind

k3d-create:
	k3d cluster create --config $(ROOT)/infra/k3d.yaml

k3d-load: images
	k3d image import $(IMAGE_API) $(IMAGE_UI) $(IMAGE_OP) -c $(K3D_CLUSTER)

apply-k3d:
	kubectl apply -k $(ROOT)/infra/overlays/k3d

sample:
	kubectl apply -f $(ROOT)/infra/samples/fleetsandbox.yaml

status:
	kubectl -n agent-fleet get deploy,po,svc,fsb

uninstall:
	kubectl delete -k $(KUSTOMIZE) --ignore-not-found

k3d-delete:
	k3d cluster delete $(K3D_CLUSTER)
