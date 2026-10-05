package contract

import (
	"strings"
	"unicode"
)

// Mirror the runtime's transport subset early, so extraction does not claim an unimportable
// operation is supported. The Rust importer still independently validates every descriptor.
func validateOperation(op Operation) error {
	bad := func(reason string) error { return fail(op.Evidence, reason) }
	if len(op.Route) > 64<<10 || strings.ContainsFunc(op.Route, unicode.IsControl) {
		return bad("invalid route size/control character")
	}
	for _, s := range strings.Split(op.Route, "/") {
		s = strings.ReplaceAll(strings.ToLower(s), "%2e", ".")
		if s == "." || s == ".." {
			return bad("dot segments are unsupported")
		}
	}
	remaining := op.Route
	destinations := map[string]bool{}
	auth := map[string]bool{}
	for _, part := range op.Auth {
		p := object(part)
		kind, wire := "header", strings.ToLower(text(p["header"]))
		if q := text(p["query"]); q != "" {
			kind, wire = "query", q
		}
		key := kind + ":" + wire
		if wire == "" || auth[key] {
			return bad("invalid/duplicate authentication destination")
		}
		if kind == "header" && (!headerToken(wire) || framing(wire)) {
			return bad("authentication cannot override framing headers")
		}
		auth[key] = true
	}
	for _, p := range op.Parameters {
		if len(p.Name) > 256 || len(p.Wire) > 256 || strings.ContainsFunc(p.Wire, unicode.IsControl) {
			return bad("invalid parameter name")
		}
		wire := p.Wire
		if p.Location == "header" {
			wire = strings.ToLower(wire)
		}
		key := p.Location + ":" + wire
		if destinations[key] || auth[key] {
			return bad("colliding parameter/auth destination")
		}
		destinations[key] = true
		if p.Location == "path" {
			marker := "{" + p.Wire + "}"
			found := false
			for _, s := range strings.Split(op.Route, "/") {
				if s == marker {
					found = true
				}
			}
			if !found {
				return bad("path parameters require complete route segments")
			}
			remaining = strings.ReplaceAll(remaining, marker, "parameter")
		}
		if p.Location == "header" && (!headerToken(wire) || framing(wire) || wire == "authorization" || wire == "cookie") {
			return bad("reserved or invalid ordinary header")
		}
	}
	if strings.ContainsAny(remaining, "{}") {
		return bad("unbound route placeholder")
	}
	return nil
}
func framing(s string) bool {
	return contains([]string{"host", "content-type", "content-length", "transfer-encoding", "connection", "upgrade", "trailer", "te", "proxy-authorization", "proxy-connection"}, s)
}
func headerToken(s string) bool {
	if s == "" {
		return false
	}
	for _, b := range []byte(s) {
		if !(b >= 'a' && b <= 'z' || b >= 'A' && b <= 'Z' || b >= '0' && b <= '9' || strings.ContainsRune("!#$%&'*+-.^_`|~", rune(b))) {
			return false
		}
	}
	return true
}
