package main

// The errors subcommand drives grpc-gateway's DefaultHTTPErrorHandler and
// DefaultRoutingErrorHandler behind a real net/http server and records what
// reaches the client over HTTP/1.1: status, headers, body and trailers.

import (
	"bufio"
	"bytes"
	"encoding/hex"
	"encoding/json"
	"fmt"
	"io"
	"log"
	"net"
	"net/http"
	"net/http/httptest"
	"os"
	"sort"
	"strconv"
	"strings"
	"unicode/utf8"

	"github.com/grpc-ecosystem/grpc-gateway/v2/runtime"
	spb "google.golang.org/genproto/googleapis/rpc/status"
	"google.golang.org/grpc/codes"
	"google.golang.org/grpc/grpclog"
	"google.golang.org/grpc/metadata"
	"google.golang.org/grpc/status"
	"google.golang.org/protobuf/reflect/protoregistry"
	"google.golang.org/protobuf/types/known/anypb"
)

type anyCase struct {
	TypeURL  string `json:"type_url,omitempty"`
	ValueHex string `json:"value_hex,omitempty"`
}

type statusCase struct {
	Name    string    `json:"name"`
	Code    int32     `json:"code"`
	Message string    `json:"message"`
	Details []anyCase `json:"details,omitempty"`
	// Metadata is whether the call produced server metadata at all; HeaderMD
	// and TrailerMD are its pairs, in order.
	Metadata  bool        `json:"metadata,omitempty"`
	HeaderMD  [][2]string `json:"header_md,omitempty"`
	TrailerMD [][2]string `json:"trailer_md,omitempty"`
	TE        string      `json:"te,omitempty"`
}

type errorCases struct {
	Codes    []int32      `json:"codes"`
	Statuses []statusCase `json:"statuses"`
	Routes   []routeCase  `json:"routes"`
}

type wire struct {
	Status   int         `json:"status"`
	Headers  [][2]string `json:"headers"`
	Body     string      `json:"body"`
	Trailers [][2]string `json:"trailers,omitempty"`
}

type statusVector struct {
	statusCase
	// Registered lists, per detail, whether this binary's global registry
	// resolves its type URL — the only thing protojson needs to render it.
	Registered []bool `json:"registered,omitempty"`
	wire
}

type routeErrorVector struct {
	routeCase
	wire
}

type codeVector struct {
	Code   int32 `json:"code"`
	Status int   `json:"status"`
}

type errorVectors struct {
	Generator string `json:"generator"`
	Note      string `json:"note"`
	// NotPrintable lists, as [first, last] pairs, the runes in U+0080..U+07FF
	// strconv.Quote escapes: the only non-ASCII runes a malformed-escape
	// sequence ("%" and two bytes) can hold.
	NotPrintable [][2]int           `json:"quote_not_printable_2byte"`
	Codes        []codeVector       `json:"codes"`
	Statuses     []statusVector     `json:"statuses"`
	Routes       []routeErrorVector `json:"routes"`
	Dropped      []string           `json:"dropped_nondeterministic,omitempty"`
}

const bodyNote = "Headers exclude date, content-length, connection and transfer-encoding, which net/http adds " +
	"for framing; names are lower-cased and values of the trailer header sorted, because grpc-gateway " +
	"announces trailers in Go map order. protojson inserts a random space after each comma, chosen " +
	"per binary (internal/detrand); bodies are recorded without it. The two fallback bodies are " +
	"constants in runtime/errors.go and are recorded byte for byte."

var fallbacks = []string{
	`{"code": 13, "message": "failed to marshal error message"}`,
	`{"code": 13, "message": "failed to rewrite error message"}`,
}

func runErrors() {
	// Failed marshals and superfluous WriteHeader calls are the behaviour
	// under test, not news.
	grpclog.SetLoggerV2(grpclog.NewLoggerV2(io.Discard, io.Discard, io.Discard))
	var c errorCases
	if err := json.NewDecoder(os.Stdin).Decode(&c); err != nil {
		fail("decode error cases: %v", err)
	}
	out := errorVectors{Generator: os.Getenv("ABADA_GENERATOR"), Note: bodyNote, NotPrintable: notPrintable()}
	for _, code := range c.Codes {
		out.Codes = append(out.Codes, codeVector{code, runtime.HTTPStatusFromCode(codes.Code(uint32(code)))})
	}
	for _, sc := range c.Statuses {
		out.Statuses = append(out.Statuses, statusOf(sc))
	}
	for _, rc := range c.Routes {
		first := routeError(rc)
		deterministic := true
		for i := 0; i < 32; i++ {
			if again := routeError(rc); !sameWire(first.wire, again.wire) {
				deterministic = false
				break
			}
		}
		if !deterministic {
			out.Dropped = append(out.Dropped, rc.Name)
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

func notPrintable() [][2]int {
	var out [][2]int
	for r := 0x80; r < 0x800; r++ {
		if strconv.IsPrint(rune(r)) {
			continue
		}
		if n := len(out); n > 0 && out[n-1][1] == r-1 {
			out[n-1][1] = r
		} else {
			out = append(out, [2]int{r, r})
		}
	}
	return out
}

func mdOf(pairs [][2]string) metadata.MD {
	md := metadata.MD{}
	for _, p := range pairs {
		md.Append(p[0], p[1])
	}
	return md
}

func statusOf(sc statusCase) statusVector {
	v := statusVector{statusCase: sc}
	sp := &spb.Status{Code: sc.Code, Message: sc.Message}
	for _, d := range sc.Details {
		value, err := hex.DecodeString(d.ValueHex)
		if err != nil {
			fail("case %s: value_hex: %v", sc.Name, err)
		}
		sp.Details = append(sp.Details, &anypb.Any{TypeUrl: d.TypeURL, Value: value})
		_, rerr := protoregistry.GlobalTypes.FindMessageByURL(d.TypeURL)
		v.Registered = append(v.Registered, rerr == nil)
	}
	mux := runtime.NewServeMux()
	handler := http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		ctx := r.Context()
		if sc.Metadata {
			ctx = runtime.NewServerMetadataContext(ctx, runtime.ServerMetadata{
				HeaderMD: mdOf(sc.HeaderMD), TrailerMD: mdOf(sc.TrailerMD),
			})
		}
		_, outbound := runtime.MarshalerForRequest(mux, r)
		// ErrorProto returns nil for code 0, as a gRPC client would.
		runtime.HTTPError(ctx, mux, outbound, w, r, status.ErrorProto(sp))
	})
	extra := ""
	if sc.TE != "" {
		extra = "TE: " + sc.TE + "\r\n"
	}
	v.wire = exchange(handler, "GET", "/error", extra, sc.Name)
	return v
}

func routeError(rc routeCase) routeErrorVector {
	mux := runtime.NewServeMux(runtime.WithUnescapingMode(mode(rc.Mode)))
	for i, h := range rc.Handlers {
		idx := i
		if err := mux.HandlePath(h.Method, h.Template, func(w http.ResponseWriter, _ *http.Request, _ map[string]string) {
			fmt.Fprintf(w, "handler %d", idx)
		}); err != nil {
			fail("case %s: handler %d: %v", rc.Name, i, err)
		}
	}
	return routeErrorVector{routeCase: rc, wire: exchange(mux, rc.Method, rc.Target, "", rc.Name)}
}

// exchange sends one raw HTTP/1.1 request, so the target reaches the server
// as written, and parses the raw answer, so nothing a client library
// normalises is hidden.
func exchange(h http.Handler, method, target, extra, name string) wire {
	srv := httptest.NewUnstartedServer(h)
	srv.Config.ErrorLog = log.New(io.Discard, "", 0)
	srv.Start()
	defer srv.Close()
	conn, err := net.Dial("tcp", srv.Listener.Addr().String())
	if err != nil {
		fail("case %s: dial: %v", name, err)
	}
	defer conn.Close()
	fmt.Fprintf(conn, "%s %s HTTP/1.1\r\nHost: abada\r\n%sConnection: close\r\n\r\n", method, target, extra)
	raw, err := io.ReadAll(conn)
	if err != nil {
		fail("case %s: read: %v", name, err)
	}
	return parseWire(raw, name)
}

var framing = map[string]bool{"date": true, "content-length": true, "connection": true, "transfer-encoding": true}

func headerLines(block []byte, name string) (all [][2]string, kept [][2]string) {
	for _, line := range strings.Split(string(block), "\r\n") {
		if line == "" {
			continue
		}
		k, v, ok := strings.Cut(line, ":")
		if !ok {
			fail("case %s: header line %q", name, line)
		}
		kv := [2]string{strings.ToLower(k), strings.TrimPrefix(v, " ")}
		all = append(all, kv)
		if !framing[kv[0]] {
			kept = append(kept, kv)
		}
	}
	sort.SliceStable(kept, func(i, j int) bool {
		if kept[i][0] != kept[j][0] {
			return kept[i][0] < kept[j][0]
		}
		return kept[i][0] == "trailer" && kept[i][1] < kept[j][1]
	})
	return all, kept
}

func parseWire(raw []byte, name string) wire {
	head, rest, ok := bytes.Cut(raw, []byte("\r\n\r\n"))
	if !ok {
		fail("case %s: no end of headers in %q", name, raw)
	}
	statusLine, headerBlock, _ := bytes.Cut(head, []byte("\r\n"))
	fields := strings.Fields(string(statusLine))
	if len(fields) < 2 {
		fail("case %s: status line %q", name, statusLine)
	}
	var w wire
	w.Status, _ = strconv.Atoi(fields[1])
	all, kept := headerLines(headerBlock, name)
	w.Headers = kept
	if w.Headers == nil {
		w.Headers = [][2]string{}
	}
	chunked := false
	for _, kv := range all {
		if kv[0] == "transfer-encoding" && kv[1] == "chunked" {
			chunked = true
		}
	}
	body := rest
	if chunked {
		body = nil
		r := bufio.NewReader(bytes.NewReader(rest))
		for {
			line, err := r.ReadString('\n')
			if err != nil {
				fail("case %s: chunk size: %v", name, err)
			}
			n, err := strconv.ParseInt(strings.TrimSpace(line), 16, 64)
			if err != nil {
				fail("case %s: chunk size %q", name, line)
			}
			if n == 0 {
				tail, _ := io.ReadAll(r)
				_, w.Trailers = headerLines(tail, name)
				break
			}
			chunk := make([]byte, n+2)
			if _, err := io.ReadFull(r, chunk); err != nil {
				fail("case %s: chunk: %v", name, err)
			}
			body = append(body, chunk[:n]...)
		}
	}
	if !utf8.Valid(body) {
		fail("case %s: body is not UTF-8: %q", name, body)
	}
	w.Body = withoutDetrand(string(body))
	return w
}

// withoutDetrand removes the whitespace protojson adds outside strings, value
// by value, leaving the fallback constants as they are.
func withoutDetrand(body string) string {
	var out strings.Builder
	for len(body) > 0 {
		end := topLevelEnd(body)
		value := body[:end]
		body = body[end:]
		if isFallback(value) || !strings.HasPrefix(value, "{") {
			out.WriteString(value)
			continue
		}
		inString, escaped := false, false
		for i := 0; i < len(value); i++ {
			c := value[i]
			switch {
			case inString && escaped:
				escaped = false
			case inString && c == '\\':
				escaped = true
			case c == '"':
				inString = !inString
			case !inString && c == ' ':
				continue
			}
			out.WriteByte(c)
		}
	}
	return out.String()
}

func isFallback(s string) bool {
	for _, f := range fallbacks {
		if s == f {
			return true
		}
	}
	return false
}

// topLevelEnd is the length of the first JSON object at the start of s, or
// len(s) when s does not start with one.
func topLevelEnd(s string) int {
	if !strings.HasPrefix(s, "{") {
		return len(s)
	}
	depth, inString, escaped := 0, false, false
	for i := 0; i < len(s); i++ {
		c := s[i]
		switch {
		case inString && escaped:
			escaped = false
		case inString && c == '\\':
			escaped = true
		case c == '"':
			inString = !inString
		case !inString && c == '{':
			depth++
		case !inString && c == '}':
			depth--
			if depth == 0 {
				return i + 1
			}
		}
	}
	return len(s)
}

func sameWire(a, b wire) bool {
	x, _ := json.Marshal(a)
	y, _ := json.Marshal(b)
	return bytes.Equal(x, y)
}
