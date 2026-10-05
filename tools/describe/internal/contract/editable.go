package contract

import (
	"encoding/json"
	"errors"
	"fmt"
	"strings"
)

type EditableDraft struct {
	Draft  map[string]any `json:"draft"`
	Source Evidence       `json:"source"`
}

func EditableFromDocument(doc Document) (EditableDraft, error) {
	// Evidence belongs to this projection; never mutate the source descriptor.
	if doc.Source.Provenance != nil {
		p := *doc.Source.Provenance
		p.Entries = append([]ProvenanceEntry(nil), p.Entries...)
		doc.Source.Provenance = &p
	}
	bytes, err := doc.Marshal()
	if err != nil {
		return EditableDraft{}, err
	}
	draft, err := Decode(bytes)
	if err != nil {
		return EditableDraft{}, err
	}
	delete(draft, "version")
	delete(draft, "source")
	draft["draftVersion"] = 1
	draft["problems"] = []any{}
	for i, raw := range list(draft["operations"]) {
		op := object(raw)
		responses := object(op["responses"])
		items := []any{}
		for j, status := range keys(responses) {
			var media any = "application/json"
			if responses[status] == nil {
				media = nil
			}
			items = append(items, map[string]any{"status": json.Number(status), "mediaType": media, "type": responses[status]})
			// Rewrite only evidence targets; source pointers still identify original OpenAPI fields.
			old := fmt.Sprintf("#/operations/%d/responses/%s", i, pointer(status))
			next := fmt.Sprintf("#/operations/%d/responses/%d", i, j)
			if doc.Source.Provenance != nil {
				for k := range doc.Source.Provenance.Entries {
					p := &doc.Source.Provenance.Entries[k]
					if p.Target == old {
						p.Target = next + "/status"
					} else if strings.HasPrefix(p.Target, old+"/") {
						p.Target = next + strings.TrimPrefix(p.Target, old)
					}
				}
			}
		}
		op["responses"] = items
	}
	return EditableDraft{Draft: draft, Source: doc.Source}, nil
}
func (d EditableDraft) Marshal() ([]byte, error) {
	b, err := json.MarshalIndent(d, "", "  ")
	if err != nil {
		return nil, err
	}
	if len(b) > MaxOutput {
		return nil, errors.New("draft envelope exceeds 1 MiB")
	}
	root, err := Decode(b)
	if err != nil {
		return nil, err
	}
	count := 0
	var visit func(any)
	visit = func(v any) {
		count++
		switch v := v.(type) {
		case map[string]any:
			for _, x := range v {
				visit(x)
			}
		case []any:
			for _, x := range v {
				visit(x)
			}
		}
	}
	visit(root)
	if count > 20000 {
		return nil, errors.New("draft envelope exceeds 20000 nodes")
	}
	return append(b, '\n'), nil
}
