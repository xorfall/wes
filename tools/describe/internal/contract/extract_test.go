package contract

import (
	"bytes"
	"context"
	"encoding/json"
	"net/http"
	"net/http/httptest"
	"os"
	"strings"
	"testing"
)

func fixture(t *testing.T) []byte {
	t.Helper()
	b, e := os.ReadFile("../../../../examples/api-import/openapi.json")
	if e != nil {
		t.Fatal(e)
	}
	return b
}
func edit(t *testing.T, f func(map[string]any)) []byte {
	t.Helper()
	m, e := Decode(fixture(t))
	if e != nil {
		t.Fatal(e)
	}
	f(m)
	b, e := json.Marshal(m)
	if e != nil {
		t.Fatal(e)
	}
	return b
}
func TestFixture(t *testing.T) {
	d, e := Extract(fixture(t), "inventory", false)
	if e != nil {
		t.Fatal(e, d.Diagnostics)
	}
	if d.Source.Discovered != 3 || d.Source.Emitted != 3 || d.Source.Skipped != 0 {
		t.Fatal(d.Source)
	}
	if len(d.Servers) != 1 {
		t.Fatal("lost review servers")
	}
	b, e := d.Marshal()
	if e != nil {
		t.Fatal(e)
	}
	if bytes.Contains(b, []byte(`"base": "http`)) {
		t.Fatal("executable server default")
	}
	again, _ := Extract(fixture(t), "inventory", false)
	b2, _ := again.Marshal()
	if !bytes.Equal(b, b2) {
		t.Fatal("nondeterministic descriptor")
	}
}
func TestRequiredNullableMapAndReference(t *testing.T) {
	d, e := Extract(fixture(t), "fixture", false)
	if e != nil {
		t.Fatal(e)
	}
	found := false
	for _, v := range d.Types {
		fields := object(object(v)["fields"])
		if fields["note"] != nil {
			found = true
			n := object(fields["note"])
			if !flag(n["optional"]) || !strings.HasPrefix(text(n["type"]), "Option<") {
				t.Fatal(n)
			}
			if !strings.HasPrefix(text(object(fields["labels"])["type"]), "Map<Text,") {
				t.Fatal(fields["labels"])
			}
		}
	}
	if !found {
		t.Fatal("lost nested Item")
	}
}
func TestUnsupportedNeverSilentlyWidens(t *testing.T) {
	for _, schema := range []any{
		map[string]any{"oneOf": []any{map[string]any{"type": "number"}, map[string]any{"type": "integer"}}},
		map[string]any{"type": "object", "additionalProperties": false},
		map[string]any{"$ref": "https://never-fetch.invalid/schema.json"},
		map[string]any{"type": "string", "pattern": "[a-z]+"},
		map[string]any{"type": "object", "properties": map[string]any{"child": map[string]any{"$ref": "#/components/schemas/Item"}}},
	} {
		b := edit(t, func(m map[string]any) { object(object(m["components"])["schemas"])["Item"] = schema })
		d, e := Extract(b, "fixture", false)
		if e == nil {
			t.Fatalf("silent widening: %v %v", schema, d)
		}
		if d.Source.Format == "" {
			continue
		} // Invalid references reject the source before operation lowering.
		partial, e := Extract(b, "fixture", true)
		if e != nil || partial.Source.Emitted != 1 || len(partial.Diagnostics) == 0 {
			t.Fatal(e, partial)
		}
	}
}
func TestParameterOverrideAndSecurityInheritance(t *testing.T) {
	b := edit(t, func(m map[string]any) {
		object(m["components"])["securitySchemes"] = map[string]any{"token": map[string]any{"type": "http", "scheme": "bearer"}}
		m["security"] = []any{map[string]any{"token": []any{}}}
		p := object(object(m["paths"])["/items/{id}"])
		get := object(p["get"])
		delete(get, "security")
		get["parameters"] = []any{map[string]any{"name": "id", "in": "path", "required": true, "schema": map[string]any{"type": "integer"}}}
	})
	d, e := Extract(b, "fixture", false)
	if e != nil {
		t.Fatal(e, d.Diagnostics)
	}
	get := d.Operations[0]
	if len(get.Auth) != 1 || len(get.Parameters) != 1 || get.Parameters[0].Type != "Int" {
		t.Fatal(get)
	}
	if len(d.Operations[1].Auth) != 0 {
		t.Fatal("public override lost")
	}
	b = edit(t, func(m map[string]any) {
		object(object(object(m["paths"])["/items/{id}"])["get"])["security"] = []any{map[string]any{}, map[string]any{}}
	})
	if _, e := Extract(b, "fixture", false); e == nil {
		t.Fatal("chose a security alternative")
	}
}
func TestNullableAnyOfAndFlatAllOf(t *testing.T) {
	for _, schema := range []any{
		map[string]any{"anyOf": []any{map[string]any{"type": "string"}, map[string]any{"type": "null"}}},
		map[string]any{"allOf": []any{map[string]any{"type": "object", "properties": map[string]any{"id": map[string]any{"type": "integer"}}, "required": []any{"id"}}, map[string]any{"type": "object", "properties": map[string]any{"name": map[string]any{"type": "string"}}}}},
	} {
		b := edit(t, func(m map[string]any) { object(object(m["components"])["schemas"])["Item"] = schema })
		if d, e := Extract(b, "fixture", false); e != nil {
			t.Fatal(e, d.Diagnostics)
		}
	}
}
func TestStrictBounds(t *testing.T) {
	for _, s := range []string{`{"x":1,"x":2}`, `{} {}`, strings.Repeat("[", 70) + strings.Repeat("]", 70), strings.Repeat(" ", MaxInput+1)} {
		if _, e := Decode([]byte(s)); e == nil {
			t.Fatal("accepted invalid/excessive JSON")
		}
	}
	if _, e := Read(context.Background(), "-", strings.NewReader(strings.Repeat("x", MaxInput+1))); e == nil {
		t.Fatal("stdin silently truncated")
	}
}
func TestMalformedContractsFailBeforeOutput(t *testing.T) {
	for _, schema := range []any{
		map[string]any{"type": "integer", "minimum": json.Number("10"), "maximum": json.Number("1")},
		map[string]any{"type": "string", "enum": []any{json.Number("2")}},
		map[string]any{"type": "string", "minLength": json.Number("-1")},
		map[string]any{"type": "integer", "minimum": json.Number("1e999999999")},
		map[string]any{"type": "object", "required": "id"},
	} {
		b := edit(t, func(m map[string]any) { object(object(m["components"])["schemas"])["Item"] = schema })
		if _, err := Extract(b, "fixture", false); err == nil {
			t.Fatal("malformed schema was emitted", schema)
		}
	}
}
func TestURLBoundaries(t *testing.T) {
	s := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		switch r.URL.Path {
		case "/ok":
			w.Write([]byte(`{}`))
		case "/redirect":
			http.Redirect(w, r, "https://never-fetch.invalid", 302)
		case "/large":
			w.Write(bytes.Repeat([]byte{'x'}, MaxInput+1))
		}
	}))
	defer s.Close()
	if b, e := Read(context.Background(), s.URL+"/ok", nil); e != nil || string(b) != "{}" {
		t.Fatal(e)
	}
	for _, path := range []string{"/redirect", "/large", "/ok?token=never-print"} {
		if _, e := Read(context.Background(), s.URL+path, nil); e == nil || strings.Contains(e.Error(), "never-print") {
			t.Fatal("URL safety failure", e)
		}
	}
}
