package main

import (
	"encoding/json"
	"errors"
	"strings"
	"testing"
	"wes-describe/internal/contract"
)

func TestFailureReportRetainsCausesWithoutCopyingQuotesAndHasExplicitBudget(t *testing.T) {
	doc := contract.Document{}
	for i := 0; i < 100; i++ {
		doc.Diagnostics = append(doc.Diagnostics, strings.Repeat("ü", 3000))
	}
	report := reportFailure(doc, errors.New(strings.Repeat("ü", 3000)))
	bytes, err := json.Marshal(report)
	if err != nil || len(report.Issues) != 32 || report.Omitted != 68 || len(bytes) > 256*1024 {
		t.Fatal(report.Omitted, len(bytes), err)
	}
}
