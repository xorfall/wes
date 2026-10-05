package main

import (
	"unicode/utf8"
	"wes-describe/internal/contract"
)

// Private bounded import diagnostics, never runtime authority or public error text.
type failureReport struct {
	Version int            `json:"version"`
	Message string         `json:"message"`
	Issues  []failureIssue `json:"issues"`
	Omitted int            `json:"omitted"`
}
type failureIssue struct {
	Kind      string      `json:"kind"`
	Operation string      `json:"operation"`
	Message   string      `json:"message"`
	Lines     []lineRange `json:"lines"`
}
type lineRange struct {
	Start int `json:"start"`
	End   int `json:"end"`
}

func bounded(text string, limit int) string {
	if len(text) <= limit {
		return text
	}
	text = text[:limit-3]
	for !utf8.ValidString(text) {
		text = text[:len(text)-1]
	}
	return text + "…"
}
func reportFailure(doc contract.Document, err error) *failureReport {
	r := &failureReport{Version: 1, Message: bounded(err.Error(), 2048), Issues: []failureIssue{}}
	add := func(message string) {
		if len(r.Issues) == 32 {
			r.Omitted++
			return
		}
		r.Issues = append(r.Issues, failureIssue{"validation", "", bounded(message, 2048), []lineRange{}})
	}
	for _, note := range doc.Diagnostics {
		add(note)
	}
	return r
}
