package contract

import (
	"encoding/json"
	"testing"
)

func TestDocumentationSurvivesEditableConversionWithoutChangingContracts(t *testing.T) {
	body := []byte(`{"openapi":"3.1.0","paths":{"/items":{"post":{"operationId":"createItem","summary":"Create an item","description":"A **documented** operation.\n\n` + "```json\\n{}\\n```" + `","parameters":[{"in":"query","name":"limit","description":"Maximum items.","schema":{"type":"integer"}}],"requestBody":{"description":"The item to create.","content":{"application/json":{"schema":{"$ref":"#/components/schemas/Item"}}}},"responses":{"201":{"description":"Created item.","content":{"application/json":{"schema":{"$ref":"#/components/schemas/Item"}}}},"400":{"description":"Invalid input."},"500":{"description":"Other errors."}}}}},"components":{"schemas":{"Item":{"type":"object","description":"An inventory item.","properties":{"name":{"type":"string","description":"Human readable name."},"owner":{"$ref":"#/components/schemas/Owner","description":"Owner for this item."}},"required":["name"]},"Owner":{"type":"object","description":"Canonical owner.","properties":{"id":{"type":"integer"}}}}}}`)
	d, err := Extract(body, "synthetic", false)
	if err != nil {
		t.Fatal(err)
	}
	op := d.Operations[0]
	if op.Summary != "Create an item" || op.Description == "" || op.Parameters[0].Description != "Maximum items." || op.Parameters[1].Description != "The item to create." {
		t.Fatalf("lost operation docs: %+v", op)
	}
	if op.ResponseDescriptions["201"] != "Created item." || op.ResponseDescriptions["400"] != "Invalid input." || op.ResponseDescriptions["500"] != "Other errors." {
		t.Fatal(op.ResponseDescriptions)
	}
	if len(op.Responses) != 3 || op.Responses["201"] == nil {
		t.Fatal("lost documented HTTP response contracts")
	}
	var foundItem, foundOwner bool
	for _, raw := range d.Types {
		def := object(raw)
		if def["description"] == "Canonical owner." {
			foundOwner = true
		}
		fields := object(def["fields"])
		if object(fields["name"])["description"] == "Human readable name." {
			foundItem = true
			if def["description"] != "An inventory item." || object(fields["owner"])["description"] != "Owner for this item." {
				t.Fatal(def)
			}
			if object(fields["name"])["optional"] != false {
				t.Fatal("changed requiredness")
			}
		}
	}
	if !foundItem || !foundOwner {
		t.Fatal("lost schema docs or changed shared reference", d.Types)
	}
	editable, err := EditableFromDocument(d)
	if err != nil {
		t.Fatal(err)
	}
	encoded, err := editable.Marshal()
	if err != nil {
		t.Fatal(err)
	}
	var decoded map[string]any
	if err := json.Unmarshal(encoded, &decoded); err != nil {
		t.Fatal(err)
	}
	restored := object(list(object(decoded["draft"])["operations"])[0])
	if restored["description"] != op.Description || object(restored["responseDescriptions"])["400"] != "Invalid input." {
		t.Fatal(restored)
	}
}

func TestUnresolvedErrorResponseIsNotSilentlyDiscarded(t *testing.T) {
	body := []byte(`{"openapi":"3.0.3","paths":{"/health":{"get":{"responses":{"204":{"description":"Healthy"},"default":{"$ref":"https://never-fetch.invalid/errors.json"}}}}}}`)
	if _, err := Extract(body, "synthetic", false); err == nil {
		t.Fatal("discarded unresolved error response")
	}
}
