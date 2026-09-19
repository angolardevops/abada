// Command protogen runs protoc plugins over a descriptor set without protoc:
// it builds the CodeGeneratorRequest protoc would send and writes the files the
// plugins answer. The e2e oracle compiles what protoc-gen-go,
// protoc-gen-go-grpc and protoc-gen-grpc-gateway generate, so the gateway it
// measures is the code a grpc-gateway user runs, not a re-creation of it.
//
//	protogen -set x.binpb -importpath <go import path> -module <go module> \
//	  -out <module root> -plugin <path>[:<parameter>]...
//
// Every file of the set outside google/ is generated into the one Go package
// named by -importpath.
package main

import (
	"bytes"
	"flag"
	"fmt"
	"os"
	"os/exec"
	"path/filepath"
	"strings"

	"google.golang.org/protobuf/proto"
	"google.golang.org/protobuf/types/descriptorpb"
	"google.golang.org/protobuf/types/pluginpb"
)

type plugins []string

func (p *plugins) String() string     { return strings.Join(*p, ",") }
func (p *plugins) Set(v string) error { *p = append(*p, v); return nil }

func main() {
	set := flag.String("set", "", "descriptor set (protoc --include_imports -o)")
	importPath := flag.String("importpath", "", "Go import path of the generated package")
	module := flag.String("module", "", "Go module the output root belongs to")
	out := flag.String("out", "", "module root to write into")
	var ps plugins
	flag.Var(&ps, "plugin", "plugin binary, optionally :parameter")
	flag.Parse()

	raw, err := os.ReadFile(*set)
	if err != nil {
		fail("%v", err)
	}
	var fds descriptorpb.FileDescriptorSet
	if err := proto.Unmarshal(raw, &fds); err != nil {
		fail("%s: %v", *set, err)
	}
	var generate, mappings []string
	for _, f := range fds.GetFile() {
		if strings.HasPrefix(f.GetName(), "google/") {
			continue
		}
		generate = append(generate, f.GetName())
		mappings = append(mappings, "M"+f.GetName()+"="+*importPath)
	}
	if len(generate) == 0 {
		fail("%s: nothing to generate", *set)
	}
	for _, p := range ps {
		bin, param, _ := strings.Cut(p, ":")
		params := append([]string{"module=" + *module}, mappings...)
		if param != "" {
			params = append(params, param)
		}
		req := &pluginpb.CodeGeneratorRequest{
			FileToGenerate: generate,
			Parameter:      proto.String(strings.Join(params, ",")),
			ProtoFile:      fds.GetFile(),
		}
		in, err := proto.Marshal(req)
		if err != nil {
			fail("%v", err)
		}
		cmd := exec.Command(bin)
		cmd.Stdin = bytes.NewReader(in)
		cmd.Stderr = os.Stderr
		answer, err := cmd.Output()
		if err != nil {
			fail("%s: %v", bin, err)
		}
		var resp pluginpb.CodeGeneratorResponse
		if err := proto.Unmarshal(answer, &resp); err != nil {
			fail("%s: %v", bin, err)
		}
		if resp.Error != nil {
			fail("%s: %s", bin, resp.GetError())
		}
		for _, f := range resp.GetFile() {
			path := filepath.Join(*out, filepath.FromSlash(f.GetName()))
			if err := os.MkdirAll(filepath.Dir(path), 0o755); err != nil {
				fail("%v", err)
			}
			if err := os.WriteFile(path, []byte(f.GetContent()), 0o644); err != nil {
				fail("%v", err)
			}
		}
	}
}

func fail(f string, a ...any) {
	fmt.Fprintf(os.Stderr, "protogen: "+f+"\n", a...)
	os.Exit(1)
}
