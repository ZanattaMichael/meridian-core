package plugin

import (
	"bufio"
	"context"
	"errors"
	"fmt"
	"io"
	"net"
	"os"
	"os/exec"
	"strconv"
	"strings"
	"sync"
	"time"

	"github.com/ZanattaMichael/meridian-core/pkg/sdk"
	pb "github.com/ZanattaMichael/meridian-core/pkg/sdk/pluginpb"
	"google.golang.org/grpc"
	"google.golang.org/grpc/credentials/insecure"
)

// HandshakeTimeout bounds how long a host waits for a plugin to announce
// itself. A plugin that has not printed its handshake line by then is either
// not a Meridian plugin or is wedged; both are better reported than waited on.
const HandshakeTimeout = 10 * time.Second

// stderrLimit caps how much of a plugin's stderr is retained for diagnostics. A
// plugin in a crash loop can produce a great deal of it, and an unbounded
// buffer in the host would be a denial of service by a misbehaving target.
const stderrLimit = 64 << 10

// Client is a running plugin process, presented as an ordinary emitter.
//
// It satisfies sdk.Emitter, which is the point of the whole boundary: code that
// compiles a document cannot tell a subprocess target from a compiled-in one.
type Client struct {
	cmd    *exec.Cmd
	conn   *grpc.ClientConn
	api    pb.EmitterClient
	info   Info
	stderr *tail
	// stderrDone closes once every byte the plugin wrote has been collected.
	stderrDone <-chan struct{}
	// shutdown closes the plugin's stdin, which is how the host says it is
	// done; the plugin exits when that read returns.
	shutdown func() error

	closeOnce sync.Once
	closeErr  error
}

// StartOption adjusts how a plugin is launched.
type StartOption func(*startConfig)

type startConfig struct {
	args    []string
	env     []string
	timeout time.Duration
}

// WithArgs passes arguments to the plugin binary.
func WithArgs(args ...string) StartOption {
	return func(c *startConfig) { c.args = append(c.args, args...) }
}

// WithEnv sets the plugin's environment, replacing the host's.
func WithEnv(env ...string) StartOption {
	return func(c *startConfig) { c.env = append(c.env, env...) }
}

// WithHandshakeTimeout overrides how long to wait for the handshake line.
func WithHandshakeTimeout(d time.Duration) StartOption {
	return func(c *startConfig) { c.timeout = d }
}

// Start launches a plugin binary and completes the handshake.
//
// On any failure the process is killed before returning, so a half-started
// plugin never survives the error that describes it.
func Start(ctx context.Context, path string, opts ...StartOption) (*Client, error) {
	cfg := startConfig{timeout: HandshakeTimeout}
	for _, opt := range opts {
		opt(&cfg)
	}

	cmd := exec.Command(path, cfg.args...)
	if cfg.env != nil {
		cmd.Env = cfg.env
	}

	stdout, err := cmd.StdoutPipe()
	if err != nil {
		return nil, &Error{Path: path, Msg: "could not read the plugin's stdout", Err: err}
	}
	// Holding the write end of the plugin's stdin is how the host says "still
	// here": closing it on shutdown is what tells the plugin to exit.
	stdin, err := cmd.StdinPipe()
	if err != nil {
		return nil, &Error{Path: path, Msg: "could not open the plugin's stdin", Err: err}
	}
	errTail := &tail{limit: stderrLimit}
	stderr, err := cmd.StderrPipe()
	if err != nil {
		return nil, &Error{Path: path, Msg: "could not read the plugin's stderr", Err: err}
	}

	if err := cmd.Start(); err != nil {
		return nil, &Error{Path: path, Msg: "could not start the plugin", Err: err}
	}
	// The copy runs to completion before any error is rendered: cmd.Wait closes
	// the pipe, so reading Stderr without waiting for the copy would report a
	// truncated explanation, or none, depending on scheduling.
	errDone := make(chan struct{})
	go func() { defer close(errDone); _, _ = io.Copy(errTail, stderr) }()

	fail := func(msg string, err error) (*Client, error) {
		_ = stdin.Close()
		_ = cmd.Process.Kill()
		_ = cmd.Wait()
		drain(errDone)
		return nil, &Error{Path: path, Msg: msg, Err: err, Stderr: errTail.String()}
	}

	network, address, handshake, err := readHandshake(ctx, stdout, cfg.timeout)
	if err != nil {
		return fail("handshake failed", err)
	}
	if handshake.ProtocolVersion != sdk.ProtocolVersion {
		return fail("protocol mismatch", fmt.Errorf(
			"the plugin speaks protocol version %d and this host implements %d; rebuild the plugin against a matching SDK",
			handshake.ProtocolVersion, sdk.ProtocolVersion))
	}

	conn, err := grpc.NewClient("passthrough:///"+address,
		grpc.WithTransportCredentials(insecure.NewCredentials()),
		grpc.WithContextDialer(dialer(network)))
	if err != nil {
		return fail("could not connect to the plugin", err)
	}

	c := &Client{cmd: cmd, conn: conn, api: pb.NewEmitterClient(conn), stderr: errTail, stderrDone: errDone}
	// Closing stdin is the shutdown signal, so the pipe is owned by Close.
	c.shutdown = func() error { return stdin.Close() }

	described, err := c.describe(ctx)
	if err != nil {
		_ = c.Close()
		drain(errDone)
		return nil, &Error{Path: path, Msg: "the plugin did not describe itself", Err: err, Stderr: errTail.String()}
	}
	described.Path = path
	c.info = described
	return c, nil
}

// handshake is the parsed form of the plugin's announcement line.
type handshakeLine struct {
	ProtocolVersion uint32
	SDKVersion      string
}

// readHandshake reads the single line a plugin prints once it is listening.
//
// The read runs in a goroutine so a plugin that prints nothing is bounded by
// the timeout rather than hanging the host forever.
func readHandshake(ctx context.Context, stdout io.Reader, timeout time.Duration) (network, address string, h handshakeLine, err error) {
	type result struct {
		line string
		err  error
	}
	lines := make(chan result, 1)
	go func() {
		line, err := bufio.NewReader(stdout).ReadString('\n')
		lines <- result{line: line, err: err}
	}()

	select {
	case <-ctx.Done():
		return "", "", h, ctx.Err()
	case <-time.After(timeout):
		return "", "", h, fmt.Errorf("no handshake line within %s", timeout)
	case r := <-lines:
		if r.err != nil && r.line == "" {
			return "", "", h, fmt.Errorf("the plugin exited before announcing itself: %w", r.err)
		}
		return parseHandshake(strings.TrimRight(r.line, "\r\n"))
	}
}

// parseHandshake reads MAGIC|protocol|sdk|network|address.
func parseHandshake(line string) (network, address string, h handshakeLine, err error) {
	parts := strings.Split(line, "|")
	if len(parts) != 5 || parts[0] != sdk.HandshakeMagic {
		return "", "", h, fmt.Errorf("the first line of output was %q, which is not a Meridian plugin handshake", line)
	}
	version, err := strconv.ParseUint(parts[1], 10, 32)
	if err != nil {
		return "", "", h, fmt.Errorf("the handshake reported protocol version %q, which is not a number", parts[1])
	}
	if parts[4] == "" {
		return "", "", h, fmt.Errorf("the handshake reported no address to dial")
	}
	return parts[3], parts[4], handshakeLine{ProtocolVersion: uint32(version), SDKVersion: parts[2]}, nil
}

// drain waits for the stderr collector to finish, bounded so a plugin that
// leaves the pipe open in a child process cannot wedge the host's error path.
func drain(done <-chan struct{}) {
	select {
	case <-done:
	case <-time.After(time.Second):
	}
}

// collectedStderr reports what the plugin wrote. A plugin that failed mid-call
// is usually still exiting, so this waits briefly for its last words.
func (c *Client) collectedStderr() string {
	if c.stderrDone != nil {
		drain(c.stderrDone)
	}
	return c.stderr.String()
}

func dialer(network string) func(context.Context, string) (net.Conn, error) {
	return func(ctx context.Context, address string) (net.Conn, error) {
		var d net.Dialer
		return d.DialContext(ctx, network, address)
	}
}

func (c *Client) describe(ctx context.Context) (Info, error) {
	resp, err := c.api.Describe(ctx, &pb.DescribeRequest{})
	if err != nil {
		return Info{}, err
	}
	if resp.GetName() == "" {
		return Info{}, fmt.Errorf("the plugin reported no target name")
	}
	return Info{
		ProtocolVersion: resp.GetProtocolVersion(),
		SDKVersion:      resp.GetSdkVersion(),
		Name:            resp.GetName(),
		ResourceTypes:   resp.GetResourceTypes(),
		Capabilities:    capabilities(resp.GetCapabilities()),
	}, nil
}

func capabilities(c *pb.Capabilities) sdk.Capabilities {
	return sdk.Capabilities{
		NativeNotify:     c.GetNativeNotify(),
		SyntheticNotify:  c.GetSyntheticNotify(),
		RuntimeCondition: c.GetRuntimeCondition(),
	}
}

// Info returns what the plugin reported at handshake time.
func (c *Client) Info() Info { return c.info }

// Name returns the target name documents select with spec.target.
func (c *Client) Name() string { return c.info.Name }

// SupportedResourceTypes lists every IR resource type the target maps.
func (c *Client) SupportedResourceTypes() []string {
	return append([]string(nil), c.info.ResourceTypes...)
}

// Capabilities returns what the plugin declared it can express.
func (c *Client) Capabilities() sdk.Capabilities { return c.info.Capabilities }

// Emit compiles a tree in the plugin process.
func (c *Client) Emit(g *sdk.ResourceGraph, data sdk.Data) (sdk.Artifact, []sdk.Warning, error) {
	return c.EmitContext(context.Background(), g, data)
}

// EmitContext is Emit with a caller-controlled deadline.
func (c *Client) EmitContext(ctx context.Context, g *sdk.ResourceGraph, data sdk.Data) (sdk.Artifact, []sdk.Warning, error) {
	req, err := sdk.EmitRequest(g, data)
	if err != nil {
		return sdk.Artifact{}, nil, &Error{Plugin: c.info.Name, Path: c.info.Path, Msg: "could not encode the request", Err: err}
	}
	resp, err := c.api.Emit(ctx, req)
	if err != nil {
		return sdk.Artifact{}, nil, &Error{
			Plugin: c.info.Name, Path: c.info.Path,
			Msg: "the plugin failed while compiling", Err: err, Stderr: c.collectedStderr(),
		}
	}
	return sdk.EmitResponse(resp, c.info.Name)
}

// Close shuts the plugin down and waits for it to exit.
func (c *Client) Close() error {
	c.closeOnce.Do(func() {
		if c.conn != nil {
			_ = c.conn.Close()
		}
		if c.shutdown != nil {
			_ = c.shutdown()
		}
		c.closeErr = c.wait()
	})
	return c.closeErr
}

// wait gives the plugin a moment to exit on its own before killing it, so a
// clean shutdown stays clean and a stuck one still terminates.
func (c *Client) wait() error {
	done := make(chan error, 1)
	go func() { done <- c.cmd.Wait() }()
	select {
	case err := <-done:
		return exitError(err)
	case <-time.After(5 * time.Second):
		_ = c.cmd.Process.Kill()
		<-done
		return &Error{Plugin: c.info.Name, Path: c.info.Path, Msg: "did not exit when asked and was killed"}
	}
}

// exitError ignores the exit status of a plugin that was told to stop: being
// terminated on request is not a failure to report.
func exitError(err error) error {
	if err == nil {
		return nil
	}
	var ee *exec.ExitError
	if errors.As(err, &ee) {
		return nil
	}
	if errors.Is(err, os.ErrClosed) {
		return nil
	}
	return err
}

var _ sdk.Emitter = (*Client)(nil)

// tail keeps the first stderrLimit bytes a plugin wrote. The first bytes rather
// than the last: a startup failure explains itself at the top, and truncating
// the head would throw away the sentence that matters.
type tail struct {
	mu    sync.Mutex
	buf   []byte
	limit int
	over  bool
}

func (t *tail) Write(p []byte) (int, error) {
	t.mu.Lock()
	defer t.mu.Unlock()
	if room := t.limit - len(t.buf); room > 0 {
		if len(p) <= room {
			t.buf = append(t.buf, p...)
		} else {
			t.buf = append(t.buf, p[:room]...)
			t.over = true
		}
	} else if len(p) > 0 {
		t.over = true
	}
	return len(p), nil
}

func (t *tail) String() string {
	t.mu.Lock()
	defer t.mu.Unlock()
	s := strings.TrimSpace(string(t.buf))
	if t.over {
		s += "\n... (truncated)"
	}
	return s
}
