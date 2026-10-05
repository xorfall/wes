package main

import (
	"bytes"
	"encoding/json"
	"flag"
	"os"
	"os/exec"
	"strings"
	"testing"
)

func TestExtractionCLIProcess(t *testing.T) {
	if os.Getenv("WES_EXTRACT_TEST_CHILD") != "1" {
		return
	}
	for i, argument := range os.Args {
		if argument == "--" {
			os.Args = append(os.Args[:1], os.Args[i+1:]...)
			break
		}
	}
	flag.CommandLine = flag.NewFlagSet("extract", flag.ExitOnError)
	diagnosticsJSON = flag.Bool("diagnostics-json", false, "")
	main()
	os.Exit(0)
}

func TestCLIHasNoAutomaticModelSelection(t *testing.T) {
	structured := `{"openapi":"3.0.3","info":{"title":"synthetic","version":"1"},"paths":{"/items":{"get":{"operationId":"items","responses":{"200":{"description":"items","content":{"application/json":{"schema":{"type":"string"}}}}}}}}}`
	for _, test := range []struct {
		name, source string
		arguments    []string
		success      bool
	}{
		{"OpenAPI without model", structured, nil, true},
		{"prose without model", "Read all items with GET /items.", nil, false},
		{"retired automatic flag", structured, []string{"-auto"}, false},
		{"model flags are not supported", "Read all items.", []string{"-model", "synthetic"}, false},
	} {
		t.Run(test.name, func(t *testing.T) {
			arguments := append([]string{"-test.run=^TestExtractionCLIProcess$", "--", "-provider", "fixture", "-from", "-"}, test.arguments...)
			command := exec.Command(os.Args[0], arguments...)
			command.Env = []string{"WES_EXTRACT_TEST_CHILD=1"}
			command.Stdin = strings.NewReader(test.source)
			var stderr bytes.Buffer
			command.Stderr = &stderr
			output, err := command.Output()
			if (err == nil) != test.success {
				t.Fatalf("err=%v stderr=%s", err, stderr.String())
			}
			if test.success {
				var document map[string]any
				if err := json.Unmarshal(output, &document); err != nil || document["version"] != float64(1) {
					t.Fatalf("%s: %v", output, err)
				}
			} else if len(output) != 0 {
				t.Fatalf("rejected source produced output: %s", output)
			}
			if strings.Contains(stderr.String(), "Explicit model mode") {
				t.Fatal("unexpected model invocation")
			}
		})
	}
}
