package main

import (
	"fmt"
	"net/http"
	"net/url"
	"os"
	"sort"
	"testing"

	"github.com/grpc-ecosystem/grpc-gateway/v2/internal/httprule"
	"github.com/grpc-ecosystem/grpc-gateway/v2/runtime"
)

type nopWriter struct{ h http.Header }

func (w *nopWriter) Header() http.Header         { return w.h }
func (w *nopWriter) Write(b []byte) (int, error) { return len(b), nil }
func (w *nopWriter) WriteHeader(int)             {}

// runBench measures grpc-gateway's ServeMux routing over the same canonical
// requests abada's route_bench uses. The handler does nothing, so what is
// measured is the parse of the target plus the routing.
func runBench(path string) {
	raw := captureContract(path)
	mux := runtime.NewServeMux()
	matched := 0
	for _, b := range raw.Bindings {
		comp, _ := httprule.Parse(b.Template)
		t := comp.Compile()
		pat, _ := runtime.NewPattern(t.Version, t.OpCodes, t.Pool, t.Verb)
		mux.Handle(b.HTTPMethod, pat, func(http.ResponseWriter, *http.Request, map[string]string) { matched++ })
	}
	type req struct{ method, target string }
	var reqs []req
	for _, r := range raw.Routes {
		if r.Canonical {
			reqs = append(reqs, req{r.Method, r.Target})
		}
	}
	w := &nopWriter{h: http.Header{}}
	var samples []float64
	for s := 0; s < 7; s++ {
		res := testing.Benchmark(func(b *testing.B) {
			for i := 0; i < b.N; i++ {
				q := reqs[i%len(reqs)]
				u, _ := url.ParseRequestURI(q.target)
				mux.ServeHTTP(w, &http.Request{Method: q.method, URL: u, Header: http.Header{}})
			}
		})
		samples = append(samples, float64(res.NsPerOp()))
	}
	sort.Float64s(samples)
	fmt.Fprintf(os.Stdout, "grpc-gateway route: %d bindings, %d requests, median %.0f ns/request (min %.0f, max %.0f)\n",
		len(raw.Bindings), len(reqs), samples[3], samples[0], samples[6])
}
