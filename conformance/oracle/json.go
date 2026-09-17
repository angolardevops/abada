package main

import (
	"bytes"
	"encoding/hex"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"net/http"
	"os"
	"reflect"
	"sort"
	"strings"
	"testing"

	_ "github.com/grpc-ecosystem/grpc-gateway/v2/internal/abadaoracle/abadapb"

	"github.com/grpc-ecosystem/grpc-gateway/v2/runtime"
	"google.golang.org/protobuf/encoding/protojson"
	"google.golang.org/protobuf/encoding/prototext"
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
	Input   json.RawMessage `json:"input,omitempty"`
	// Body, when set, is the request body as text, for bodies that are not one
	// JSON value (empty, trailing bytes); BodyHex for bytes that are not UTF-8.
	Body    *string `json:"body,omitempty"`
	BodyHex string  `json:"body_hex,omitempty"`
	// Text builds the message from prototext instead of decoding a body, for
	// messages no JSON body can produce (NaN, out-of-range well-known types, a
	// Value with no kind); ProtoHex from binary. Only the output is recorded.
	Text     *string `json:"text,omitempty"`
	ProtoHex string  `json:"proto_hex,omitempty"`
	// Field is a `body: "<field>"` binding: the body is decoded into that
	// field of the Go request struct, as the generated handler does, after
	// Prefill (prototext) was put in the message.
	Field   string `json:"field,omitempty"`
	Prefill string `json:"prefill,omitempty"`
	// ResponseField is a `response_body: "<field>"` binding: the output is
	// the marshaled field of the message built by Text or Input.
	ResponseField string `json:"response_field,omitempty"`
	// Repeat, when set, cycles the elements of the input's only array field up
	// to that many: a list response of realistic size without writing it out.
	Repeat int  `json:"repeat,omitempty"`
	Bench  bool `json:"bench,omitempty"`
}

type jsonVector struct {
	jsonCase
	// Accepted is whether the gateway's default inbound marshaler decodes the
	// input into the message, the way a generated handler decodes a body.
	// Always true for Text and ProtoHex cases.
	Accepted bool   `json:"accepted"`
	Error    string `json:"error,omitempty"`
	// Output is what the default outbound marshaler (EmitUnpopulated: true)
	// writes for the decoded message, whitespace removed. protojson randomises
	// whitespace on purpose, so only the compacted form is a fixed expectation.
	Output string `json:"output,omitempty"`
	// OutputError is set when the marshaler fails; the text is for reading.
	OutputError string `json:"output_error,omitempty"`
	// OutputOmitUnpopulated is the same message with EmitUnpopulated: false,
	// the most common override of the default.
	OutputOmitUnpopulated string `json:"output_omit_unpopulated,omitempty"`
	OutputOmitError       string `json:"output_omit_error,omitempty"`
	// Proto is the decoded message in deterministic binary form (map entries
	// sorted), so a decoder can be checked without trusting its own encoder.
	// Absent when the message is empty.
	Proto []byte `json:"proto,omitempty"`
	// PrefillProto is the message a Field case decodes into, in binary.
	PrefillProto []byte `json:"prefill_proto,omitempty"`
}

type jsonVectors struct {
	Generator string       `json:"generator"`
	Contract  string       `json:"contract"`
	Marshaler string       `json:"marshaler"`
	Cases     []jsonVector `json:"cases"`
}

// loadContractTypes registers every message of the descriptor set that the
// binary does not already link (the google.protobuf ones and the conformance
// protos it does) in the global registry. The gateway's default marshaler
// resolves Any through that registry, exactly as it does for generated Go
// types. A linked file must be the one in the descriptor set.
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
		if linked, err := protoregistry.GlobalFiles.FindFileByPath(f.Path()); err == nil &&
			strings.HasPrefix(f.Path(), "abada/") {
			if !proto.Equal(protodesc.ToFileDescriptorProto(linked), protodesc.ToFileDescriptorProto(f)) {
				fail("%s: the linked Go types differ from %s; run conformance/protos/generate.sh", f.Path(), path)
			}
		}
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

// newMessage makes an empty message through the global registry: the
// generated Go type when the binary links one, dynamicpb otherwise.
func newMessage(name string) proto.Message {
	mt, err := protoregistry.GlobalTypes.FindMessageByName(protoreflect.FullName(name))
	if err != nil {
		fail("message %s: %v", name, err)
	}
	return mt.New().Interface()
}

// decodeBody is what the generated handler does with a request body.
func decodeBody(m runtime.Marshaler, body []byte, v any) error {
	err := m.NewDecoder(bytes.NewReader(body)).Decode(v)
	if errors.Is(err, io.EOF) {
		return nil
	}
	return err
}

// structField is the field of a generated Go message with the given proto
// name: what `&protoReq.<Field>` and `response.<Field>` name in generated code.
func structField(msg proto.Message, name, caseName string) reflect.Value {
	rv := reflect.ValueOf(msg)
	if rv.Kind() != reflect.Ptr || rv.Elem().Kind() != reflect.Struct {
		fail("%s: field cases need a generated Go type, %T is not one", caseName, msg)
	}
	st := rv.Elem()
	for i := 0; i < st.NumField(); i++ {
		for _, part := range strings.Split(st.Type().Field(i).Tag.Get("protobuf"), ",") {
			if part == "name="+name {
				return st.Field(i)
			}
		}
	}
	fail("%s: no Go field for %q (oneof members are not supported)", caseName, name)
	return reflect.Value{}
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

// errorText normalises what protojson randomises in its errors: a space or a
// non-breaking space, chosen per binary, so that nobody matches on them.
func errorText(err error) string {
	return strings.ReplaceAll(err.Error(), "\u00a0", " ")
}

// caseBody is the request body of a case, exactly as abada's tests will read
// it back from the vectors: a JSON input is compacted when the vectors are
// written, so it is compacted here too.
func caseBody(c jsonCase) []byte {
	switch {
	case c.BodyHex != "":
		b, err := hex.DecodeString(c.BodyHex)
		if err != nil {
			fail("%s: body_hex: %v", c.Name, err)
		}
		return b
	case c.Body != nil:
		return []byte(*c.Body)
	}
	return []byte(compact(c.Input))
}

// runJSON writes, for each case, whether grpc-gateway's default marshaler
// accepts the body and what it answers for the decoded message.
func runJSON(path, source string) {
	loadContractTypes(path)
	m := defaultMarshaler()
	omit := &runtime.JSONPb{UnmarshalOptions: protojson.UnmarshalOptions{DiscardUnknown: true}}
	out := jsonVectors{
		Generator: os.Getenv("ABADA_GENERATOR"),
		Contract:  source,
		Marshaler: "runtime.ServeMux default: JSONPb{EmitUnpopulated: true, DiscardUnknown: true}",
	}
	deterministic := proto.MarshalOptions{Deterministic: true, AllowPartial: true}
	for _, c := range readJSONCases() {
		// Correctness is checked on the input as written; Repeat only sizes
		// the benchmark, and would make the vectors large for nothing.
		v := jsonVector{jsonCase: c}
		msg := newMessage(c.Message)
		var err error
		switch {
		case c.Text != nil:
			if err := (prototext.UnmarshalOptions{AllowPartial: true}).Unmarshal([]byte(*c.Text), msg); err != nil {
				fail("%s: text: %v", c.Name, err)
			}
		case c.ProtoHex != "":
			b, herr := hex.DecodeString(c.ProtoHex)
			if herr != nil {
				fail("%s: proto_hex: %v", c.Name, herr)
			}
			if err := (proto.UnmarshalOptions{AllowPartial: true}).Unmarshal(b, msg); err != nil {
				fail("%s: proto_hex: %v", c.Name, err)
			}
		case c.Field != "":
			// decodeNonProtoField walks Go maps: a case whose answer depends on
			// map order (two JSON keys naming the same map key) cannot be a
			// fixed expectation, so it is refused rather than recorded.
			var first []byte
			for run := 0; run < 16; run++ {
				msg = newMessage(c.Message)
				if err := (prototext.UnmarshalOptions{AllowPartial: true}).Unmarshal([]byte(c.Prefill), msg); err != nil {
					fail("%s: prefill: %v", c.Name, err)
				}
				if v.PrefillProto, err = deterministic.Marshal(msg); err != nil {
					fail("%s: prefill binary: %v", c.Name, err)
				}
				err = decodeBody(m, caseBody(c), structField(msg, c.Field, c.Name).Addr().Interface())
				b, _ := deterministic.Marshal(msg)
				if err != nil {
					b = []byte("error")
				}
				if run == 0 {
					first = b
				} else if !bytes.Equal(first, b) {
					fail("%s: the answer depends on Go map order; change the case", c.Name)
				}
			}
		default:
			err = decodeBody(m, caseBody(c), msg)
		}
		if err != nil {
			v.Error = errorText(err)
			out.Cases = append(out.Cases, v)
			continue
		}
		v.Accepted = true
		var subject any = msg
		if c.ResponseField != "" {
			subject = structField(msg, c.ResponseField, c.Name).Interface()
		}
		if b, err := m.Marshal(subject); err != nil {
			v.OutputError = errorText(err)
		} else {
			v.Output = compact(b)
		}
		if b, err := omit.Marshal(subject); err != nil {
			v.OutputOmitError = errorText(err)
		} else {
			v.OutputOmitUnpopulated = compact(b)
		}
		if v.Proto, err = deterministic.Marshal(msg); err != nil {
			fail("%s: binary: %v", c.Name, err)
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
	loadContractTypes(path)
	m := defaultMarshaler()
	for _, c := range readJSONCases() {
		if !c.Bench {
			continue
		}
		body := expandRepeat(c)
		msg := newMessage(c.Message)
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
				fresh := newMessage(c.Message)
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
