package main

// The metadata subcommand drives runtime.AnnotateContext, the part of
// grpc-gateway that turns incoming HTTP headers into outgoing gRPC metadata
// (runtime/context.go) and the request-side matcher it uses by default
// (runtime/mux.go's DefaultHeaderMatcher). No wire exchange is needed here:
// AnnotateContext is a pure function of a *http.Request, not something that
// writes an HTTP response.

import (
	"encoding/json"
	"net/http"
	"os"
	"sort"

	"github.com/grpc-ecosystem/grpc-gateway/v2/runtime"
	"google.golang.org/grpc/metadata"
)

type headerLine [2]string

type metadataCase struct {
	Name       string       `json:"name"`
	Headers    []headerLine `json:"headers"`
	Host       string       `json:"host,omitempty"`
	RemoteAddr string       `json:"remote_addr,omitempty"`
}

type metadataCases struct {
	Cases []metadataCase `json:"cases"`
}

type metadataVector struct {
	metadataCase
	// Pairs is every (key, value) grpc-gateway would send to the RPC, key
	// sorted (metadata is a map, not a sequence — grpc-gateway does not
	// promise cross-key order either; DEfaultHeaderMatcher plus a Go map
	// range is not order-stable), values in the order AnnotateContext
	// produced them for that key.
	Pairs []headerLine `json:"pairs"`
	// HasTimeout is whether a Grpc-Timeout header produced a deadline.
	// The deadline's exact duration is not recorded: reading it back from
	// ctx.Deadline() is a wall-clock measurement a few microseconds off
	// from what AnnotateContext actually computed, which would make this
	// committed file change on every regeneration for no behavioural
	// reason. The grammar (which unit multiplies to what) is simple enough
	// to assert directly in Rust — see the unit tests in
	// crates/abada/src/service/metadata.rs — so nothing here needs it.
	HasTimeout bool `json:"has_timeout"`
	// Error is AnnotateContext's error text when it refuses the request
	// outright (a malformed Grpc-Timeout or an undecodable -Bin value);
	// empty otherwise.
	Error string `json:"error,omitempty"`
}

type metadataVectors struct {
	Generator string           `json:"generator"`
	Vectors   []metadataVector `json:"vectors"`
}

func runMetadata() {
	var c metadataCases
	if err := json.NewDecoder(os.Stdin).Decode(&c); err != nil {
		fail("decode metadata cases: %v", err)
	}
	out := metadataVectors{Generator: os.Getenv("ABADA_GENERATOR")}
	for _, mc := range c.Cases {
		out.Vectors = append(out.Vectors, metadataOf(mc))
	}
	enc := json.NewEncoder(os.Stdout)
	enc.SetIndent("", "  ")
	enc.SetEscapeHTML(false)
	if err := enc.Encode(out); err != nil {
		fail("encode: %v", err)
	}
}

func metadataOf(mc metadataCase) metadataVector {
	v := metadataVector{metadataCase: mc}
	req := &http.Request{
		Method:     "GET",
		Header:     http.Header{},
		Host:       mc.Host,
		RemoteAddr: mc.RemoteAddr,
	}
	for _, h := range mc.Headers {
		req.Header.Add(h[0], h[1])
	}
	mux := runtime.NewServeMux()
	ctx, err := runtime.AnnotateContext(req.Context(), mux, req, "/abada.metadata.v1.Probe/Call")
	if err != nil {
		v.Error = err.Error()
		return v
	}
	if md, ok := metadata.FromOutgoingContext(ctx); ok {
		v.Pairs = pairsOf(md)
	} else {
		v.Pairs = []headerLine{}
	}
	_, v.HasTimeout = ctx.Deadline()
	return v
}

func pairsOf(md metadata.MD) []headerLine {
	keys := make([]string, 0, len(md))
	for k := range md {
		keys = append(keys, k)
	}
	sort.Strings(keys)
	var out []headerLine
	for _, k := range keys {
		for _, val := range md[k] {
			out = append(out, headerLine{k, val})
		}
	}
	if out == nil {
		out = []headerLine{}
	}
	return out
}
