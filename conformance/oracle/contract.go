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
	"google.golang.org/genproto/googleapis/api/annotations"
	"google.golang.org/protobuf/proto"
	"google.golang.org/protobuf/reflect/protodesc"
	"google.golang.org/protobuf/reflect/protoreflect"
	"google.golang.org/protobuf/types/descriptorpb"
)

type binding struct {
	Service         string `json:"service"`
	Method          string `json:"method"`
	HTTPMethod      string `json:"http_method"`
	Template        string `json:"template"`
	Body            string `json:"body,omitempty"`
	ResponseBody    string `json:"response_body,omitempty"`
	ClientStreaming bool   `json:"client_streaming,omitempty"`
	ServerStreaming bool   `json:"server_streaming,omitempty"`
	Index           int    `json:"index"`
}

type contractRoute struct {
	// From is the binding the target was drawn from.
	From      int               `json:"from"`
	Canonical bool              `json:"canonical,omitempty"`
	Mode      string            `json:"mode"`
	Method    string            `json:"method"`
	Target    string            `json:"target"`
	Outcome   string            `json:"outcome"`
	Handler   int               `json:"handler,omitempty"`
	Params    map[string]string `json:"params,omitempty"`
}

type contractVectors struct {
	Generator string          `json:"generator"`
	Source    string          `json:"source"`
	Bindings  []binding       `json:"bindings"`
	Routes    []contractRoute `json:"routes"`
	Dropped   int             `json:"dropped_nondeterministic"`
}

func ruleBindings(svc protoreflect.ServiceDescriptor, m protoreflect.MethodDescriptor, r *annotations.HttpRule, index int) binding {
	b := binding{
		Service: string(svc.FullName()), Method: string(m.Name()),
		Body: r.GetBody(), ResponseBody: r.GetResponseBody(),
		ClientStreaming: m.IsStreamingClient(), ServerStreaming: m.IsStreamingServer(),
		Index: index,
	}
	switch p := r.GetPattern().(type) {
	case *annotations.HttpRule_Get:
		b.HTTPMethod, b.Template = "GET", p.Get
	case *annotations.HttpRule_Put:
		b.HTTPMethod, b.Template = "PUT", p.Put
	case *annotations.HttpRule_Post:
		b.HTTPMethod, b.Template = "POST", p.Post
	case *annotations.HttpRule_Delete:
		b.HTTPMethod, b.Template = "DELETE", p.Delete
	case *annotations.HttpRule_Patch:
		b.HTTPMethod, b.Template = "PATCH", p.Patch
	case *annotations.HttpRule_Custom:
		b.HTTPMethod, b.Template = p.Custom.GetKind(), p.Custom.GetPath()
	default:
		fail("%s.%s: rule without pattern", svc.FullName(), m.Name())
	}
	return b
}

func runContract(path, source string) {
	out := captureContract(path)
	out.Source = source
	enc := json.NewEncoder(os.Stdout)
	enc.SetIndent("", "  ")
	enc.SetEscapeHTML(false)
	if err := enc.Encode(out); err != nil {
		fail("encode: %v", err)
	}
}

func captureContract(path string) contractVectors {
	raw, err := os.ReadFile(path)
	if err != nil {
		fail("read %s: %v", path, err)
	}
	var set descriptorpb.FileDescriptorSet
	if err := proto.Unmarshal(raw, &set); err != nil {
		fail("decode: %v", err)
	}
	files, err := protodesc.NewFiles(&set)
	if err != nil {
		fail("resolve: %v", err)
	}
	out := contractVectors{Generator: os.Getenv("ABADA_GENERATOR")}
	// Declaration order: file order in the set, then services, methods, bindings.
	for _, fd := range set.GetFile() {
		desc, err := files.FindFileByPath(fd.GetName())
		if err != nil {
			fail("%s: %v", fd.GetName(), err)
		}
		for i := 0; i < desc.Services().Len(); i++ {
			svc := desc.Services().Get(i)
			for j := 0; j < svc.Methods().Len(); j++ {
				m := svc.Methods().Get(j)
				rule, ok := proto.GetExtension(m.Options(), annotations.E_Http).(*annotations.HttpRule)
				if !ok || rule == nil || rule.GetPattern() == nil {
					continue
				}
				out.Bindings = append(out.Bindings, ruleBindings(svc, m, rule, 0))
				for k, extra := range rule.GetAdditionalBindings() {
					out.Bindings = append(out.Bindings, ruleBindings(svc, m, extra, k+1))
				}
			}
		}
	}

	// One canonical request per binding, verb kept: it must reach that binding
	// and no other. A miss here is a route the contract cannot serve.
	for from, b := range out.Bindings {
		r := routeContract(out.Bindings, "legacy", b.HTTPMethod, canonical(b.Template))
		r.From = from
		r.Canonical = true
		out.Routes = append(out.Routes, r)
	}

	rng := rand.New(rand.NewSource(2))
	for _, modeName := range []string{"legacy", "all_except_reserved"} {
		for from, b := range out.Bindings {
			for n := 0; n < 6; n++ {
				target := expand(rng, b.Template)
				r := routeContract(out.Bindings, modeName, b.HTTPMethod, target)
				stable := true
				for k := 0; k < 16; k++ {
					if again := routeContract(out.Bindings, modeName, b.HTTPMethod, target); !sameRoute(routeVector{Outcome: r.Outcome, Handler: r.Handler, Params: r.Params}, routeVector{Outcome: again.Outcome, Handler: again.Handler, Params: again.Params}) {
						stable = false
						break
					}
				}
				if !stable {
					out.Dropped++
					continue
				}
				r.From = from
				out.Routes = append(out.Routes, r)
			}
		}
	}
	return out
}

func routeContract(bs []binding, modeName, method, target string) contractRoute {
	r := contractRoute{Mode: modeName, Method: method, Target: target}
	u, err := url.ParseRequestURI(target)
	if err != nil || target == "" || target[0] != '/' {
		r.Outcome = "invalid_target"
		return r
	}
	mux := runtime.NewServeMux(runtime.WithUnescapingMode(mode(modeName)))
	for i, b := range bs {
		comp, err := httprule.Parse(b.Template)
		if err != nil {
			fail("%s.%s: %v", b.Service, b.Method, err)
		}
		t := comp.Compile()
		pat, err := runtime.NewPattern(t.Version, t.OpCodes, t.Pool, t.Verb)
		if err != nil {
			fail("%s.%s: %v", b.Service, b.Method, err)
		}
		idx := i
		mux.Handle(b.HTTPMethod, pat, func(w http.ResponseWriter, _ *http.Request, p map[string]string) {
			w.Header().Set("X-Abada-Handler", fmt.Sprint(idx))
			_ = json.NewEncoder(w).Encode(p)
		})
	}
	rec := httptest.NewRecorder()
	mux.ServeHTTP(rec, &http.Request{Method: method, URL: u, Header: http.Header{}, RequestURI: target})
	switch rec.Code {
	case http.StatusOK:
		r.Outcome = "matched"
		fmt.Sscan(rec.Header().Get("X-Abada-Handler"), &r.Handler)
		_ = json.Unmarshal(rec.Body.Bytes(), &r.Params)
	case http.StatusNotFound:
		r.Outcome = "not_found"
	case http.StatusMethodNotAllowed, http.StatusNotImplemented:
		r.Outcome = "method_not_allowed"
	case http.StatusBadRequest:
		r.Outcome = "bad_request"
	default:
		fail("unexpected status %d for %s %s", rec.Code, method, target)
	}
	return r
}

// canonical fills every variable and wildcard with fixed values and keeps the verb.
func canonical(tmpl string) string {
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
				b.WriteString(strings.TrimPrefix(canonical("/"+inner[k+1:]), "/"))
			} else {
				b.WriteString("x")
			}
			i = j + 1
		case strings.HasPrefix(body[i:], "**"):
			b.WriteString("x/y")
			i += 2
		case body[i] == '*':
			b.WriteString("x")
			i++
		default:
			b.WriteByte(body[i])
			i++
		}
	}
	return b.String() + verb
}

func parseFields(tmpl string) ([]string, error) {
	c, err := httprule.Parse(tmpl)
	if err != nil {
		return nil, err
	}
	return c.Compile().Fields, nil
}
