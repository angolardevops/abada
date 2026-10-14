// Command parity is the reference side of the abada performance gate
// (docs: .claude/skills/abada-performance). One binary, four roles, so the
// same Go toolchain and the same client library measure both gateways:
//
//	parity backend  -listen :9000 -set request.binpb
//	    a gRPC server with a fixed, in-memory answer per RPC. Both gateways
//	    call it. It does no work of its own.
//	parity gateway  -listen :8080 -backend 127.0.0.1:9000
//	    grpc-gateway v2.27.3's generated handlers on a runtime.ServeMux over a
//	    real net/http server and a real grpc.ClientConn.
//	parity loadgen  ...            (loadgen.go)
//	    the load generator both gateways are measured with, out of process.
//	parity check    ...            prints what a gateway answers for the mix.
package main

import (
	"fmt"
	"os"
)

func main() {
	if len(os.Args) < 2 {
		fmt.Fprintln(os.Stderr, "usage: parity backend|gateway|loadgen|check|ready [flags]")
		os.Exit(2)
	}
	role, args := os.Args[1], os.Args[2:]
	switch role {
	case "backend":
		runBackend(args)
	case "gateway":
		runGateway(args)
	case "loadgen":
		runLoadgen(args)
	case "check":
		runCheck(args)
	case "ready":
		runReady(args)
	default:
		fmt.Fprintf(os.Stderr, "parity: unknown role %q\n", role)
		os.Exit(2)
	}
}
