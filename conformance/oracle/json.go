package main

import (
	"bytes"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"net/http"
	"os"
	"sort"
	"testing"

	"github.com/grpc-ecosystem/grpc-gateway/v2/runtime"
	"google.golang.org/protobuf/encoding/protojson"
	"google.golang.org/protobuf/proto"
	"google.golang.org/protobuf/reflect/protodesc"
	"google.golang.org/protobuf/reflect/protoreflect"
	"google.golang.org/protobuf/reflect/protoregistry"
	"google.golang.org/protobuf/types/descriptorpb"
	"google.golang.org/protobuf/types/dynamicpb"
)

type jsonCase struct {
	Name    string          `json:"name"`
	Message string          `json:"message"`
	Input   json.RawMessage `json:"input"`
	// Repeat, when set, cycles the elements of the input's only array field up
	// to that many: a list response of realistic size without writing it out.
	Repeat int  `json:"repeat,omitempty"`
	Bench  bool `json:"bench,omitempty"`
}

type jsonVector struct {
	jsonCase
	// Accepted is whether the gateway's default inbound marshaler decodes the
	// input into the message, the way a generated handler decodes a body.
	Accepted bool   `json:"accepted"`
	Error    string `json:"error,omitempty"`
	// Output is what the default outbound marshaler (EmitUnpopulated: true)
	// writes for the decoded message, whitespace removed. protojson randomises
	// whitespace on purpose, so only the compacted form is a fixed expectation.
	Output string `json:"output,omitempty"`
	// OutputOmitUnpopulated is the same message with EmitUnpopulated: false,
	// the most common override of the default.
	OutputOmitUnpopulated string `json:"output_omit_unpopulated,omitempty"`
}

type jsonVectors struct {
	Generator string       `json:"generator"`
	Contract  string       `json:"contract"`
	Marshaler string       `json:"marshaler"`
	Cases     []jsonVector `json:"cases"`
}

// loadContractTypes registers every message of the descriptor set that the
// binary does not already link (the google.protobuf ones it does) in the
// global registry. The gateway's default marshaler resolves Any through that
// registry, exactly as it does for generated Go types.
func loadContractTypes(path string) *protoregistry.Files {
	raw, err := os.ReadFile(path)
	if err != nil {
		fail("read %s: %v", path, err)
	}
	var set descriptorpb.FileDescriptorSet
	if err := proto.Unmarshal(raw, &set); err != nil {
		fail("decode %s: %v", path, err)
	}
	files, err := protodesc.NewFiles(&set)
	if err != nil {
		fail("descriptors: %v", err)
	}
	var register func(protoreflect.MessageDescriptors)
	register = func(ms protoreflect.MessageDescriptors) {
		for i := 0; i < ms.Len(); i++ {
			md := ms.Get(i)
			if _, err := protoregistry.GlobalTypes.FindMessageByName(md.FullName()); err != nil {
				if err := protoregistry.GlobalTypes.RegisterMessage(dynamicpb.NewMessageType(md)); err != nil {
					fail("register %s: %v", md.FullName(), err)
				}
			}
			register(md.Messages())
		}
	}
	files.RangeFiles(func(f protoreflect.FileDescriptor) bool {
		register(f.Messages())
		return true
	})
	return files
}

func expandRepeat(c jsonCase) json.RawMessage {
	if c.Repeat == 0 {
		return c.Input
	}
	var obj map[string]json.RawMessage
	if err := json.Unmarshal(c.Input, &obj); err != nil {
		fail("%s: repeat needs an object: %v", c.Name, err)
	}
	done := false
	for k, v := range obj {
		var items []json.RawMessage
		if json.Unmarshal(v, &items) != nil {
			continue
		}
		if done || len(items) == 0 {
			fail("%s: repeat needs exactly one non-empty array field", c.Name)
		}
		out := make([]json.RawMessage, c.Repeat)
		for i := range out {
			out[i] = items[i%len(items)]
		}
		obj[k], _ = json.Marshal(out)
		done = true
	}
	b, _ := json.Marshal(obj)
	return b
}

func defaultMarshaler() runtime.Marshaler {
	// The mux's own default, not a copy of its options: MarshalerForRequest on
	// a bare ServeMux with no Content-Type or Accept returns it.
	in, out := runtime.MarshalerForRequest(runtime.NewServeMux(), &http.Request{Header: http.Header{}})
	if in != out {
		fail("default inbound and outbound marshalers differ")
	}
	return in
}

func newMessage(files *protoregistry.Files, name string) *dynamicpb.Message {
	d, err := files.FindDescriptorByName(protoreflect.FullName(name))
	if err != nil {
		fail("message %s: %v", name, err)
	}
	return dynamicpb.NewMessage(d.(protoreflect.MessageDescriptor))
}

// decodeBody is what the generated handler does with a request body.
func decodeBody(m runtime.Marshaler, body []byte, msg proto.Message) error {
	err := m.NewDecoder(bytes.NewReader(body)).Decode(msg)
	if errors.Is(err, io.EOF) {
		return nil
	}
	return err
}

func compact(b []byte) string {
	var buf bytes.Buffer
	if err := json.Compact(&buf, b); err != nil {
		fail("compact: %v", err)
	}
	return buf.String()
}

func readJSONCases() []jsonCase {
	var c struct {
		Cases []jsonCase `json:"cases"`
	}
	if err := json.NewDecoder(os.Stdin).Decode(&c); err != nil {
		fail("decode json cases: %v", err)
	}
	return c.Cases
}

// runJSON writes, for each case, whether grpc-gateway's default marshaler
// accepts the body and what it answers for the decoded message.
func runJSON(path, source string) {
	files := loadContractTypes(path)
	m := defaultMarshaler()
	omit := &runtime.JSONPb{UnmarshalOptions: protojson.UnmarshalOptions{DiscardUnknown: true}}
	out := jsonVectors{
		Generator: os.Getenv("ABADA_GENERATOR"),
		Contract:  source,
		Marshaler: "runtime.ServeMux default: JSONPb{EmitUnpopulated: true, DiscardUnknown: true}",
	}
	for _, c := range readJSONCases() {
		// Correctness is checked on the input as written; Repeat only sizes
		// the benchmark, and would make the vectors large for nothing.
		v := jsonVector{jsonCase: c}
		msg := newMessage(files, c.Message)
		if err := decodeBody(m, c.Input, msg); err != nil {
			v.Error = err.Error()
		} else {
			v.Accepted = true
			b, err := m.Marshal(msg)
			if err != nil {
				fail("%s: marshal: %v", c.Name, err)
			}
			v.Output = compact(b)
			b, err = omit.Marshal(msg)
			if err != nil {
				fail("%s: marshal: %v", c.Name, err)
			}
			v.OutputOmitUnpopulated = compact(b)
		}
		out.Cases = append(out.Cases, v)
	}
	enc := json.NewEncoder(os.Stdout)
	enc.SetIndent("", "  ")
	enc.SetEscapeHTML(false)
	if err := enc.Encode(out); err != nil {
		fail("encode: %v", err)
	}
}

// runJSONBench measures grpc-gateway's default marshaler on the bench cases:
// decode of the body into the message, and marshal of the message. The
// messages are dynamicpb, not generated Go types, so this is a reference for
// the order of magnitude and not a like-for-like figure.
func runJSONBench(path string) {
	files := loadContractTypes(path)
	m := defaultMarshaler()
	for _, c := range readJSONCases() {
		if !c.Bench {
			continue
		}
		body := expandRepeat(c)
		msg := newMessage(files, c.Message)
		if err := decodeBody(m, body, msg); err != nil {
			fail("%s: %v", c.Name, err)
		}
		median := func(f func(b *testing.B)) (float64, int64) {
			var samples []float64
			var allocs int64
			for s := 0; s < 7; s++ {
				res := testing.Benchmark(f)
				samples = append(samples, float64(res.NsPerOp()))
				allocs = res.AllocsPerOp()
			}
			sort.Float64s(samples)
			return samples[3], allocs
		}
		dec, decAllocs := median(func(b *testing.B) {
			b.ReportAllocs()
			for i := 0; i < b.N; i++ {
				fresh := newMessage(files, c.Message)
				if err := decodeBody(m, body, fresh); err != nil {
					b.Fatal(err)
				}
			}
		})
		enc, encAllocs := median(func(b *testing.B) {
			b.ReportAllocs()
			for i := 0; i < b.N; i++ {
				if _, err := m.Marshal(msg); err != nil {
					b.Fatal(err)
				}
			}
		})
		fmt.Printf("grpc-gateway %-32s %6d bytes  decode %9.0f ns (%d allocs)  encode %9.0f ns (%d allocs)\n",
			c.Name, len(body), dec, decAllocs, enc, encAllocs)
	}
}
