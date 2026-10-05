package contract

import (
	"bytes"
	"encoding/json"
	"github.com/pb33f/go-yaml"
	"strings"
	"testing"
)

func TestJSONYAMLParityAndEditableProjection(t *testing.T) {
	source, err := Decode(fixture(t))
	if err != nil {
		t.Fatal(err)
	}
	yamlSource, err := yaml.Marshal(source)
	if err != nil {
		t.Fatal(err)
	}
	// json.Number is a string to YAML's encoder; decode ordinary fixture scalars first.
	if err := json.Unmarshal(fixture(t), &source); err != nil {
		t.Fatal(err)
	}
	yamlSource, err = yaml.Marshal(source)
	if err != nil {
		t.Fatal(err)
	}
	a, err := Extract(fixture(t), "inventory", false)
	if err != nil {
		t.Fatal(err)
	}
	b, err := Extract(yamlSource, "inventory", false)
	if err != nil {
		t.Fatal(err)
	}
	b.Source.SHA256 = a.Source.SHA256
	for i := range b.Source.Provenance.Entries {
		b.Source.Provenance.Entries[i].Source = "sha256:" + a.Source.SHA256
	}
	ab, _ := a.Marshal()
	bb, _ := b.Marshal()
	if !bytes.Equal(ab, bb) {
		t.Fatalf("JSON/YAML differ:\n%s\n%s", ab, bb)
	}
	draft, err := EditableFromDocument(a)
	if err != nil {
		t.Fatal(err)
	}
	if draft.Draft["draftVersion"] != 1 || len(list(draft.Draft["operations"])) != 3 {
		t.Fatal(draft)
	}
	for _, entry := range draft.Source.Provenance.Entries {
		if _, ok := pointerValue(draft.Draft, entry.Target); !ok {
			t.Fatalf("dangling draft evidence: %+v", entry)
		}
	}
	after, _ := a.Marshal()
	if !bytes.Equal(ab, after) {
		t.Fatal("draft projection mutated source evidence")
	}
}

func TestTypedAlternativesAndErrorResponses(t *testing.T) {
	for _, keyword := range []string{"anyOf", "oneOf"} {
		source := edit(t, func(m map[string]any) {
			object(object(m["components"])["schemas"])["Item"] = map[string]any{keyword: []any{map[string]any{"type": "string"}, map[string]any{"type": "integer"}}}
		})
		doc, err := Extract(source, "fixture", false)
		if err != nil {
			t.Fatal(err, doc.Diagnostics)
		}
		body, _ := doc.Marshal()
		if !strings.Contains(string(body), `Union\u003cText,Int\u003e`) {
			t.Fatal(string(body))
		}
	}
	source := []byte(`{"openapi":"3.1.0","info":{"title":"Fixture","version":"1"},"paths":{"/items":{"get":{"responses":{"200":{"description":"OK","content":{"application/json":{"schema":{}}}},"400":{"description":"Bad input","content":{"application/json":{"schema":{"type":"string"}}}}}}}}}`)
	doc, err := Extract(source, "fixture", false)
	if err != nil {
		t.Fatal(err, doc.Diagnostics)
	}
	if *doc.Operations[0].Responses["400"] != "Text" || *doc.Operations[0].Responses["200"] != "Unknown" {
		t.Fatal(doc.Operations)
	}
}

func TestOpenAPISourceBoundaries(t *testing.T) {
	for _, source := range []string{
		"openapi: 3.1.0\npaths: {}\npaths: {}", "openapi: 3.1.0\npaths: {}\n---\npaths: {}",
		"openapi: 3.1.0\npaths: {}\nx: &x [*x]", "openapi: 3.1.0\npaths: {}\nx: !!bool maybe", "openapi: 3.1.0\npaths: {}\nx: !!null maybe", "openapi: 3.2.0\npaths: {}",
		`{"openapi":"3.1.0","paths":{},"components":{"schemas":{"Item":{"$ref":"https://never-fetch.invalid/a"}}}}`,
		strings.Repeat("[", 70) + strings.Repeat("]", 70),
	} {
		if _, err := Extract([]byte(source), "fixture", false); err == nil {
			t.Fatalf("accepted %s", source)
		}
	}
}

func TestNullableExclusiveUnionIsNotWidened(t *testing.T) {
	source := edit(t, func(m map[string]any) {
		object(object(m["components"])["schemas"])["Item"] = map[string]any{"oneOf": []any{map[string]any{"type": "string", "nullable": true}, map[string]any{"type": "integer", "nullable": true}}}
	})
	if _, err := Extract(source, "fixture", false); err == nil {
		t.Fatal("accepted overlapping null branches")
	}
}

func TestTaggedOneOfUsesConstAndPreservesItsEvidence(t *testing.T) {
	source := edit(t, func(m map[string]any) {
		branch := func(tag string) any {
			return map[string]any{"type": "object", "required": []any{"kind"}, "properties": map[string]any{"kind": map[string]any{"const": tag}}}
		}
		object(object(m["components"])["schemas"])["Item"] = map[string]any{"oneOf": []any{branch("text"), branch("tool")}}
	})
	doc, err := Extract(source, "fixture", false)
	if err != nil {
		t.Fatal(err, doc.Diagnostics)
	}
	found := 0
	for _, entry := range checkedProvenance(t, doc) {
		if strings.HasSuffix(entry.Target, "/enum") {
			if !strings.HasSuffix(entry.Pointer, "/const") || entry.Basis != "documented" {
				t.Fatal(entry)
			}
			found++
		}
	}
	if found < 2 {
		t.Fatal("lost discriminator constraints", found)
	}
}
