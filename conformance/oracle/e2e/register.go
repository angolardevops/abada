package main

import (
	"context"

	abadaconformancev1 "github.com/grpc-ecosystem/grpc-gateway/v2/internal/abadaoracle/e2e/gen/abadaconformancev1"
	delonixnodev1 "github.com/grpc-ecosystem/grpc-gateway/v2/internal/abadaoracle/e2e/gen/delonixnodev1"
	"github.com/grpc-ecosystem/grpc-gateway/v2/runtime"
	"google.golang.org/grpc"
)

type registerFunc func(context.Context, *runtime.ServeMux, *grpc.ClientConn) error

// registry maps each service with HTTP rules to its generated registration.
// main refuses a descriptor set with a service missing here.
var registry = map[string]registerFunc{
	"delonix.node.v1.OperationService":      delonixnodev1.RegisterOperationServiceHandler,
	"delonix.node.v1.ContainerService":      delonixnodev1.RegisterContainerServiceHandler,
	"delonix.node.v1.PodService":            delonixnodev1.RegisterPodServiceHandler,
	"delonix.node.v1.VirtualMachineService": delonixnodev1.RegisterVirtualMachineServiceHandler,
	"delonix.node.v1.NetworkService":        delonixnodev1.RegisterNetworkServiceHandler,
	"delonix.node.v1.VolumeService":         delonixnodev1.RegisterVolumeServiceHandler,
	"delonix.node.v1.ImageService":          delonixnodev1.RegisterImageServiceHandler,
	"delonix.node.v1.StackService":          delonixnodev1.RegisterStackServiceHandler,
	"delonix.node.v1.NodeService":           delonixnodev1.RegisterNodeServiceHandler,
	"abada.conformance.v1.PathService":      abadaconformancev1.RegisterPathServiceHandler,
	"abada.conformance.v1.QueryService":     abadaconformancev1.RegisterQueryServiceHandler,
	"abada.conformance.v1.BodyService":      abadaconformancev1.RegisterBodyServiceHandler,
}
