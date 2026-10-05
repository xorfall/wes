package contract

import (
	"fmt"
	"strings"
)

// Provenance describes the definition, never the authority or origin of runtime values.
// Targets address emitted descriptor fields. Pointers address the supplied OpenAPI
// document; source locations are inert metadata.
type Provenance struct {
	Version int               `json:"version"`
	Status  string            `json:"status"`
	Entries []ProvenanceEntry `json:"entries"`
}
type SourceLines struct {
	Start int `json:"start"`
	End   int `json:"end"`
}
type ProvenanceEntry struct {
	Target  string        `json:"target"`
	Source  string        `json:"source"`
	Pointer string        `json:"pointer"`
	Lines   []SourceLines `json:"lines"`
	Basis   string        `json:"basis"`
	Reason  string        `json:"reason"`
}
type origin struct {
	target, pointer, reason string
	absent, derived         bool
}

func (e *extractor) origin(target, at, reason string) {
	_, exists := pointerValue(e.root, at)
	e.doc.origins = append(e.doc.origins, origin{target: target, pointer: at, reason: reason, absent: !exists})
}
func (e *extractor) derived(target, at, reason string) {
	e.origin(target, at, reason)
	e.doc.origins[len(e.doc.origins)-1].derived = true
}

// References are already validated by extraction. Follow their source identity too,
// rather than pointing at properties which only exist on the referenced definition.
func (e *extractor) sourcePointer(raw any, at string) string {
	for n := 0; n < 48; n++ {
		ref := text(object(raw)["$ref"])
		if ref == "" {
			break
		}
		value, ok := pointerValue(e.root, ref)
		if !ok {
			break
		}
		at, raw = ref, value
	}
	return at
}
func (e *extractor) typeOrigin(target string, raw any, at string) {
	at = e.sourcePointer(raw, at)
	schemaAt := at
	if _, ok := pointerValue(e.root, at+"/type"); ok {
		at += "/type"
	}
	e.origin(target, at, "Translated schema type; the runtime checks the resulting contract.")
	node, _ := pointerValue(e.root, schemaAt)
	s := object(node)
	if flag(s["nullable"]) {
		e.origin(target, schemaAt+"/nullable", "Nullable schema becomes an Option contract.")
	}
	if text(s["type"]) == "array" {
		e.typeOrigin(target, s["items"], schemaAt+"/items")
	}
	if text(s["type"]) == "object" && object(s["additionalProperties"]) != nil && len(object(s["properties"])) == 0 {
		e.typeOrigin(target, s["additionalProperties"], schemaAt+"/additionalProperties")
	}
}

func (d *Document) provenance() {
	p := &Provenance{Version: 1, Status: "current", Entries: []ProvenanceEntry{}}
	// Deterministic ordering and de-duplication for aliases reused by several operations.
	byTarget := map[string]origin{}
	for _, o := range d.origins {
		byTarget[o.target+"\x00"+o.pointer] = o
	}
	for _, key := range keysAny(byTarget) {
		o := byTarget[key]
		target := o.target
		entry := ProvenanceEntry{Target: target, Source: "sha256:" + d.Source.SHA256, Pointer: o.pointer,
			Lines: []SourceLines{}, Basis: "documented", Reason: o.reason}
		if o.absent || o.derived {
			entry.Basis = "inferred"
		}
		if strings.HasSuffix(target, "/auth") && o.absent {
			entry.Basis = "unknown"
			entry.Reason = "Authentication is not documented. No credentials are attached; this does not establish public access."
		}
		p.Entries = append(p.Entries, entry)
	}
	d.Source.Provenance = p
}

func (e *extractor) operationOrigins(op Operation, at, securityAt string) {
	target := fmt.Sprintf("#/operations/%d", len(e.doc.Operations))
	e.origin(target+"/method", at, "HTTP method from the source operation.")
	e.origin(target+"/route", at, "Route from the source operation.")
	e.derived(target+"/path", at, "Command name normalized from operationId or method and route.")
	authTarget := target + "/auth"
	if len(op.AuthOptions) > 0 {
		authTarget = target + "/authOptions"
	}
	e.origin(authTarget, securityAt, "Documented authentication mapping; credential values are supplied separately at invocation.")
	if len(op.Auth) == 0 && (len(op.AuthOptions) == 0 || (len(op.AuthOptions) == 1 && len(op.AuthOptions[0].Schemes) == 0)) {
		e.doc.origins[len(e.doc.origins)-1].reason = "No authentication requirements are declared by the source security array."
	}
}
