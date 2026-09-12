package sdk

import (
	"context"
	"fmt"
	"io"
	"net"
	"os"
	"os/signal"
	"path/filepath"
	"sync"
	"syscall"

	pb "github.com/ZanattaMichael/meridian-core/pkg/sdk/pluginpb"
	"google.golang.org/grpc"
)

// Serve runs an emitter as a plugin process and blocks until the host is done
// with it. A plugin's main is expected to be one call to this function.
//
// The lifecycle is deliberately host-driven. The plugin listens first, then
// announces where it is listening on a single stdout line, then serves. It exits
// when the host closes its stdin or signals it, so an orphaned plugin cannot
// outlive the run that started it — a plugin left listening after its host died
// is a leak an operator has no obvious way to find.
func Serve(e Emitter) error {
	return serve(e, os.Stdout, os.Stdin)
}

func serve(e Emitter, announce io.Writer, done io.Reader) error {
	if e == nil {
		return fmt.Errorf("cannot serve a nil emitter")
	}

	lis, cleanup, err := listen()
	if err != nil {
		return err
	}
	defer cleanup()

	srv := grpc.NewServer()
	pb.RegisterEmitterServer(srv, &server{emitter: e})

	// The handshake is written only once the listener is up, so a host that has
	// read the line can always dial it.
	if _, err := fmt.Fprintf(announce, "%s|%d|%s|%s|%s\n",
		HandshakeMagic, ProtocolVersion, Version, lis.Addr().Network(), lis.Addr().String()); err != nil {
		return fmt.Errorf("writing the plugin handshake: %w", err)
	}
	if f, ok := announce.(*os.File); ok {
		_ = f.Sync()
	}

	var once sync.Once
	stop := func() { once.Do(srv.GracefulStop) }

	signals := make(chan os.Signal, 1)
	signal.Notify(signals, os.Interrupt, syscall.SIGTERM)
	defer signal.Stop(signals)
	go func() {
		<-signals
		stop()
	}()

	if done != nil {
		go func() {
			// Any read returning is the host letting go: either it closed the
			// pipe or it exited and the kernel closed it for us.
			_, _ = io.Copy(io.Discard, done)
			stop()
		}()
	}

	return srv.Serve(lis)
}

// listen prefers a unix socket in a private directory, which no other user can
// connect to, and falls back to a loopback port where unix sockets are not
// available. The fallback is a real weakening — any local process can dial a
// loopback port — so it is last resort rather than the default.
func listen() (net.Listener, func(), error) {
	dir, err := os.MkdirTemp("", "meridian-plugin-")
	if err == nil {
		if err := os.Chmod(dir, 0o700); err == nil {
			path := filepath.Join(dir, "plugin.sock")
			if lis, err := net.Listen("unix", path); err == nil {
				return lis, func() { os.RemoveAll(dir) }, nil
			}
		}
		os.RemoveAll(dir)
	}

	lis, err := net.Listen("tcp", "127.0.0.1:0")
	if err != nil {
		return nil, nil, fmt.Errorf("a plugin could not listen on a unix socket or on loopback: %w", err)
	}
	return lis, func() {}, nil
}

// server adapts the Go-native Emitter interface onto the generated service.
type server struct {
	pb.UnimplementedEmitterServer
	emitter Emitter
}

func (s *server) Describe(_ context.Context, _ *pb.DescribeRequest) (*pb.DescribeResponse, error) {
	return &pb.DescribeResponse{
		ProtocolVersion: ProtocolVersion,
		SdkVersion:      Version,
		Name:            s.emitter.Name(),
		ResourceTypes:   s.emitter.SupportedResourceTypes(),
		Capabilities:    capabilitiesToWire(s.emitter.Capabilities()),
	}, nil
}

func (s *server) Emit(_ context.Context, req *pb.EmitRequest) (*pb.EmitResponse, error) {
	g, err := GraphFromWire(req.GetGraph())
	if err != nil {
		// A malformed request is a protocol fault, not a target refusing to
		// compile, so it travels as a status error.
		return nil, fmt.Errorf("reading the resource graph: %w", err)
	}
	data, err := decodeValues(req.GetDataJson())
	if err != nil {
		return nil, fmt.Errorf("reading the resolved data: %w", err)
	}

	artifact, warnings, err := s.emitter.Emit(g, Data(data))
	if err != nil {
		return &pb.EmitResponse{Error: compileErrorToWire(err)}, nil
	}
	return &pb.EmitResponse{
		Artifact: artifactToWire(artifact),
		Warnings: warningsToWire(warnings),
	}, nil
}
