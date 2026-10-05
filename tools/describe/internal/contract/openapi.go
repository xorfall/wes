package contract

import (
	"bytes"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"log/slog"
	"strconv"
	"strings"

	"github.com/pb33f/go-yaml"
	"github.com/pb33f/libopenapi"
	"github.com/pb33f/libopenapi/datamodel"
	v3 "github.com/pb33f/libopenapi/datamodel/high/v3"
)

type openapiSource struct {
	root  map[string]any
	model *libopenapi.DocumentModel[v3.Document]
}

// Parse with libopenapi, keeping exact scalar spellings for native constraints.
// The source node walk bounds aliases/depth before the library builds its index.
func parseOpenAPI(body []byte) (*openapiSource, error) {
	if len(body) > MaxInput {
		return nil, errors.New("OpenAPI document exceeds 4 MiB")
	}
	var root yaml.Node
	decoder := yaml.NewDecoder(bytes.NewReader(body))
	if err := decoder.Decode(&root); err != nil {
		return nil, errors.New("invalid OpenAPI JSON/YAML syntax")
	}
	var trailing yaml.Node
	if err := decoder.Decode(&trailing); err != io.EOF {
		return nil, errors.New("expected one OpenAPI document")
	}
	value, err := sourceValue(&root)
	if err != nil {
		return nil, err
	}
	object, ok := value.(map[string]any)
	if !ok {
		return nil, errors.New("OpenAPI document must be an object")
	}
	version, _ := object["openapi"].(string)
	if !strings.HasPrefix(version, "3.0.") && !strings.HasPrefix(version, "3.1.") {
		return nil, errors.New("expected OpenAPI 3.0 or 3.1 JSON/YAML; prose and HTML are not supported")
	}
	// Check schema refs before indexing; no source may authorize additional I/O.
	var refs func(any, string) error
	refs = func(value any, at string) error {
		switch v := value.(type) {
		case map[string]any:
			for _, key := range keys(v) {
				if key == "$ref" {
					ref, ok := v[key].(string)
					if !ok || !strings.HasPrefix(ref, "#/") {
						return fail(at+"/$ref", "only local JSON-pointer references are supported; external refs are never fetched")
					}
				}
				if err := refs(v[key], at+"/"+pointer(key)); err != nil {
					return err
				}
			}
		case []any:
			for i, x := range v {
				if err := refs(x, fmt.Sprintf("%s/%d", at, i)); err != nil {
					return err
				}
			}
		}
		return nil
	}
	if err := refs(object, "#"); err != nil {
		return nil, err
	}
	config := datamodel.NewDocumentConfiguration()
	config.AllowFileReferences = false
	config.AllowRemoteReferences = false
	config.SkipExternalRefResolution = true
	config.Logger = slog.New(slog.NewTextHandler(io.Discard, nil))
	document, err := libopenapi.NewDocumentWithConfiguration(body, config)
	if err != nil {
		return nil, fmt.Errorf("OpenAPI parse: %w", err)
	}
	model, err := document.BuildV3Model()
	if err != nil {
		return nil, fmt.Errorf("OpenAPI references: %w", err)
	}
	if model == nil || model.Model.Paths == nil || model.Model.Paths.PathItems == nil {
		return nil, errors.New("OpenAPI paths must be an object")
	}
	return &openapiSource{object, model}, nil
}

func sourceValue(root *yaml.Node) (any, error) {
	work := 0
	return sourceValueBudget(root, &work)
}
func sourceValueBudget(root *yaml.Node, work *int) (any, error) {
	var visit func(*yaml.Node, int, string) (any, error)
	visit = func(node *yaml.Node, depth int, at string) (any, error) {
		*work++
		if node == nil || depth > 64 || *work > 100000 {
			return nil, fail(at, "source structural budget exceeded")
		}
		switch node.Kind {
		case yaml.DocumentNode:
			if len(node.Content) != 1 {
				return nil, fail(at, "expected one source document")
			}
			return visit(node.Content[0], depth+1, at)
		case yaml.AliasNode:
			return visit(node.Alias, depth+1, at)
		case yaml.MappingNode:
			value := map[string]any{}
			for i := 0; i < len(node.Content); i += 2 {
				key := node.Content[i]
				if key.Kind != yaml.ScalarNode || (key.Tag != "!!str" && key.Tag != "!!int") {
					return nil, fail(at, "mapping keys must be strings; YAML merge keys are not supported")
				}
				if _, exists := value[key.Value]; exists {
					return nil, fail(at+"/"+pointer(key.Value), "duplicate source key")
				}
				child, err := visit(node.Content[i+1], depth+1, at+"/"+pointer(key.Value))
				if err != nil {
					return nil, err
				}
				value[key.Value] = child
			}
			return value, nil
		case yaml.SequenceNode:
			value := []any{}
			for i, child := range node.Content {
				v, err := visit(child, depth+1, fmt.Sprintf("%s/%d", at, i))
				if err != nil {
					return nil, err
				}
				value = append(value, v)
			}
			return value, nil
		case yaml.ScalarNode:
			switch node.Tag {
			case "!!str":
				return node.Value, nil
			case "!!null":
				if node.Value != "" && node.Value != "~" && !strings.EqualFold(node.Value, "null") {
					return nil, fail(at, "invalid YAML null")
				}
				return nil, nil
			case "!!bool":
				if !strings.EqualFold(node.Value, "true") && !strings.EqualFold(node.Value, "false") {
					return nil, fail(at, "invalid YAML boolean")
				}
				return strings.EqualFold(node.Value, "true"), nil
			case "!!int", "!!float":
				if len(node.Value) == 0 || !strings.ContainsRune("-0123456789", rune(node.Value[0])) || !json.Valid([]byte(node.Value)) {
					return nil, fail(at, "numeric values must use finite JSON decimal notation")
				}
				return json.Number(node.Value), nil
			default:
				return nil, fail(at, "unsupported YAML scalar tag")
			}
		}
		return nil, fail(at, "unsupported source node")
	}
	return visit(root, 0, "#")
}

// Provenance pointers address the bounded source object; reference resolution is
// owned by libopenapi, not this metadata lookup.
func pointerValue(root any, at string) (any, bool) {
	if at == "#" {
		return root, true
	}
	if !strings.HasPrefix(at, "#/") {
		return nil, false
	}
	current := root
	for _, part := range strings.Split(at[2:], "/") {
		part = strings.ReplaceAll(strings.ReplaceAll(part, "~1", "/"), "~0", "~")
		switch value := current.(type) {
		case map[string]any:
			var ok bool
			current, ok = value[part]
			if !ok {
				return nil, false
			}
		case []any:
			i, err := strconv.Atoi(part)
			if err != nil || i < 0 || i >= len(value) {
				return nil, false
			}
			current = value[i]
		default:
			return nil, false
		}
	}
	return current, true
}
