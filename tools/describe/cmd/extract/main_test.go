package main

import (
	"fmt"
	"testing"
)

func TestStableIntegrationFailureCodes(t *testing.T) {
	for _, code := range []int{21, 22} {
		err := &extractionFailure{code: code, err: fmt.Errorf("untrusted diagnostic")}
		if exitCode(fmt.Errorf("wrapped: %w", err)) != code {
			t.Fatal(code)
		}
	}
	if exitCode(fmt.Errorf("ordinary error")) != 1 {
		t.Fatal("unexpected generic code")
	}
}
