// Package contract extracts reviewed API contracts, never execution destinations.
package contract

import (
	"bytes"
	"encoding/json"
	"errors"
	"fmt"
	"io"
)

const MaxInput = 4 << 20
const MaxOutput = 1 << 20

type Document struct {
	Version     int            `json:"version"`
	Provider    string         `json:"provider"`
	Types       map[string]any `json:"types"`
	Operations  []Operation    `json:"operations"`
	Source      Evidence       `json:"source"`
	Servers     []any          `json:"servers"`
	Diagnostics []string       `json:"diagnostics"`
	origins     []origin
}
type Evidence struct {
	SHA256     string      `json:"sha256"`
	Format     string      `json:"format"`
	Discovered int         `json:"discovered"`
	Emitted    int         `json:"emitted"`
	Skipped    int         `json:"skipped"`
	Provenance *Provenance `json:"provenance,omitempty"`
}
type AuthOption struct {
	Schemes []string `json:"schemes"`
	Auth    []any    `json:"auth"`
}
type Operation struct {
	Path                 []string           `json:"path"`
	Summary              string             `json:"summary"`
	Description          string             `json:"description,omitempty"`
	ResponseDescriptions map[string]string  `json:"responseDescriptions,omitempty"`
	Method               string             `json:"method"`
	Route                string             `json:"route"`
	Auth                 []any              `json:"auth"`
	AuthOptions          []AuthOption       `json:"authOptions,omitempty"`
	Parameters           []Parameter        `json:"parameters"`
	Responses            map[string]*string `json:"responses"`
	Evidence             string             `json:"evidence"`
}
type Parameter struct {
	Name        string `json:"name"`
	Description string `json:"description,omitempty"`
	Wire        string `json:"wire"`
	Location    string `json:"location"`
	Type        string `json:"type"`
	Required    bool   `json:"required"`
	Encoding    string `json:"encoding"`
}

// Decode rejects duplicate keys, trailing documents, excessive depth and work before extraction.
func Decode(body []byte) (map[string]any, error) {
	if len(body) > MaxInput {
		return nil, errors.New("documentation exceeds 4 MiB")
	}
	d := json.NewDecoder(bytes.NewReader(body))
	d.UseNumber()
	nodes := 0
	var read func(int) (any, error)
	read = func(depth int) (any, error) {
		nodes++
		if depth > 64 || nodes > 100000 {
			return nil, errors.New("JSON structural budget exceeded")
		}
		token, err := d.Token()
		if err != nil {
			return nil, errors.New("invalid JSON documentation")
		}
		switch token {
		case json.Delim('{'):
			result := map[string]any{}
			for d.More() {
				k, e := d.Token()
				if e != nil {
					return nil, e
				}
				key, ok := k.(string)
				if !ok {
					return nil, errors.New("invalid key")
				}
				if _, exists := result[key]; exists {
					return nil, errors.New("duplicate JSON key")
				}
				v, e := read(depth + 1)
				if e != nil {
					return nil, e
				}
				result[key] = v
			}
			_, err = d.Token()
			return result, err
		case json.Delim('['):
			result := []any{}
			for d.More() {
				v, e := read(depth + 1)
				if e != nil {
					return nil, e
				}
				result = append(result, v)
			}
			_, err = d.Token()
			return result, err
		default:
			return token, nil
		}
	}
	result, err := read(0)
	if err != nil {
		return nil, err
	}
	if _, e := d.Token(); e != io.EOF {
		return nil, errors.New("trailing JSON")
	}
	obj, ok := result.(map[string]any)
	if !ok {
		return nil, errors.New("documentation must be a JSON object")
	}
	return obj, nil
}
func (d Document) Marshal() ([]byte, error) {
	body, err := json.MarshalIndent(d, "", "  ")
	if err != nil {
		return nil, err
	}
	if len(body) > MaxOutput {
		return nil, errors.New("descriptor exceeds runtime 1 MiB budget")
	}
	// Runtime's shared raw reader has a tighter node budget than documentation extraction.
	var value any
	decoder := json.NewDecoder(bytes.NewReader(body))
	decoder.UseNumber()
	if err = decoder.Decode(&value); err != nil {
		return nil, err
	}
	count := 0
	var visit func(any)
	visit = func(v any) {
		count++
		switch v := v.(type) {
		case map[string]any:
			for _, c := range v {
				visit(c)
			}
		case []any:
			for _, c := range v {
				visit(c)
			}
		}
	}
	visit(value)
	if count > 20000 {
		return nil, fmt.Errorf("descriptor has %d nodes; runtime maximum is 20000", count)
	}
	return append(body, '\n'), nil
}
