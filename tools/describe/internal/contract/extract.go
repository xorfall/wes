package contract

import (
	"crypto/sha256"
	"encoding/hex"
	"encoding/json"
	"errors"
	"fmt"
	"github.com/pb33f/libopenapi/index"
	"sort"
	"strconv"
	"strings"
	"unicode"
)

type extractor struct {
	root        map[string]any
	index       *index.SpecIndex
	doc         Document
	serial      int
	work        int
	refs        map[string]string
	aliasOwners map[string]string
}

func object(v any) map[string]any { m, _ := v.(map[string]any); return m }
func list(v any) []any            { a, _ := v.([]any); return a }
func text(v any) string           { s, _ := v.(string); return s }
func flag(v any) bool             { b, _ := v.(bool); return b }
func keys(m map[string]any) []string {
	k := make([]string, 0, len(m))
	for n := range m {
		k = append(k, n)
	}
	sort.Strings(k)
	return k
}
func pointer(s string) string { return strings.ReplaceAll(strings.ReplaceAll(s, "~", "~0"), "/", "~1") }
func ident(s string) string {
	var b strings.Builder
	for _, c := range s {
		if c < 128 && (unicode.IsLetter(c) || unicode.IsDigit(c) || c == '_') {
			b.WriteRune(c)
		} else {
			b.WriteByte('_')
		}
	}
	r := b.String()
	if r == "" {
		return "operation"
	}
	if r[0] >= '0' && r[0] <= '9' {
		return "op_" + r
	}
	return r
}
func fail(where, reason string) error { return fmt.Errorf("%s: %s", where, reason) }

// Extract fails closed by default. Partial output is explicitly requested and carries skipped
// operation counts plus pointer diagnostics; it is never advertised as complete API support.
func Extract(body []byte, provider string, allowPartial bool) (Document, error) {
	source, err := parseOpenAPI(body)
	if err != nil {
		return Document{}, err
	}
	root := source.root
	version := text(root["openapi"])
	if provider == "" || ident(provider) != provider {
		return Document{}, errors.New("provider must be an ASCII identifier")
	}
	sum := sha256.Sum256(body)
	e := extractor{root: root, index: source.model.Index, doc: Document{Version: 1, Provider: provider, Types: map[string]any{}, Operations: []Operation{}, Source: Evidence{SHA256: hex.EncodeToString(sum[:]), Format: version}, Servers: []any{}, Diagnostics: []string{}}}
	e.servers(root, "#")
	for _, key := range []string{"webhooks"} {
		if len(object(root[key])) > 0 {
			return e.doc, fail("#/"+key, "not supported; no callbacks or out-of-band operations are generated")
		}
	}
	paths := object(root["paths"])
	if paths == nil {
		return e.doc, errors.New("paths must be an object")
	}
	seen := map[string]bool{}
	routes := []string{}
	for route := range source.model.Model.Paths.PathItems.FromOldest() {
		routes = append(routes, route)
	}
	sort.Strings(routes)
	for _, route := range routes {
		path, err := e.deref(paths[route], "#/paths/"+pointer(route), nil)
		if err != nil {
			return e.doc, err
		}
		e.servers(path, "#/paths/"+pointer(route))
		for _, method := range []string{"get", "head", "post", "put", "patch", "delete", "options", "trace"} {
			raw, exists := path[method]
			if !exists {
				continue
			}
			e.doc.Source.Discovered++
			where := e.sourcePointer(paths[route], "#/paths/"+pointer(route)) + "/" + method
			// Failed operations must not leave unreachable/invalid type definitions in partial output.
			before := map[string]any{}
			beforeOrigins := len(e.doc.origins)
			e.refs = map[string]string{}
			for k, v := range e.doc.Types {
				before[k] = v
			}
			op, err := e.operation(route, method, object(raw), path, where)
			if err == nil {
				err = validateOperation(op)
			}
			if err == nil && seen[op.Path[0]] {
				err = fail(where, "duplicate normalized operationId")
			}
			if err != nil {
				e.doc.Types = before
				e.doc.origins = e.doc.origins[:beforeOrigins]
				e.doc.Source.Skipped++
				e.doc.Diagnostics = append(e.doc.Diagnostics, err.Error())
				continue
			}
			seen[op.Path[0]] = true
			e.doc.Operations = append(e.doc.Operations, op)
		}
	}
	e.doc.Source.Emitted = len(e.doc.Operations)
	e.doc.provenance()
	if e.doc.Source.Skipped > 0 && !allowPartial {
		return e.doc, errors.New("extraction incomplete; inspect diagnostics (partial output requires -allow-partial)")
	}
	if len(e.doc.Operations) == 0 || len(e.doc.Operations) > 1000 {
		return e.doc, errors.New("expected 1 to 1000 supported operations")
	}
	_, err = e.doc.Marshal()
	return e.doc, err
}
func (e *extractor) servers(node map[string]any, where string) {
	for _, server := range list(node["servers"]) {
		e.doc.Servers = append(e.doc.Servers, map[string]any{"at": where, "declaration": server})
	}
}
func (e *extractor) deref(v any, where string, stack []string) (map[string]any, error) {
	e.work++
	if e.work > 100000 || len(stack) > 48 {
		return nil, fail(where, "reference expansion budget exceeded")
	}
	m := object(v)
	if m == nil {
		return nil, fail(where, "expected object")
	}
	ref, exists := m["$ref"]
	if !exists {
		return m, nil
	}
	r := text(ref)
	if !strings.HasPrefix(r, "#/") {
		return nil, fail(where, "only local JSON-pointer references are supported; external refs are never fetched")
	}
	for _, old := range stack {
		if old == r {
			return nil, fail(where, "recursive reference requires a future recursive contract type")
		}
	}
	for k := range m {
		if k != "$ref" && k != "summary" && k != "description" {
			return nil, fail(where, "semantic $ref siblings are unsupported")
		}
	}
	reference, _ := e.index.SearchIndexForReference(r)
	if reference == nil || reference.Node == nil {
		return nil, fail(where, "unresolved OpenAPI reference")
	}
	target, err := sourceValueBudget(reference.Node, &e.work)
	if err != nil {
		return nil, err
	}
	resolved, err := e.deref(target, where, append(stack, r))
	if err != nil {
		return nil, err
	}
	copy := map[string]any{}
	for k, v := range resolved {
		copy[k] = v
	}
	for _, k := range []string{"summary", "description"} {
		if v, ok := m[k]; ok {
			copy[k] = v
		}
	}
	return copy, nil
}
func (e *extractor) operation(route, method string, op, path map[string]any, where string) (Operation, error) {
	result := Operation{Path: []string{ident(text(op["operationId"]))}, Summary: text(op["summary"]), Description: text(op["description"]), ResponseDescriptions: map[string]string{}, Method: strings.ToUpper(method), Route: route, Auth: []any{}, Parameters: []Parameter{}, Responses: map[string]*string{}, Evidence: where}
	if op == nil || method == "trace" {
		return result, fail(where, "unsupported or invalid operation")
	}
	if !strings.HasPrefix(route, "/") || strings.ContainsAny(route, "?#\\") {
		return result, fail(where, "invalid route")
	}
	if text(op["operationId"]) == "" {
		result.Path[0] = ident(method + "_" + route)
	}
	if len(object(op["callbacks"])) > 0 {
		return result, fail(where, "callbacks are unsupported")
	}
	e.servers(op, where)
	security, exists := op["security"]
	securityAt := where + "/security"
	if !exists {
		security, exists = e.root["security"]
		if exists {
			securityAt = "#/security"
		}
	}
	if exists && security == nil {
		return result, fail(where, "security cannot be null")
	}
	auth, options, err := e.security(security, where)
	if err != nil {
		return result, err
	}
	result.Auth = auth
	result.AuthOptions = options
	// Explicit anonymous access is one documented choice; absence remains unknown.
	if exists && len(list(security)) == 0 {
		result.AuthOptions = []AuthOption{{Schemes: []string{}, Auth: []any{}}}
	}
	e.operationOrigins(result, where, securityAt)
	target := fmt.Sprintf("#/operations/%d", len(e.doc.Operations))
	if !exists {
		e.doc.Diagnostics = append(e.doc.Diagnostics, where+": authentication unknown; no credentials attached (not a declaration of public access)")
	}
	params := map[string]map[string]any{}
	paramOrigins := map[string]string{}
	for i, owner := range []map[string]any{path, op} {
		ownerAt := where
		if i == 0 {
			ownerAt = strings.TrimSuffix(where, "/"+method)
		}
		if v, ok := owner["parameters"]; ok {
			if _, ok := v.([]any); !ok {
				return result, fail(where, "parameters must be an array")
			}
		}
		local := map[string]bool{}
		for j, raw := range list(owner["parameters"]) {
			at := fmt.Sprintf("%s/parameters/%d", ownerAt, j)
			p, err := e.deref(raw, at, nil)
			if err != nil {
				return result, err
			}
			key := text(p["in"]) + ":" + text(p["name"])
			if local[key] {
				return result, fail(where, "duplicate parameter identity")
			}
			local[key] = true
			params[key] = p
			paramOrigins[key] = e.sourcePointer(raw, at)
		}
	}
	names := map[string]bool{}
	for _, key := range keysAny(params) {
		p := params[key]
		for _, k := range []string{"required", "explode", "allowReserved", "allowEmptyValue"} {
			if v, ok := p[k]; ok {
				if _, ok := v.(bool); !ok {
					return result, fail(where, "parameter flags must be boolean")
				}
			}
		}
		location, wire := text(p["in"]), text(p["name"])
		at := paramOrigins[key]
		if wire == "" || !contains([]string{"path", "query", "header"}, location) {
			return result, fail(at, "only path, query and header parameters are supported")
		}
		if flag(p["allowReserved"]) || flag(p["allowEmptyValue"]) || p["content"] != nil {
			return result, fail(at, "reserved/empty/content parameter encoding is unsupported")
		}
		schema, err := e.deref(p["schema"], at+"/schema", nil)
		if err != nil {
			return result, err
		}
		kind, err := e.schema(p["schema"], at+"/schema", nil)
		if err != nil {
			return result, err
		}
		encoding := "scalar"
		style := text(p["style"])
		explode := location == "query"
		if v, ok := p["explode"]; ok {
			explode = flag(v)
		}
		switch text(schema["type"]) {
		case "array":
			if location != "query" || (style != "" && style != "form") || !explode || !scalarSchema(object(schema["items"])) {
				return result, fail(at, "only query form/explode arrays of scalars are supported")
			}
			encoding = "repeat"
		case "object":
			if location != "query" || style != "deepObject" || !explode {
				return result, fail(at, "object parameter requires deepObject/explode")
			}
			if extra := object(schema["additionalProperties"]); extra != nil {
				extra, err = e.deref(extra, at, nil)
				if err != nil || !scalarSchema(extra) {
					return result, fail(at, "deepObject values must be non-null scalars")
				}
			}
			for _, v := range object(schema["properties"]) {
				v, err := e.deref(v, at, nil)
				if err != nil || !scalarSchema(v) {
					return result, fail(at, "deepObject fields must be non-null scalars")
				}
			}
			encoding = "deepObject"
		default:
			if !scalarSchema(schema) || (style != "" && style != map[string]string{"query": "form", "path": "simple", "header": "simple"}[location]) {
				return result, fail(at, "unsupported scalar parameter style or nullable value")
			}
		}
		if location == "path" && !flag(p["required"]) {
			return result, fail(at, "path parameters must be required")
		}
		name := ident(wire)
		if names[name] {
			name = ident(location + "_" + wire)
		}
		if names[name] {
			return result, fail(at, "colliding argument names")
		}
		names[name] = true
		paramTarget := fmt.Sprintf("%s/parameters/%d", target, len(result.Parameters))
		e.typeOrigin(paramTarget+"/type", p["schema"], at+"/schema")
		e.origin(paramTarget+"/required", at+"/required", "Parameter requiredness; absent flags use OpenAPI optional semantics.")
		e.origin(paramTarget+"/wire", at+"/name", "Parameter wire name.")
		e.origin(paramTarget+"/location", at+"/in", "Parameter transport location.")
		e.derived(paramTarget+"/name", at+"/name", "Argument identifier normalized from the wire name.")
		e.derived(paramTarget+"/encoding", at, "Encoding selected from the parameter location, style, explode and schema.")
		result.Parameters = append(result.Parameters, Parameter{Name: name, Description: text(p["description"]), Wire: wire, Location: location, Type: kind, Required: flag(p["required"]), Encoding: encoding})
	}
	if raw, exists := op["requestBody"]; exists {
		if method == "get" || method == "head" {
			return result, fail(where, "GET/HEAD body is unsupported")
		}
		bodyAt := e.sourcePointer(raw, where+"/requestBody")
		b, err := e.deref(raw, bodyAt, nil)
		if err != nil {
			return result, err
		}
		kind, err := e.content(b, bodyAt)
		if err != nil {
			return result, err
		}
		if kind == nil {
			return result, fail(where, "request body needs JSON content")
		}
		name := "body"
		if names[name] {
			name = "request_body"
		}
		if names[name] {
			return result, fail(where, "body argument name collision")
		}
		bodyTarget := fmt.Sprintf("%s/parameters/%d", target, len(result.Parameters))
		e.origin(bodyTarget+"/required", bodyAt+"/required", "Request body requiredness; absent flags use OpenAPI optional semantics.")
		e.typeOrigin(bodyTarget+"/type", object(object(b["content"])["application/json"])["schema"], bodyAt+"/content/application~1json/schema")
		result.Parameters = append(result.Parameters, Parameter{Name: name, Description: text(b["description"]), Wire: "body", Location: "body", Type: *kind, Required: flag(b["required"]), Encoding: "json"})
	}
	responses := object(op["responses"])
	if responses == nil {
		return result, fail(where, "responses must be an object")
	}
	for _, code := range keys(responses) {
		responseAt := where + "/responses/" + pointer(code)
		response, err := e.deref(responses[code], responseAt, nil)
		if err != nil {
			return result, err
		}
		if description := text(response["description"]); description != "" {
			result.ResponseDescriptions[code] = description
		}
		if code == "default" || strings.Contains(code, "X") {
			return result, fail(responseAt, "default/range response contracts require explicit status codes in the native HTTP model")
		}
		n, err := strconv.Atoi(code)
		if err != nil || n < 100 || n > 599 || strconv.Itoa(n) != code {
			return result, fail(responseAt, "invalid HTTP response status")
		}
		responseAt = e.sourcePointer(responses[code], responseAt)
		kind, err := e.content(response, responseAt)
		if err != nil {
			return result, err
		}
		if (n == 204 || n == 205 || n == 304 || method == "head") && kind != nil {
			return result, fail(responseAt, "bodyless response cannot declare content")
		}
		result.Responses[code] = kind
		e.origin(target+"/responses/"+code, responseAt, "Response contract for this exact HTTP status.")
		if kind != nil {
			e.typeOrigin(target+"/responses/"+code, object(object(response["content"])["application/json"])["schema"], responseAt+"/content/application~1json/schema")
		}
	}
	if len(result.Responses) == 0 {
		return result, fail(where, "no declared response contracts")
	}
	return result, nil
}
func keysAny[V any](m map[string]V) []string {
	k := []string{}
	for n := range m {
		k = append(k, n)
	}
	sort.Strings(k)
	return k
}
func contains(a []string, s string) bool {
	for _, v := range a {
		if v == s {
			return true
		}
	}
	return false
}
func scalarSchema(s map[string]any) bool {
	return !flag(s["nullable"]) && contains([]string{"string", "integer", "number", "boolean"}, text(s["type"]))
}
func (e *extractor) content(node map[string]any, where string) (*string, error) {
	raw, exists := node["content"]
	if !exists {
		return nil, nil
	}
	content := object(raw)
	if content == nil || len(content) != 1 {
		return nil, fail(where, "exactly one JSON media type is supported")
	}
	media := object(content["application/json"])
	if media == nil {
		return nil, fail(where, "only application/json is supported (no implicit multipart, binary or streaming)")
	}
	if _, exists := media["encoding"]; exists {
		return nil, fail(where, "media-specific encoding is unsupported")
	}
	kind, err := e.schema(media["schema"], where+"/content/application~1json/schema", nil)
	return &kind, err
}
func (e *extractor) security(raw any, where string) ([]any, []AuthOption, error) {
	if raw == nil {
		return []any{}, nil, nil
	}
	alternatives, ok := raw.([]any)
	if !ok || len(alternatives) > 32 {
		return nil, nil, fail(where, "security must be an array of at most 32 alternatives")
	}
	if len(alternatives) == 0 {
		return []any{}, nil, nil
	}
	options := []AuthOption{}
	seen := map[string]bool{}
	for _, rawRequirement := range alternatives {
		requirement := object(rawRequirement)
		if requirement == nil {
			return nil, nil, fail(where, "invalid security requirement")
		}
		names := keys(requirement)
		identity, _ := json.Marshal(names)
		if seen[string(identity)] {
			return nil, nil, fail(where, "duplicate security alternative")
		}
		seen[string(identity)] = true
		auth, err := e.securityRequirement(requirement, where)
		if err != nil {
			return nil, nil, err
		}
		if names == nil {
			names = []string{}
		}
		options = append(options, AuthOption{names, auth})
	}
	if len(options) == 1 {
		return options[0].Auth, nil, nil
	}
	return []any{}, options, nil
}
func (e *extractor) securityRequirement(requirement map[string]any, where string) ([]any, error) {
	result := []any{}
	if len(requirement) > 32 {
		return nil, fail(where, "too many authentication requirements")
	}
	for _, name := range keys(requirement) {
		if _, ok := requirement[name].([]any); !ok {
			return nil, fail(where, "security scopes must be an array")
		}
		scheme, err := e.deref(object(object(e.root["components"])["securitySchemes"])[name], where+"/security", nil)
		if err != nil {
			return nil, err
		}
		if ident(name) != name {
			return nil, fail(where, "security scheme name needs a reviewed credential alias")
		}
		if len(list(requirement[name])) > 0 {
			return nil, fail(where, "OAuth scopes/token acquisition are not implemented")
		}
		switch text(scheme["type"]) {
		case "apiKey":
			location := text(scheme["in"])
			if !contains([]string{"header", "query"}, location) {
				return nil, fail(where, "cookie authentication unsupported")
			}
			result = append(result, map[string]any{location: text(scheme["name"]), "secret": name, "scheme": ""})
			if location == "query" {
				delete(result[len(result)-1].(map[string]any), "scheme")
			}
		case "http":
			if strings.ToLower(text(scheme["scheme"])) != "bearer" {
				if strings.EqualFold(text(scheme["scheme"]), "basic") {
					result = append(result, map[string]any{"userSecret": name + ".username", "secret": name + ".password"})
					continue
				}
				return nil, fail(where, "unsupported HTTP authentication scheme")
			}
			result = append(result, map[string]any{"header": "Authorization", "scheme": "Bearer", "secret": name})
		default:
			return nil, fail(where, "unsupported security scheme; configure token handling explicitly")
		}
	}
	return result, nil
}

func (e *extractor) schema(raw any, where string, stack []string) (string, error) {
	if len(stack) > 48 {
		return "", fail(where, "schema expansion budget exceeded")
	}
	original := object(raw)
	if original == nil {
		return "", fail(where, "boolean/missing schemas need an explicit contract")
	}
	if ref := text(original["$ref"]); ref != "" {
		for _, r := range stack {
			if r == ref {
				return "", fail(where, "recursive schema is unsupported")
			}
		}
		if cached := e.refs[ref]; cached != "" {
			return cached, nil
		}
		stack = append(stack, ref)
	} else {
		stack = append(stack, where)
	}
	s, err := e.deref(raw, where, nil)
	if err != nil {
		return "", err
	}
	if ref := text(original["$ref"]); ref != "" {
		// A use-site description belongs to its parameter/field, not to the shared type alias.
		s, err = e.deref(map[string]any{"$ref": ref}, where, nil)
		if err != nil {
			return "", err
		}
	}
	where = e.sourcePointer(raw, where)
	propertyOrigins := map[string]string{}
	requiredOrigins := map[string]string{}
	for _, keyword := range []string{"oneOf", "anyOf"} {
		if branches, exists := s[keyword]; exists {
			for key := range s {
				if !contains([]string{keyword, "title", "description", "example", "examples", "deprecated"}, key) {
					return "", fail(where, "union siblings require an intersection contract")
				}
			}
			parts := list(branches)
			if len(parts) == 0 || len(parts) > 32 {
				return "", fail(where, "union must have 1..32 alternatives")
			}
			if keyword == "oneOf" {
				if err := e.disjoint(parts, where); err != nil {
					return "", err
				}
			}
			alternatives := []string{}
			nullable := false
			for i, part := range parts {
				node, err := e.deref(part, where, nil)
				if err != nil {
					return "", err
				}
				if text(node["type"]) == "null" && len(node) == 1 {
					nullable = true
					continue
				}
				kind, err := e.schema(part, fmt.Sprintf("%s/%s/%d", where, keyword, i), stack)
				if err != nil {
					return "", err
				}
				if !contains(alternatives, kind) {
					alternatives = append(alternatives, kind)
				}
			}
			if len(alternatives) == 0 {
				return "", fail(where, "null-only schemas have no native value contract")
			}
			kind := alternatives[len(alternatives)-1]
			for i := len(alternatives) - 2; i >= 0; i-- {
				kind = "Union<" + alternatives[i] + "," + kind + ">"
			}
			if nullable {
				kind = "Option<" + kind + ">"
			}
			return kind, nil
		}
	}
	if branches, exists := s["allOf"]; exists {
		if len(s) != 1 || len(list(branches)) == 0 {
			return "", fail(where, "allOf siblings/empty intersection unsupported")
		}
		merged := map[string]any{"type": "object"}
		props := map[string]any{}
		required := map[string]bool{}
		for i, part := range list(branches) {
			partAt := e.sourcePointer(part, fmt.Sprintf("%s/allOf/%d", where, i))
			node, err := e.deref(part, partAt, nil)
			if err != nil {
				return "", err
			}
			if text(node["type"]) != "object" {
				return "", fail(where, "only flat open-record allOf is supported")
			}
			for k := range node {
				if !contains([]string{"type", "properties", "required", "description", "title"}, k) {
					return "", fail(where, "allOf has unrepresentable record constraints")
				}
			}
			for k, v := range object(node["properties"]) {
				if _, exists := props[k]; exists {
					return "", fail(where, "overlapping allOf fields require an explicit intersection contract")
				}
				props[k] = v
				propertyOrigins[k] = partAt + "/properties/" + pointer(k)
				requiredOrigins[k] = partAt + "/required"
			}
			for _, r := range list(node["required"]) {
				required[text(r)] = true
			}
		}
		merged["properties"] = props
		req := []any{}
		for _, k := range keysAny(required) {
			req = append(req, k)
		}
		merged["required"] = req
		s = merged
	}
	if value, exists := s["const"]; exists {
		if _, exists := s["enum"]; exists {
			return "", fail(where, "const with enum needs an intersection contract")
		}
		copy := map[string]any{}
		for key, v := range s {
			if key != "const" {
				copy[key] = v
			}
		}
		copy["enum"] = []any{value}
		s = copy
		if text(s["type"]) == "" {
			switch v := value.(type) {
			case string:
				s["type"] = "string"
			case bool:
				s["type"] = "boolean"
			case json.Number:
				if _, err := strconv.ParseInt(string(v), 10, 64); err == nil {
					s["type"] = "integer"
				} else {
					s["type"] = "number"
				}
			default:
				return "", fail(where, "only scalar const contracts are supported")
			}
		}
	}
	allowed := []string{"type", "properties", "required", "additionalProperties", "items", "nullable", "enum", "minimum", "maximum", "minLength", "maxLength", "minItems", "maxItems", "title", "description", "example", "examples", "default", "deprecated", "format", "$schema", "$id", "$comment", "readOnly", "writeOnly"}
	for _, k := range keys(s) {
		if strings.HasPrefix(k, "x-") {
			e.doc.Diagnostics = append(e.doc.Diagnostics, where+"/"+pointer(k)+": extension retained only in source document")
			continue
		}
		if !contains(allowed, k) {
			return "", fail(where+"/"+pointer(k), "schema keyword is not yet representable; no silent widening")
		}
	}
	if flag(s["readOnly"]) || flag(s["writeOnly"]) {
		return "", fail(where, "readOnly/writeOnly require direction-specific contracts")
	}
	for _, k := range []string{"nullable", "readOnly", "writeOnly"} {
		if v, ok := s[k]; ok {
			if _, ok := v.(bool); !ok {
				return "", fail(where, "schema flags must be boolean")
			}
		}
	}
	nullable := flag(s["nullable"])
	typ := text(s["type"])
	if variants, ok := s["type"].([]any); ok {
		if len(variants) == 0 || len(variants) > 7 {
			return "", fail(where, "invalid schema type alternatives")
		}
		alternatives := []string{}
		seen := map[string]bool{}
		for _, v := range variants {
			t := text(v)
			if t == "" || seen[t] {
				return "", fail(where, "invalid or duplicate schema type")
			}
			seen[t] = true
			if t == "null" {
				nullable = true
				continue
			}
			branch := map[string]any{}
			for key, value := range s {
				if key != "nullable" {
					branch[key] = value
				}
			}
			branch["type"] = t
			kind, err := e.schema(branch, where, stack)
			if err != nil {
				return "", err
			}
			alternatives = append(alternatives, kind)
		}
		if len(alternatives) == 0 {
			return "", fail(where, "null-only schemas have no native value contract")
		}
		kind := alternatives[len(alternatives)-1]
		for i := len(alternatives) - 2; i >= 0; i-- {
			kind = "Union<" + alternatives[i] + "," + kind + ">"
		}
		if nullable {
			kind = "Option<" + kind + ">"
		}
		return kind, nil
	}
	definition := map[string]any{}
	if err := constraints(s, typ, where); err != nil {
		return "", err
	}
	base := ""
	if typ == "" {
		metadata := true
		for key := range s {
			if !contains([]string{"title", "description", "example", "examples", "default", "deprecated", "$comment", "$schema"}, key) {
				metadata = false
			}
		}
		if metadata {
			return "Unknown", nil
		}
	}
	switch typ {
	case "string":
		base = "Text"
	case "integer":
		base = "Int"
	case "number":
		base = "Decimal"
	case "boolean":
		base = "Bool"
	case "array":
		item, err := e.schema(s["items"], where+"/items", stack)
		if err != nil {
			return "", err
		}
		base = "List<" + item + ">"
	case "object":
		props := object(s["properties"])
		if raw, ok := s["properties"]; ok && object(raw) == nil {
			return "", fail(where, "properties must be an object")
		}
		if raw, ok := s["required"]; ok {
			if _, ok := raw.([]any); !ok {
				return "", fail(where, "required must be an array")
			}
		}
		additional, explicit := s["additionalProperties"]
		if explicit && additional != true && additional != false && object(additional) == nil {
			return "", fail(where, "additionalProperties must be boolean or schema")
		}
		if explicit && additional == false {
			return "", fail(where, "closed records are not supported by the existing open Record contract")
		}
		if extra := object(additional); extra != nil {
			if len(props) > 0 {
				return "", fail(where, "mixed fixed fields and typed additionalProperties need a future record extension")
			}
			item, err := e.schema(extra, where+"/additionalProperties", stack)
			if err != nil {
				return "", err
			}
			base = "Map<Text," + item + ">"
		} else {
			base = "Record"
			fields := map[string]any{}
			required := map[string]bool{}
			for _, r := range list(s["required"]) {
				name := text(r)
				if name == "" || required[name] {
					return "", fail(where, "invalid required fields")
				}
				required[name] = true
				if _, ok := props[name]; !ok {
					return "", fail(where, "required field without a property schema")
				}
			}
			for _, name := range keys(props) {
				at := propertyOrigins[name]
				if at == "" {
					at = where + "/properties/" + pointer(name)
					propertyOrigins[name] = at
					requiredOrigins[name] = where + "/required"
				}
				kind, err := e.schema(props[name], at, stack)
				if err != nil {
					return "", err
				}
				field := map[string]any{"type": kind, "optional": !required[name]}
				fieldSchema, err := e.deref(props[name], at, nil)
				if err != nil {
					return "", err
				}
				if description := text(fieldSchema["description"]); description != "" {
					field["description"] = description
				}
				fields[name] = field
			}
			definition["fields"] = fields
		}
	default:
		return "", fail(where, "missing or unsupported schema type")
	}
	definition["base"] = base
	if _, ok := s["default"]; ok {
		e.doc.Diagnostics = append(e.doc.Diagnostics, where+"/default: documentation default is not supplied automatically")
	}
	for from, to := range map[string]string{"enum": "enum", "minimum": "min", "maximum": "max", "minLength": "minLength", "maxLength": "maxLength", "minItems": "minItems", "maxItems": "maxItems"} {
		if v, ok := s[from]; ok {
			definition[to] = v
		}
	}
	if format := text(s["format"]); format != "" {
		e.doc.Diagnostics = append(e.doc.Diagnostics, where+"/format: "+format+" is an annotation, not an enforced format constraint")
	}
	if len(definition) == 1 && text(original["$ref"]) == "" {
		if nullable {
			return "Option<" + base + ">", nil
		}
		return base, nil
	}
	e.serial++
	if e.serial > 1000 {
		return "", fail(where, "generated type limit exceeded")
	}
	name := fmt.Sprintf("ApiType%d", e.serial)
	if ref := text(original["$ref"]); ref != "" {
		sum := sha256.Sum256([]byte(ref))
		parts := strings.Split(ref, "/")
		name = "Api_" + ident(parts[len(parts)-1]) + "_" + hex.EncodeToString(sum[:4])
		if e.aliasOwners == nil {
			e.aliasOwners = map[string]string{}
		}
		if previous, exists := e.aliasOwners[name]; exists && previous != ref {
			return "", fail(where, "schema alias collision; rename the source schema")
		}
		e.aliasOwners[name] = ref
	}
	if description := text(s["description"]); description != "" {
		definition["description"] = description
	}
	e.doc.Types[name] = definition
	typeTarget := "#/types/" + pointer(name)
	e.typeOrigin(typeTarget+"/base", raw, where)
	for field := range object(definition["fields"]) {
		fieldTarget := typeTarget + "/fields/" + pointer(field)
		e.typeOrigin(fieldTarget+"/type", object(s["properties"])[field], propertyOrigins[field])
		e.origin(fieldTarget+"/optional", requiredOrigins[field], "Field presence follows the required list. Without a documented presence guarantee the field is accepted as optional, not asserted always present.")
	}
	for from, to := range map[string]string{"enum": "enum", "minimum": "min", "maximum": "max", "minLength": "minLength", "maxLength": "maxLength", "minItems": "minItems", "maxItems": "maxItems"} {
		if _, exists := definition[to]; exists {
			sourceKey := from
			if from == "enum" {
				if _, exists := pointerValue(e.root, where+"/const"); exists {
					sourceKey = "const"
				}
			}
			e.origin(typeTarget+"/"+to, where+"/"+sourceKey, "Explicit schema constraint translated without weakening.")
		}
	}
	if nullable {
		name = "Option<" + name + ">"
	}
	if ref := text(original["$ref"]); ref != "" {
		if e.refs == nil {
			e.refs = map[string]string{}
		}
		e.refs[ref] = name
	}
	return name, nil
}
