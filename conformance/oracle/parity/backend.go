package main

import (
	"flag"
	"fmt"
	"io"
	"net"
	"os"

	"google.golang.org/grpc"
	"google.golang.org/grpc/codes"

	"google.golang.org/grpc/encoding"
	"google.golang.org/grpc/grpclog"
	"google.golang.org/grpc/metadata"
	"google.golang.org/grpc/status"
	"google.golang.org/protobuf/encoding/protojson"
	"google.golang.org/protobuf/proto"
	"google.golang.org/protobuf/reflect/protodesc"
	"google.golang.org/protobuf/reflect/protoreflect"
	"google.golang.org/protobuf/types/descriptorpb"
	"google.golang.org/protobuf/types/dynamicpb"
)

// rawCodec hands the backend the request bytes untouched and lets it answer
// with bytes it prepared at start: no per-request decoding on this side.
type rawCodec struct{}

func (rawCodec) Marshal(v any) ([]byte, error) { return *(v.(*[]byte)), nil }
func (rawCodec) Unmarshal(data []byte, v any) error {
	*(v.(*[]byte)) = data
	return nil
}
func (rawCodec) Name() string { return "proto" }

var _ encoding.Codec = rawCodec{}

// responseJSON is what `Response` answers: 64-bit integers, a map, a
// repeated field and a Timestamp inside `nested`, the field the rule selects.
const responseJSON = `{"nested":{"fInt64":"9007199254740993","fUint64":"18446744073709551615",
"fString":"hello from the backend","rString":["alpha","beta","gamma","delta"],
"mStr":{"k1":"v1","k2":"v2","k3":"v3"},"mInt":{"1":10,"2":20},
"fTimestamp":"2026-09-19T10:00:00.123Z","fDuration":"3.5s","fEnum":"GREEN",
"rInt32":[1,2,3,4,5,6,7,8]}}`

func runBackend(args []string) {
	fs := flag.NewFlagSet("backend", flag.ExitOnError)
	listen := fs.String("listen", "127.0.0.1:9000", "address")
	set := fs.String("set", "", "descriptor set of the request contract")
	_ = fs.Parse(args)
	grpclog.SetLoggerV2(grpclog.NewLoggerV2(io.Discard, io.Discard, io.Discard))

	raw, err := os.ReadFile(*set)
	must(err)
	var fds descriptorpb.FileDescriptorSet
	must(proto.Unmarshal(raw, &fds))
	files, err := protodesc.NewFiles(&fds)
	must(err)
	d, err := files.FindDescriptorByName("abada.conformance.request.v1.Everything")
	must(err)
	msg := dynamicpb.NewMessage(d.(protoreflect.MessageDescriptor))
	must(protojson.Unmarshal([]byte(responseJSON), msg))
	everything, err := proto.MarshalOptions{Deterministic: true}.Marshal(msg)
	must(err)
	empty := []byte{}

	lis, err := net.Listen("tcp", *listen)
	must(err)
	srv := grpc.NewServer(
		grpc.ForceServerCodec(rawCodec{}),
		grpc.UnknownServiceHandler(func(_ any, stream grpc.ServerStream) error {
			var in []byte
			if err := stream.RecvMsg(&in); err != nil {
				return err
			}
			if md, ok := metadata.FromIncomingContext(stream.Context()); ok && len(md.Get("x-fail")) > 0 {
				return status.Error(codes.NotFound, "the resource does not exist")
			}
			rpc, _ := grpc.MethodFromServerStream(stream)
			out := &empty
			if rpc == "/abada.conformance.request.v1.BodyService/Response" {
				out = &everything
			}
			return stream.SendMsg(out)
		}),
	)
	fmt.Fprintln(os.Stderr, "backend listening on", *listen)
	must(srv.Serve(lis))
}

func must(err error) {
	if err != nil {
		fmt.Fprintln(os.Stderr, "parity:", err)
		os.Exit(1)
	}
}
