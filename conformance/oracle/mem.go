package main

// The mem subcommand measures what grpc-gateway's own request path costs in
// memory for a JSON body, so `abada-security` S5 ("not worse than Go's memory
// floor") is a comparison and not an assertion. The abada side of the same
// measurement is crates/abada/examples/decode_mem.rs.
//
//	abadaoracle mem <descriptor set> [body ...]
//
// Two stages are reported per body, because they are what the two sides must
// be compared at:
//
//   - decode: runtime.JSONPb (the mux's own default marshaler) into a
//     dynamicpb message — exactly what a generated handler does with a body.
//   - +encode: the same, then proto.Marshal of the decoded message, which is
//     what the handler hands to the gRPC client. A gateway holds both at once,
//     so this is the number to compare against a gateway measurement.
//
// Three numbers per stage, from runtime.MemStats with the GC off
// (debug.SetGCPercent(-1)), so nothing is collected mid-measurement and the
// figures are allocation, not residency-after-GC:
//
//   - peak: HeapAlloc at the end of the stage minus at the start. With the GC
//     off and every temporary still uncollected, this IS the high-water mark.
//   - resident: HeapAlloc after the temporaries are dropped and one GC has
//     run, with the decoded message still alive — the message's own cost.
//   - total: TotalAlloc over the stage, every byte handed out.
//
// The bodies are the shapes of the S5 table; a body given on the command line
// replaces them. This writes no vector: a memory figure is not stable across
// machines or Go builds, so it is read by hand into
// docs/readiness/grpc-gateway-parity.md with its conditions, never committed
// as an expectation.

import (
	"fmt"
	"net/url"
	"os"
	goruntime "runtime"
	"runtime/debug"
	"strings"
	"time"

	"google.golang.org/protobuf/proto"
	"google.golang.org/protobuf/reflect/protoreflect"
	"google.golang.org/protobuf/reflect/protoregistry"

	"github.com/grpc-ecosystem/grpc-gateway/v2/runtime"
	"github.com/grpc-ecosystem/grpc-gateway/v2/utilities"
)

const memMessage = "abada.conformance.request.v1.Everything"

type memBody struct {
	name string
	json []byte
}

func memBodies() []memBody {
	var elems strings.Builder
	for i := 0; i < 1000000; i++ {
		elems.WriteString("0,")
	}
	tenPow6 := "{\"rInt32\":[" + elems.String() + "0]}"

	elems.Reset()
	for i := 0; i < 100000; i++ {
		elems.WriteString("0,")
	}
	tenPow5 := "{\"rInt32\":[" + elems.String() + "0]}"

	elems.Reset()
	for i := 0; i < 100000; i++ {
		elems.WriteString("\"abcdefgh\",")
	}
	strings100k := "{\"rString\":[" + elems.String() + "\"z\"]}"

	elems.Reset()
	for i := 0; i < 100000; i++ {
		fmt.Fprintf(&elems, "\"k%d\":\"v\",", i)
	}
	mapBody := "{\"mStr\":{" + elems.String() + "\"z\":\"\"}}"

	elems.Reset()
	for i := 0; i < 10000; i++ {
		elems.WriteString("{\"fInt32\":1},")
	}
	items := "{\"items\":[" + elems.String() + "{}]}"

	return []memBody{
		{"rInt32, 10^6 elements", []byte(tenPow6)},
		{"rInt32, 10^5 elements", []byte(tenPow5)},
		{"rString, 10^5 elements", []byte(strings100k)},
		{"mStr, 10^5 entries", []byte(mapBody)},
		{"items, 10^4 messages", []byte(items)},
		{"fString, 1 MiB", []byte("{\"fString\":\"" + strings.Repeat("a", 1<<20) + "\"}")},
	}
}

type memResult struct {
	// PeakGCOff is HeapAlloc at the end of the stage with the collector off:
	// nothing was collected, so it is every byte still live, an upper bound on
	// what a process would ever hold at once.
	peakGCOff uint64
	// PeakGCOn is the maximum HeapAlloc a sampler saw every 200 µs with the
	// collector at its default: what a live process shows, and the method the
	// first S5 table used. It is a sample, so it can miss a short spike; the
	// two numbers bracket the truth.
	peakGCOn              uint64
	resident, total       uint64
	millis, millisSampled float64
}

func heapAlloc() uint64 {
	var m goruntime.MemStats
	goruntime.ReadMemStats(&m)
	return m.HeapAlloc
}

func totalAlloc() uint64 {
	var m goruntime.MemStats
	goruntime.ReadMemStats(&m)
	return m.TotalAlloc
}

// stage is the work being measured: decode a body into a dynamicpb message
// and, for the "+encode" stage, marshal it the way the generated handler does
// before the gRPC call. The message is returned so the caller decides when it
// dies.
func memStage(body []byte, alsoEncode bool) (proto.Message, float64) {
	marshaler := defaultMarshaler()
	t := time.Now()
	msg := newMessage(memMessage)
	if err := marshaler.Unmarshal(body, msg); err != nil {
		fmt.Fprintf(os.Stderr, "  (decode failed: %v)\n", err)
	}
	if alsoEncode {
		wire, err := proto.Marshal(msg)
		if err != nil {
			fmt.Fprintf(os.Stderr, "  (encode failed: %v)\n", err)
		}
		goruntime.KeepAlive(wire)
	}
	return msg, float64(time.Since(t).Microseconds()) / 1e3
}

// sampledPeak runs the stage with the collector at its default and a goroutine
// reading HeapAlloc every 200 µs, returning the largest it saw. This is what a
// live process holds; measureMem's GC-off figure is the upper bound beside it.
func sampledPeak(body []byte, alsoEncode bool) (uint64, float64) {
	goruntime.GC()
	base := heapAlloc()
	stop := make(chan struct{})
	done := make(chan uint64)
	go func() {
		var max uint64
		for {
			select {
			case <-stop:
				done <- max
				return
			default:
			}
			if h := heapAlloc(); h > base && h-base > max {
				max = h - base
			}
			time.Sleep(200 * time.Microsecond)
		}
	}()
	msg, millis := memStage(body, alsoEncode)
	goruntime.KeepAlive(msg)
	close(stop)
	return <-done, millis
}

// measureMem runs one stage with the GC off: HeapAlloc never goes down while
// it runs, so its value at the end is the high-water mark of the stage.
func measureMem(body []byte, alsoEncode bool) memResult {
	goruntime.GC()
	was := debug.SetGCPercent(-1)
	defer debug.SetGCPercent(was)

	start, total0 := heapAlloc(), totalAlloc()
	msg, millis := memStage(body, alsoEncode)
	peak, total := heapAlloc(), totalAlloc()
	// The decoded message is what stays; the wire buffer of the "+encode"
	// stage died inside memStage after the peak was reached. Collect, with the
	// message still alive, so "resident" is the message alone.
	debug.SetGCPercent(was)
	goruntime.GC()
	resident := heapAlloc()
	goruntime.KeepAlive(msg)
	debug.SetGCPercent(-1)
	sampled, millisSampled := sampledPeak(body, alsoEncode)
	return memResult{
		peakGCOff:     peak - start,
		peakGCOn:      sampled,
		resident:      resident - start,
		total:         total - total0,
		millis:        millis,
		millisSampled: millisSampled,
	}
}

func runMem(args []string) {
	query := false
	if len(args) > 0 && args[0] == "-query" {
		query, args = true, args[1:]
	}
	if len(args) == 0 {
		fail("usage: mem [-query] <descriptor set> [body ...]")
	}
	loadContractTypes(args[0])
	if _, err := protoregistry.GlobalTypes.FindMessageByName(protoreflect.FullName(memMessage)); err != nil {
		fail("mem: %s is not in %s: %v", memMessage, args[0], err)
	}
	bodies := memBodies()
	if query {
		bodies = queryBodies()
	}
	if len(args) > 1 {
		bodies = nil
		for i, b := range args[1:] {
			bodies = append(bodies, memBody{fmt.Sprintf("argv[%d]", i), []byte(b)})
		}
	}
	fmt.Printf("grpc-gateway %s, %s, %s/%s\n",
		os.Getenv("ABADA_GENERATOR"), goruntime.Version(), goruntime.GOOS, goruntime.GOARCH)
	fmt.Printf("\n%-24s %10s %20s %20s %20s %12s\n",
		"body", "in", "peak (GC off)", "peak (GC on, 200us)", "resident", "total")
	for _, b := range bodies {
		if query {
			// The query walk has one stage: there is no body to decode.
			_ = measureQuery(b.json)
			m := measureQuery(b.json)
			in := float64(len(b.json) + len("/v1/query/x?"))
			fmt.Printf("%-24s %-7s %10d %12d %5.1fx %12d %5.1fx %12d %5.1fx %12d %8.1f ms\n",
				b.name, "query", int(in),
				m.peakGCOff, float64(m.peakGCOff)/in,
				m.peakGCOn, float64(m.peakGCOn)/in,
				m.resident, float64(m.resident)/in,
				m.total, m.millis)
			continue
		}
		for _, stage := range []struct {
			label  string
			encode bool
		}{{"decode", false}, {"+encode", true}} {
			// The first run of a shape also warms protojson's own caches.
			_ = measureMem(b.json, stage.encode)
			m := measureMem(b.json, stage.encode)
			in := float64(len(b.json))
			fmt.Printf("%-24s %-7s %10d %12d %5.1fx %12d %5.1fx %12d %5.1fx %12d %8.1f ms\n",
				b.name, stage.label, len(b.json),
				m.peakGCOff, float64(m.peakGCOff)/in,
				m.peakGCOn, float64(m.peakGCOn)/in,
				m.resident, float64(m.resident)/in,
				m.total, m.millis)
		}
	}
}

// queryBodies are the query strings of the S5 table's query rows, in the shape
// crates/abada/tests/security.rs builds them: `keys` keys of 100 components
// each, diverging from one another at the top (`oMsg` where a bit of the key
// number is set) so each one builds messages of its own instead of sharing a
// prefix, padded to `uriLen` bytes.
func divergentDeepQuery(keys, uriLen int) string {
	bits := 0
	for 1<<bits < keys {
		bits++
	}
	bits++
	var qs []string
	for i := 0; i < keys; i++ {
		path := make([]string, 99)
		for l := range path {
			path[l] = "nested"
		}
		for l := 0; l < bits && l < len(path); l++ {
			if i>>l&1 == 1 {
				path[l] = "oMsg"
			}
		}
		qs = append(qs, strings.Join(path, ".")+".fString=1")
	}
	q := strings.Join(qs, "&")
	if len(q) > uriLen {
		fail("mem: %d keys are longer than %d", keys, uriLen)
	}
	return q + strings.Repeat("1", uriLen-len(q))
}

func queryBodies() []memBody {
	nested := func(n int) string {
		parts := make([]string, n)
		for i := range parts {
			parts[i] = "nested"
		}
		return strings.Join(parts, ".")
	}
	return []memBody{
		{"query: 94 divergent 100-deep keys", []byte(divergentDeepQuery(94, 65530-len("/v1/query/x?")))},
		{"query: 100-deep field path", []byte(nested(100) + "=1")},
		{"query: 4000-deep field path", []byte(nested(4000) + "=1")},
		{"query: 9000-deep field path", []byte(nested(9000) + "=1")},
	}
}

// memQueryStage is what a generated handler does with a query string:
// url.ParseQuery (net/http's Request.ParseForm) then
// runtime.PopulateQueryParameters with an empty filter — grpc-gateway has no
// depth limit on this walk, which is the deviation abada's 100-level limit is.
func memQueryStage(query []byte) (proto.Message, float64) {
	t := time.Now()
	values, err := url.ParseQuery(string(query))
	if err != nil {
		fmt.Fprintf(os.Stderr, "  (query does not parse: %v)\n", err)
	}
	msg := newMessage(memMessage)
	if err := runtime.PopulateQueryParameters(msg, values, utilities.NewDoubleArray(nil)); err != nil {
		fmt.Fprintf(os.Stderr, "  (populate failed: %v)\n", err)
	}
	return msg, float64(time.Since(t).Microseconds()) / 1e3
}

func measureQuery(query []byte) memResult {
	goruntime.GC()
	was := debug.SetGCPercent(-1)
	defer debug.SetGCPercent(was)
	start, total0 := heapAlloc(), totalAlloc()
	msg, millis := memQueryStage(query)
	peak, total := heapAlloc(), totalAlloc()
	debug.SetGCPercent(was)
	goruntime.GC()
	resident := heapAlloc()
	goruntime.KeepAlive(msg)
	debug.SetGCPercent(-1)
	return memResult{
		peakGCOff: peak - start,
		peakGCOn:  peak - start,
		resident:  resident - start,
		total:     total - total0,
		millis:    millis,
	}
}
