package main

import (
	"encoding/json"
	"os"
	"sort"
	"strings"

	"google.golang.org/genproto/googleapis/api/annotations"
	"google.golang.org/protobuf/proto"
	"google.golang.org/protobuf/reflect/protodesc"
	"google.golang.org/protobuf/reflect/protoreflect"
	"google.golang.org/protobuf/types/descriptorpb"
)

// inventory reports what a contract asks of a gateway beyond routing: RPCs
// without a mapping, fields that can only arrive as query parameters, and the
// JSON features its messages use. It feeds docs, not tests.
type inventory struct {
	RPCs          int            `json:"rpcs"`
	Unmapped      []string       `json:"unmapped"`
	QueryFields   map[string]int `json:"query_fields_by_rpc"`
	JSONFeatures  map[string]int `json:"json_features"`
	WellKnown     map[string]int `json:"well_known_types"`
	ServerStreams []string       `json:"server_streaming"`
}

func runInventory(path string) {
	raw, err := os.ReadFile(path)
	if err != nil {
		fail("read: %v", err)
	}
	var set descriptorpb.FileDescriptorSet
	if err := proto.Unmarshal(raw, &set); err != nil {
		fail("decode: %v", err)
	}
	files, err := protodesc.NewFiles(&set)
	if err != nil {
		fail("resolve: %v", err)
	}
	inv := inventory{QueryFields: map[string]int{}, JSONFeatures: map[string]int{}, WellKnown: map[string]int{}}
	seen := map[protoreflect.FullName]bool{}
	var walk func(md protoreflect.MessageDescriptor)
	walk = func(md protoreflect.MessageDescriptor) {
		if md == nil || seen[md.FullName()] {
			return
		}
		seen[md.FullName()] = true
		if strings.HasPrefix(string(md.FullName()), "google.protobuf.") {
			inv.WellKnown[string(md.FullName())]++
			return
		}
		fs := md.Fields()
		for i := 0; i < fs.Len(); i++ {
			f := fs.Get(i)
			switch {
			case f.IsMap():
				inv.JSONFeatures["map"]++
			case f.IsList():
				inv.JSONFeatures["repeated"]++
			}
			if o := f.ContainingOneof(); o != nil && !o.IsSynthetic() {
				inv.JSONFeatures["oneof"]++
			}
			if f.HasOptionalKeyword() {
				inv.JSONFeatures["proto3 optional"]++
			}
			switch f.Kind() {
			case protoreflect.Int64Kind, protoreflect.Uint64Kind, protoreflect.Sint64Kind, protoreflect.Fixed64Kind, protoreflect.Sfixed64Kind:
				inv.JSONFeatures["64-bit integer (JSON string)"]++
			case protoreflect.BytesKind:
				inv.JSONFeatures["bytes (base64)"]++
			case protoreflect.EnumKind:
				inv.JSONFeatures["enum"]++
			case protoreflect.FloatKind, protoreflect.DoubleKind:
				inv.JSONFeatures["float/double"]++
			case protoreflect.MessageKind, protoreflect.GroupKind:
				if !f.IsMap() {
					walk(f.Message())
				} else if f.MapValue().Kind() == protoreflect.MessageKind {
					walk(f.MapValue().Message())
				}
			}
		}
	}
	for _, fd := range set.GetFile() {
		desc, _ := files.FindFileByPath(fd.GetName())
		for i := 0; i < desc.Services().Len(); i++ {
			svc := desc.Services().Get(i)
			for j := 0; j < svc.Methods().Len(); j++ {
				m := svc.Methods().Get(j)
				inv.RPCs++
				name := string(svc.Name()) + "." + string(m.Name())
				walk(m.Input())
				walk(m.Output())
				rule, _ := proto.GetExtension(m.Options(), annotations.E_Http).(*annotations.HttpRule)
				if rule == nil || rule.GetPattern() == nil {
					inv.Unmapped = append(inv.Unmapped, name)
					continue
				}
				if m.IsStreamingServer() {
					inv.ServerStreams = append(inv.ServerStreams, name)
				}
				if rule.GetBody() == "*" {
					continue
				}
				b := ruleBindings(svc, m, rule, 0)
				inPath := map[string]bool{}
				if c, err := parseFields(b.Template); err == nil {
					for _, f := range c {
						inPath[strings.Split(f, ".")[0]] = true
					}
				}
				n := 0
				in := m.Input().Fields()
				for k := 0; k < in.Len(); k++ {
					f := in.Get(k)
					if inPath[string(f.Name())] || string(f.Name()) == rule.GetBody() {
						continue
					}
					n++
				}
				if n > 0 {
					inv.QueryFields[name] = n
				}
			}
		}
	}
	sort.Strings(inv.Unmapped)
	enc := json.NewEncoder(os.Stdout)
	enc.SetIndent("", "  ")
	_ = enc.Encode(inv)
}
