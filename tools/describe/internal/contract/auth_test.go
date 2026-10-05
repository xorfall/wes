package contract

import (
	"bytes"
	"encoding/json"
	"os"
	"testing"
)

func TestAuthAlternativesExampleMatchesCheckedInDescriptor(t *testing.T) {
	source, err := os.ReadFile("../../../../examples/http-auth-alternatives/openapi.json")
	if err != nil {
		t.Fatal(err)
	}
	d, err := Extract(source, "demo", false)
	if err != nil {
		t.Fatal(err, d.Diagnostics)
	}
	actual, err := d.Marshal()
	if err != nil {
		t.Fatal(err)
	}
	expected, err := os.ReadFile("../../../../examples/http-auth-alternatives/service.json")
	if err != nil {
		t.Fatal(err)
	}
	if !bytes.Equal(actual, expected) {
		t.Fatal("regenerate the example descriptor from the actual OpenAPI file")
	}
	var protected Operation
	for _, op := range d.Operations {
		if op.Path[0] == "listItems" {
			protected = op
		} else if len(op.Auth) != 0 || len(op.AuthOptions) != 1 || len(op.AuthOptions[0].Schemes) != 0 {
			t.Fatal("public override lost")
		}
	}
	if len(protected.AuthOptions) != 2 || len(protected.Auth) != 0 || len(protected.AuthOptions[0].Auth) != 2 {
		t.Fatal("AND/OR semantics lost", protected)
	}
	basic := object(protected.AuthOptions[1].Auth[0])
	if basic["userSecret"] != "basic.username" || basic["secret"] != "basic.password" {
		t.Fatal(basic)
	}
	draft, err := EditableFromDocument(d)
	if err != nil {
		t.Fatal(err)
	}
	if _, err = draft.Marshal(); err != nil {
		t.Fatal(err)
	}
	for _, op := range list(draft.Draft["operations"]) {
		o := object(op)
		if list(o["path"])[0] == "listItems" && len(list(o["authOptions"])) != 2 {
			t.Fatal("draft roundtrip lost alternatives")
		}
	}
}
func TestAnonymousAlternativeAndUnsupportedSchemesStayExplicit(t *testing.T) {
	source, _ := os.ReadFile("../../../../examples/http-auth-alternatives/openapi.json")
	for _, mode := range []string{"anonymous", "cookie", "oauth", "duplicate", "invalid"} {
		root, _ := Decode(source)
		switch mode {
		case "anonymous":
			root["security"] = []any{map[string]any{}, map[string]any{"basic": []any{}}}
		case "cookie":
			object(object(object(root["components"])["securitySchemes"])["apiKey"])["in"] = "cookie"
		case "oauth":
			root["security"] = []any{map[string]any{"basic": []any{"scope"}}}
		case "duplicate":
			root["security"] = []any{map[string]any{}, map[string]any{}}
		case "invalid":
			root["security"] = []any{nil}
		}
		b, _ := json.Marshal(root)
		d, err := Extract(b, "demo", false)
		if mode != "anonymous" {
			if err == nil {
				t.Fatalf("accepted %s", mode)
			}
			continue
		}
		if err != nil {
			t.Fatal(err)
		}
		for _, op := range d.Operations {
			if op.Path[0] == "listItems" && (len(op.AuthOptions) != 2 || len(op.AuthOptions[0].Schemes) != 0 || len(op.AuthOptions[0].Auth) != 0) {
				t.Fatal(op)
			}
		}
	}
}

func TestExplicitAnonymousSecurityDiffersFromMissingSecurity(t *testing.T) {
	root := map[string]any{"openapi": "3.0.3", "info": map[string]any{"title": "Inventory", "version": "1"}, "paths": map[string]any{"/items": map[string]any{"get": map[string]any{"operationId": "items", "security": []any{}, "responses": map[string]any{"200": map[string]any{"description": "OK"}}}}}}
	body, _ := json.Marshal(root)
	doc, err := Extract(body, "inventory", false)
	if err != nil {
		t.Fatal(err)
	}
	if len(doc.Operations[0].AuthOptions) != 1 {
		t.Fatal("explicit no-auth choice missing")
	}
	delete(object(object(object(root["paths"])["/items"])["get"]), "security")
	body, _ = json.Marshal(root)
	unknown, err := Extract(body, "inventory", false)
	if err != nil {
		t.Fatal(err)
	}
	if len(unknown.Operations[0].AuthOptions) != 0 || len(unknown.Diagnostics) == 0 {
		t.Fatal("unknown auth lost its advisory")
	}
}
