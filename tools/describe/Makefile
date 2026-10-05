# Offline acceptance with synthetic OpenAPI documents.

BINARY := wes-extract

.PHONY: help build test check clean

help: ## show this help
	@grep -E '^[a-z-]+:.*##' $(MAKEFILE_LIST) | awk -F':.*## ' '{printf "  %-8s %s\n", $$1, $$2}'

build: ## compile the command
	go build -o $(BINARY) ./cmd/extract

test: ## run the tests
	go test ./...

check: ## everything a change has to pass
	gofmt -l . | tee /dev/stderr | (! read)
	go vet ./...
	go test ./...

clean: ## remove what was built
	rm -f $(BINARY)
