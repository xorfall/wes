package contract

import (
	"encoding/json"
	"math/big"
	"strconv"
	"strings"
)

// Validate translated scalar constraints before calling the output complete. The runtime is a
// second independent validator; numeric bounds never pass through float64 here.
func constraints(schema map[string]any, typ, where string) error {
	pairs := [][3]string{{"minimum", "maximum", "numeric"}, {"minLength", "maxLength", "string"}, {"minItems", "maxItems", "array"}}
	for _, pair := range pairs {
		var bounds [2]*big.Rat
		for i, key := range pair[:2] {
			v, exists := schema[key]
			if !exists {
				continue
			}
			if pair[2] == "numeric" && !contains([]string{"integer", "number"}, typ) || pair[2] != "numeric" && typ != pair[2] {
				return fail(where, "constraint does not apply to its schema type")
			}
			number, ok := v.(json.Number)
			if !ok {
				return fail(where, "constraint bound must be numeric")
			}
			raw := string(number)
			if len(raw) > 128 {
				return fail(where, "numeric bound exceeds digit budget")
			}
			if index := strings.IndexAny(raw, "eE"); index >= 0 {
				exponent, err := strconv.Atoi(raw[index+1:])
				if err != nil || exponent > 4096 || exponent < -4096 {
					return fail(where, "numeric exponent exceeds budget")
				}
			}
			n, ok := new(big.Rat).SetString(raw)
			if !ok {
				return fail(where, "invalid numeric bound")
			}
			if pair[2] != "numeric" && (!n.IsInt() || n.Sign() < 0 || n.Cmp(big.NewRat(1000000, 1)) > 0) {
				return fail(where, "length/item bounds must be integers in 0..1000000")
			}
			bounds[i] = n
		}
		if bounds[0] != nil && bounds[1] != nil && bounds[0].Cmp(bounds[1]) > 0 {
			return fail(where, "minimum exceeds maximum")
		}
	}
	if raw, exists := schema["enum"]; exists {
		values, ok := raw.([]any)
		if !ok || len(values) == 0 || !contains([]string{"string", "integer", "number", "boolean"}, typ) {
			return fail(where, "enum needs a nonempty scalar sequence")
		}
		for _, value := range values {
			valid := false
			switch typ {
			case "string":
				_, valid = value.(string)
			case "boolean":
				_, valid = value.(bool)
			case "number":
				_, valid = value.(json.Number)
			case "integer":
				if n, ok := value.(json.Number); ok {
					_, err := strconv.ParseInt(string(n), 10, 64)
					valid = err == nil
				}
			}
			if !valid {
				return fail(where, "enum value does not match scalar type (nullable enum members require explicit review)")
			}
		}
	}
	return nil
}
