package contract

import (
	"strings"
	"testing"
)

func checkedProvenance(t *testing.T, d Document) []ProvenanceEntry {
	t.Helper()
	b, err := d.Marshal()
	if err != nil {
		t.Fatal(err)
	}
	root, err := Decode(b)
	if err != nil {
		t.Fatal(err)
	}
	p := d.Source.Provenance
	if p == nil || p.Version != 1 || p.Status != "current" || len(p.Entries) == 0 {
		t.Fatal(p)
	}
	seen := map[string]bool{}
	for _, e := range p.Entries {
		if _, ok := pointerValue(root, e.Target); !ok {
			t.Fatalf("dangling final target: %+v", e)
		}
		if e.Source != "sha256:"+d.Source.SHA256 || e.Reason == "" || !contains([]string{"documented", "example", "inferred", "unknown"}, e.Basis) {
			t.Fatal(e)
		}
		k := e.Target + "\x00" + e.Pointer
		if seen[k] {
			t.Fatal("duplicate evidence", e)
		}
		seen[k] = true
	}
	return p.Entries
}

func TestProvenanceFollowsFinalTypesAndOriginalRefs(t *testing.T) {
	d, err := Extract(fixture(t), "inventory", false)
	if err != nil {
		t.Fatal(err)
	}
	entries := checkedProvenance(t, d)
	found := false
	for _, e := range entries {
		if strings.HasSuffix(e.Target, "/fields/id/type") && strings.HasPrefix(e.Pointer, "#/components/schemas/Item/") {
			found = true
		}
		if len(e.Lines) != 0 {
			t.Fatal("deterministic JSON must not invent prose citations", e)
		}
	}
	if !found {
		t.Fatal("component property not linked to generated field", entries)
	}
	partial, err := Extract(edit(t, func(m map[string]any) {
		object(object(m["components"])["schemas"])["Item"] = map[string]any{"oneOf": []any{map[string]any{"type": "string"}}}
	}), "partial", true)
	if err != nil {
		t.Fatal(err)
	}
	checkedProvenance(t, partial)
}

func TestProvenanceTracksParameterOverrideAndAllOfFields(t *testing.T) {
	input := []byte(`{"openapi":"3.1.0","paths":{"/items":{"parameters":[{"name":"q","in":"query","schema":{"type":"integer"}}],"get":{"parameters":[{"name":"q","in":"query","schema":{"type":"string"}}],"responses":{"200":{"content":{"application/json":{"schema":{"allOf":[{"type":"object","properties":{"a":{"type":"integer"}}},{"type":"object","properties":{"b":{"type":"string"}}}]}}}}}}}}}`)
	d, err := Extract(input, "demo", false)
	if err != nil {
		t.Fatal(err)
	}
	entries := checkedProvenance(t, d)
	override, a, b := false, false, false
	for _, e := range entries {
		if e.Target == "#/operations/0/parameters/0/type" {
			override = e.Pointer == "#/paths/~1items/get/parameters/0/schema/type"
		}
		if strings.HasSuffix(e.Target, "/fields/a/type") {
			a = strings.Contains(e.Pointer, "/allOf/0/properties/a/type")
		}
		if strings.HasSuffix(e.Target, "/fields/b/type") {
			b = strings.Contains(e.Pointer, "/allOf/1/properties/b/type")
		}
	}
	if !override || !a || !b {
		t.Fatal(entries)
	}
}
