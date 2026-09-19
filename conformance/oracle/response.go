package main

// The response subcommand drives ForwardResponseMessage (runtime/handler.go)
// with the default marshaler, behind a real net/http server, over the same
// wire-exchange machinery errors.go uses (trailers need real HTTP/1.1
// framing). The response message is google.rpc.Status, reused only as a
// convenient real proto message already linked by this binary —
// ForwardResponseMessage does not care what it forwards, and abada's own
// JSON codec is already proven correct elsewhere (docs/DESIGN.md, "JSON");
// this is about headers, trailers and status, not encoding.

import (
	"encoding/json"
	"net/http"
	"os"

	spb "google.golang.org/genproto/googleapis/rpc/status"

	"github.com/grpc-ecosystem/grpc-gateway/v2/runtime"
)

type responseCase struct {
	Name    string `json:"name"`
	Code    int32  `json:"code"`
	Message string `json:"message"`
	// Metadata is whether the call produced server metadata at all; HeaderMD
	// and TrailerMD are its pairs, in order — same shape as errors.go's
	// statusCase, deliberately: it is the same runtime.ServerMetadata.
	Metadata  bool        `json:"metadata,omitempty"`
	HeaderMD  [][2]string `json:"header_md,omitempty"`
	TrailerMD [][2]string `json:"trailer_md,omitempty"`
	TE        string      `json:"te,omitempty"`
}

type responseCases struct {
	Cases []responseCase `json:"cases"`
}

type responseVector struct {
	responseCase
	wire
}

type responseVectors struct {
	Generator string           `json:"generator"`
	Note      string           `json:"note"`
	Vectors   []responseVector `json:"vectors"`
}

func runResponse() {
	var c responseCases
	if err := json.NewDecoder(os.Stdin).Decode(&c); err != nil {
		fail("decode response cases: %v", err)
	}
	out := responseVectors{Generator: os.Getenv("ABADA_GENERATOR"), Note: bodyNote}
	for _, rc := range c.Cases {
		out.Vectors = append(out.Vectors, responseOf(rc))
	}
	enc := json.NewEncoder(os.Stdout)
	enc.SetIndent("", "  ")
	enc.SetEscapeHTML(false)
	if err := enc.Encode(out); err != nil {
		fail("encode: %v", err)
	}
}

func responseOf(rc responseCase) responseVector {
	v := responseVector{responseCase: rc}
	msg := &spb.Status{Code: rc.Code, Message: rc.Message}
	mux := runtime.NewServeMux()
	handler := http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		ctx := r.Context()
		if rc.Metadata {
			ctx = runtime.NewServerMetadataContext(ctx, runtime.ServerMetadata{
				HeaderMD: mdOf(rc.HeaderMD), TrailerMD: mdOf(rc.TrailerMD),
			})
		}
		marshaler, _ := runtime.MarshalerForRequest(mux, r)
		runtime.ForwardResponseMessage(ctx, mux, marshaler, w, r, msg)
	})
	extra := ""
	if rc.TE != "" {
		extra = "TE: " + rc.TE + "\r\n"
	}
	v.wire = exchange(handler, "GET", "/ok", extra, rc.Name)
	return v
}
