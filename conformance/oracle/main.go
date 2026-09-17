// Command abadaoracle runs grpc-gateway's own path parser and ServeMux over the
// cases in conformance/cases and writes what they answer. abada's tests replay
// those answers, so "behaves like grpc-gateway" is a measurement, not a claim.
//
// It is copied into a grpc-gateway checkout (internal/abadaoracle) by
// scripts/regen-vectors.sh, because it needs the internal httprule package.
package main

import (
	"encoding/json"
	"fmt"
	"math/rand"
	"net/http"
	"net/http/httptest"
	"net/url"
	"os"
	"strings"

	"github.com/grpc-ecosystem/grpc-gateway/v2/internal/httprule"
	"github.com/grpc-ecosystem/grpc-gateway/v2/runtime"
)

type handlerSpec struct {
	Method   string `json:"method"`
	Template string `json:"template"`
}

type routeCase struct {
	Name     string        `json:"name"`
	Mode     string        `json:"mode"`
	Handlers []handlerSpec `json:"handlers"`
	Method   string        `json:"method"`
	Target   string        `json:"target"`
}

type cases struct {
	Templates []string    `json:"templates"`
	Routes    []routeCase `json:"routes"`
	// RandomTemplates asks for that many extra templates drawn from a fixed
	// seed, to reach combinations nobody thought to write by hand.
	RandomTemplates int `json:"random_templates"`
	RandomRoutes    int `json:"random_routes"`
}

type templateVector struct {
	Template string   `json:"template"`
	Parses   bool     `json:"parses"`
	Display  string   `json:"display,omitempty"`
	OpCodes  []int    `json:"op_codes,omitempty"`
	Pool     []string `json:"pool,omitempty"`
	Verb     string   `json:"verb,omitempty"`
	Fields   []string `json:"fields,omitempty"`
	// Valid is whether runtime.NewPattern accepts the compiled template.
	Valid bool `json:"valid"`
}

type routeVector struct {
	routeCase
	// Outcome is "matched", "not_found", "method_not_allowed", "bad_request"
	// or "invalid_target".
	Outcome string            `json:"outcome"`
	Handler int               `json:"handler,omitempty"`
	Params  map[string]string `json:"params,omitempty"`
}

type vectors struct {
	Generator      string           `json:"generator"`
	PathUnescaped  string           `json:"path_unescaped_bytes"`
	Templates      []templateVector `json:"templates"`
	Routes         []routeVector    `json:"routes"`
	Nondeterminism []string         `json:"dropped_nondeterministic,omitempty"`
}

func main() {
	if len(os.Args) == 3 && os.Args[1] == "bench" {
		runBench(os.Args[2])
		return
	}
	if len(os.Args) == 3 && os.Args[1] == "inventory" {
		runInventory(os.Args[2])
		return
	}
	if len(os.Args) == 2 && os.Args[1] == "errors" {
		runErrors()
		return
	}
	if len(os.Args) == 4 && os.Args[1] == "contract" {
		runContract(os.Args[2], os.Args[3])
		return
	}
	var c cases
	if err := json.NewDecoder(os.Stdin).Decode(&c); err != nil {
		fail("decode cases: %v", err)
	}
	rng := rand.New(rand.NewSource(1))
	for i := 0; i < c.RandomTemplates; i++ {
		c.Templates = append(c.Templates, randomTemplate(rng))
	}
	for i := 0; i < c.RandomRoutes; i++ {
		c.Routes = append(c.Routes, randomRoute(rng, i))
	}

	out := vectors{Generator: os.Getenv("ABADA_GENERATOR"), PathUnescaped: pathUnescapedBytes()}
	for _, t := range c.Templates {
		out.Templates = append(out.Templates, templateOf(t))
	}
	for _, rc := range c.Routes {
		first := route(rc)
		deterministic := true
		// The mux walks a Go map for the 405 fallback; a case whose answer
		// depends on map order cannot be a fixed expectation.
		for i := 0; i < 32; i++ {
			if again := route(rc); !sameRoute(first, again) {
				deterministic = false
				break
			}
		}
		if !deterministic {
			out.Nondeterminism = append(out.Nondeterminism, rc.Name)
			continue
		}
		out.Routes = append(out.Routes, first)
	}
	enc := json.NewEncoder(os.Stdout)
	enc.SetIndent("", "  ")
	enc.SetEscapeHTML(false)
	if err := enc.Encode(out); err != nil {
		fail("encode: %v", err)
	}
}

func fail(f string, a ...any) {
	fmt.Fprintf(os.Stderr, "abadaoracle: "+f+"\n", a...)
	os.Exit(1)
}

// pathUnescapedBytes lists, as hex, every byte net/url leaves unescaped in a
// path — the rule that decides whether a request gets a RawPath.
func pathUnescapedBytes() string {
	var b strings.Builder
	for c := 0; c < 256; c++ {
		// Prefixed with "/": a path of exactly "*" is special-cased by
		// EscapedPath and would lie about how '*' is escaped.
		p := "/" + string([]byte{byte(c)})
		if u := (url.URL{Path: p}); u.EscapedPath() == p {
			fmt.Fprintf(&b, "%02x", c)
		}
	}
	return b.String()
}

func templateOf(t string) templateVector {
	v := templateVector{Template: t}
	comp, err := httprule.Parse(t)
	if err != nil {
		return v
	}
	v.Parses = true
	v.Display = fmt.Sprint(comp)
	tmpl := comp.Compile()
	v.OpCodes, v.Pool, v.Verb, v.Fields = tmpl.OpCodes, tmpl.Pool, tmpl.Verb, tmpl.Fields
	_, perr := runtime.NewPattern(tmpl.Version, tmpl.OpCodes, tmpl.Pool, tmpl.Verb)
	v.Valid = perr == nil
	return v
}

func mode(name string) runtime.UnescapingMode {
	switch name {
	case "", "legacy":
		return runtime.UnescapingModeLegacy
	case "all_except_reserved":
		return runtime.UnescapingModeAllExceptReserved
	case "all_except_slash":
		return runtime.UnescapingModeAllExceptSlash
	case "all_characters":
		return runtime.UnescapingModeAllCharacters
	}
	fail("unknown mode %q", name)
	return 0
}

func route(rc routeCase) routeVector {
	v := routeVector{routeCase: rc}
	u, err := url.ParseRequestURI(rc.Target)
	if err != nil || !strings.HasPrefix(rc.Target, "/") {
		v.Outcome = "invalid_target"
		return v
	}
	mux := runtime.NewServeMux(runtime.WithUnescapingMode(mode(rc.Mode)))
	for i, h := range rc.Handlers {
		comp, err := httprule.Parse(h.Template)
		if err != nil {
			fail("case %s: handler %d does not parse: %v", rc.Name, i, err)
		}
		t := comp.Compile()
		pat, err := runtime.NewPattern(t.Version, t.OpCodes, t.Pool, t.Verb)
		if err != nil {
			fail("case %s: handler %d is not a valid pattern: %v", rc.Name, i, err)
		}
		idx := i
		mux.Handle(h.Method, pat, func(w http.ResponseWriter, _ *http.Request, p map[string]string) {
			w.Header().Set("X-Abada-Handler", fmt.Sprint(idx))
			_ = json.NewEncoder(w).Encode(p)
		})
	}
	req := &http.Request{Method: rc.Method, URL: u, Header: http.Header{}, RequestURI: rc.Target}
	rec := httptest.NewRecorder()
	mux.ServeHTTP(rec, req)
	switch rec.Code {
	case http.StatusOK:
		v.Outcome = "matched"
		fmt.Sscan(rec.Header().Get("X-Abada-Handler"), &v.Handler)
		if err := json.Unmarshal(rec.Body.Bytes(), &v.Params); err != nil {
			fail("case %s: params: %v", rc.Name, err)
		}
	case http.StatusNotFound:
		v.Outcome = "not_found"
	// The default routing error handler answers 501 for a wrong method.
	case http.StatusMethodNotAllowed, http.StatusNotImplemented:
		v.Outcome = "method_not_allowed"
	case http.StatusBadRequest:
		v.Outcome = "bad_request"
	default:
		fail("case %s: unexpected status %d", rc.Name, rec.Code)
	}
	return v
}

func sameRoute(a, b routeVector) bool {
	if a.Outcome != b.Outcome || a.Handler != b.Handler || len(a.Params) != len(b.Params) {
		return false
	}
	for k, x := range a.Params {
		if b.Params[k] != x {
			return false
		}
	}
	return true
}

var (
	literals = []string{"v1", "a", "b", "users", "x-y", "a.b", "~t", "%2F", "%zz", "c:d", "", "*", "**"}
	idents   = []string{"name", "id", "a", "a.b", "_x", "1a", "a-b", ""}
	verbs    = []string{"", ":get", ":a:b", ":", ":%20"}
)

func randomSegment(rng *rand.Rand, depth int) string {
	switch rng.Intn(6) {
	case 0:
		return "*"
	case 1:
		return "**"
	case 2, 3:
		return literals[rng.Intn(len(literals))]
	default:
		id := idents[rng.Intn(len(idents))]
		if depth > 1 || rng.Intn(2) == 0 {
			return "{" + id + "}"
		}
		return "{" + id + "=" + randomSegments(rng, depth+1) + "}"
	}
}

func randomSegments(rng *rand.Rand, depth int) string {
	n := 1 + rng.Intn(3)
	parts := make([]string, n)
	for i := range parts {
		parts[i] = randomSegment(rng, depth)
	}
	return strings.Join(parts, "/")
}

func randomTemplate(rng *rand.Rand) string {
	prefix := "/"
	if rng.Intn(20) == 0 {
		prefix = ""
	}
	return prefix + randomSegments(rng, 0) + verbs[rng.Intn(len(verbs))]
}

var (
	validTemplates = []string{
		"/v1/{name}", "/v1/{name=users/*}", "/v1/{name=**}", "/v1/{name}:get",
		"/v1/{parent=projects/*}/items/{id}", "/v1/**/end", "/v1/*:a:b", "/",
		"/v1/{name=a/*/b/**}", "/v1/users/{id}:start",
	}
	targetParts = []string{"v1", "users", "a", "b", "end", "projects", "items", "x%2Fy", "x%2fy", "%20", "%zz", "a:get", ":get", "a:a:b", "a%3Aget", "~", "", "%25"}
	methods     = []string{"GET", "GET", "GET", "POST"}
	modes       = []string{"legacy", "all_except_reserved", "all_except_slash", "all_characters"}
)

func randomRoute(rng *rand.Rand, i int) routeCase {
	n := 1 + rng.Intn(3)
	hs := make([]handlerSpec, n)
	for j := range hs {
		hs[j] = handlerSpec{Method: methods[rng.Intn(len(methods))], Template: validTemplates[rng.Intn(len(validTemplates))]}
	}
	target := expand(rng, hs[rng.Intn(n)].Template)
	if rng.Intn(10) < 3 {
		parts := make([]string, 1+rng.Intn(4))
		for j := range parts {
			parts[j] = targetParts[rng.Intn(len(targetParts))]
		}
		target = "/" + strings.Join(parts, "/")
	}
	return routeCase{
		Name:     fmt.Sprintf("random-%03d", i),
		Mode:     modes[rng.Intn(len(modes))],
		Handlers: hs,
		Method:   methods[rng.Intn(len(methods))],
		Target:   target,
	}
}

// expand turns a template into a request target that may match it: variables
// and wildcards become random components, and the verb is sometimes dropped.
func expand(rng *rand.Rand, tmpl string) string {
	body, verb := tmpl, ""
	if i := strings.LastIndex(tmpl, ":"); i > strings.LastIndex(tmpl, "}") {
		body, verb = tmpl[:i], tmpl[i:]
	}
	var b strings.Builder
	for i := 0; i < len(body); {
		switch {
		case body[i] == '{':
			j := strings.IndexByte(body[i:], '}') + i
			inner := body[i+1 : j]
			if k := strings.IndexByte(inner, '='); k >= 0 {
				b.WriteString(strings.TrimPrefix(expand(rng, "/"+inner[k+1:]), "/"))
			} else {
				b.WriteString(targetParts[rng.Intn(len(targetParts))])
			}
			i = j + 1
		case strings.HasPrefix(body[i:], "**"):
			k := 1 + rng.Intn(3)
			for n := 0; n < k; n++ {
				if n > 0 {
					b.WriteByte('/')
				}
				b.WriteString(targetParts[rng.Intn(len(targetParts))])
			}
			i += 2
		case body[i] == '*':
			b.WriteString(targetParts[rng.Intn(len(targetParts))])
			i++
		default:
			b.WriteByte(body[i])
			i++
		}
	}
	if rng.Intn(5) != 0 {
		b.WriteString(verb)
	}
	return b.String()
}
