package main

import (
	"context"
	"flag"
	"fmt"
	"io"
	"net/http"
	"os"

	abadaconformancerequestv1 "github.com/grpc-ecosystem/grpc-gateway/v2/internal/abadaparity/gen/abadaconformancerequestv1"
	"github.com/grpc-ecosystem/grpc-gateway/v2/runtime"
	"google.golang.org/grpc"
	"google.golang.org/grpc/credentials/insecure"
	"google.golang.org/grpc/grpclog"
)

// runGateway serves the code protoc-gen-grpc-gateway generates for the
// contract, on a real ServeMux, in front of a real grpc.ClientConn. Default
// options everywhere: it is what a user who followed the README runs.
func runGateway(args []string) {
	fs := flag.NewFlagSet("gateway", flag.ExitOnError)
	listen := fs.String("listen", "127.0.0.1:8080", "address")
	backend := fs.String("backend", "127.0.0.1:9000", "gRPC backend")
	_ = fs.Parse(args)
	grpclog.SetLoggerV2(grpclog.NewLoggerV2(io.Discard, io.Discard, io.Discard))

	conn, err := grpc.NewClient(*backend, grpc.WithTransportCredentials(insecure.NewCredentials()))
	must(err)
	mux := runtime.NewServeMux()
	ctx := context.Background()
	must(abadaconformancerequestv1.RegisterPathServiceHandler(ctx, mux, conn))
	must(abadaconformancerequestv1.RegisterQueryServiceHandler(ctx, mux, conn))
	must(abadaconformancerequestv1.RegisterBodyServiceHandler(ctx, mux, conn))
	fmt.Fprintln(os.Stderr, "grpc-gateway listening on", *listen)
	must(http.ListenAndServe(*listen, mux))
}
