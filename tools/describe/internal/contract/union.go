package contract

import "encoding/json"

// Native Union accepts any matching branch. oneOf needs exactly one, so lower it
// only when declared JSON kinds or required scalar tags prove branches disjoint.
func (e *extractor) disjoint(parts []any, where string) error {
	nodes := []map[string]any{}
	for _, part := range parts {
		node, err := e.deref(part, where, nil)
		if err != nil {
			return err
		}
		nodes = append(nodes, node)
	}
	for i, a := range nodes {
		for _, b := range nodes[i+1:] {
			kind := func(s map[string]any) string {
				k := text(s["type"])
				if k == "integer" {
					k = "number"
				}
				return k
			}
			x, y := kind(a), kind(b)
			if flag(a["nullable"]) || flag(b["nullable"]) {
				return fail(where, "nullable oneOf branches require an exclusive native union contract")
			}
			if x != "" && y != "" && x != y {
				continue
			}
			if separateEnums(a, b) {
				continue
			}
			separate := false
			if x == "object" && y == "object" {
				for key, field := range object(a["properties"]) {
					required := func(s map[string]any) bool {
						for _, v := range list(s["required"]) {
							if text(v) == key {
								return true
							}
						}
						return false
					}
					other, exists := object(b["properties"])[key]
					if exists && required(a) && required(b) {
						left, err := e.deref(field, where, nil)
						if err != nil {
							return err
						}
						right, err := e.deref(other, where, nil)
						if err != nil {
							return err
						}
						if separateEnums(left, right) {
							separate = true
							break
						}
					}
				}
			}
			if !separate {
				return fail(where, "oneOf branches may overlap; an exclusive native union contract is required")
			}
		}
	}
	return nil
}
func separateEnums(a, b map[string]any) bool {
	if flag(a["nullable"]) || flag(b["nullable"]) {
		return false
	}
	values := func(s map[string]any) []any {
		if c, ok := s["const"]; ok {
			return []any{c}
		}
		return list(s["enum"])
	}
	av, bv := values(a), values(b)
	if len(av) == 0 || len(bv) == 0 {
		return false
	}
	// Only strings/bools are compared here: numeric spellings can denote equal values.
	for _, v := range append(append([]any{}, av...), bv...) {
		switch v.(type) {
		case string, bool:
		default:
			return false
		}
	}
	for _, v := range av {
		for _, w := range bv {
			vb, _ := json.Marshal(v)
			wb, _ := json.Marshal(w)
			if string(vb) == string(wb) {
				return false
			}
		}
	}
	return true
}
