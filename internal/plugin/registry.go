package plugin

import (
	"context"
	"fmt"
	"io"
	"os"
	"path/filepath"
	"runtime"
	"sort"
	"strings"
	"sync"

	"github.com/ZanattaMichael/meridian-core/pkg/sdk"
)

// Registry resolves a target name to the emitter that implements it.
//
// It holds both compiled-in and subprocess emitters behind one lookup, because
// nothing above this package should care which a target is. That matters more
// than it sounds: it is what lets a target move from built-in to plugin, or the
// reverse, without touching the compiler.
type Registry struct {
	mu       sync.RWMutex
	emitters map[string]sdk.Emitter
	info     map[string]Info
	closers  []io.Closer
}

// NewRegistry returns an empty registry.
func NewRegistry() *Registry {
	return &Registry{emitters: map[string]sdk.Emitter{}, info: map[string]Info{}}
}

// Register adds a compiled-in emitter.
//
// A duplicate target name is an error rather than an overwrite: two emitters
// claiming "ansible" means the operator's plugin directory disagrees with their
// build, and silently picking one would make which output they got depend on
// load order.
func (r *Registry) Register(e sdk.Emitter) error {
	if e == nil {
		return fmt.Errorf("cannot register a nil emitter")
	}
	name := e.Name()
	if name == "" {
		return fmt.Errorf("cannot register an emitter with no target name")
	}

	r.mu.Lock()
	defer r.mu.Unlock()
	if err := r.claim(name, ""); err != nil {
		return err
	}
	r.emitters[name] = e
	r.info[name] = Info{
		ProtocolVersion: sdk.ProtocolVersion,
		SDKVersion:      sdk.Version,
		Name:            name,
		ResourceTypes:   e.SupportedResourceTypes(),
		Capabilities:    e.Capabilities(),
	}
	return nil
}

// Load starts one plugin binary and registers the target it reports.
func (r *Registry) Load(ctx context.Context, path string, opts ...StartOption) (Info, error) {
	client, err := Start(ctx, path, opts...)
	if err != nil {
		return Info{}, err
	}

	r.mu.Lock()
	defer r.mu.Unlock()
	if err := r.claim(client.Name(), path); err != nil {
		_ = client.Close()
		return Info{}, err
	}
	r.emitters[client.Name()] = client
	r.info[client.Name()] = client.Info()
	r.closers = append(r.closers, client)
	return client.Info(), nil
}

// claim reserves a target name. The caller holds the write lock.
func (r *Registry) claim(name, path string) error {
	existing, taken := r.info[name]
	if !taken {
		return nil
	}
	where := "compiled into this binary"
	if existing.Path != "" {
		where = existing.Path
	}
	from := "a compiled-in emitter"
	if path != "" {
		from = path
	}
	return fmt.Errorf("target %q is already registered from %s; %s claims the same name", name, where, from)
}

// Discover loads every plugin binary in a directory, by naming convention.
//
// A directory that does not exist is not an error: an operator with no plugins
// installed is the normal case, not a misconfiguration.
func (r *Registry) Discover(ctx context.Context, dir string, opts ...StartOption) ([]Info, error) {
	entries, err := os.ReadDir(dir)
	if os.IsNotExist(err) {
		return nil, nil
	}
	if err != nil {
		return nil, fmt.Errorf("reading the plugin directory %s: %w", dir, err)
	}

	// Sorted, so two hosts with the same directory load in the same order and a
	// duplicate-name conflict reports the same pair of binaries every time.
	names := make([]string, 0, len(entries))
	for _, e := range entries {
		if e.IsDir() || !isPluginName(e.Name()) {
			continue
		}
		names = append(names, e.Name())
	}
	sort.Strings(names)

	var loaded []Info
	for _, name := range names {
		info, err := r.Load(ctx, filepath.Join(dir, name), opts...)
		if err != nil {
			return loaded, err
		}
		loaded = append(loaded, info)
	}
	return loaded, nil
}

// isPluginName applies the naming convention, so discovery never executes a
// binary just to find out what it is.
func isPluginName(name string) bool {
	name = strings.TrimSuffix(name, ".exe")
	return strings.HasPrefix(name, BinaryPrefix) && len(name) > len(BinaryPrefix)
}

// BinaryName is the file name a plugin for the given target must have.
func BinaryName(target string) string {
	if runtime.GOOS == "windows" {
		return BinaryPrefix + target + ".exe"
	}
	return BinaryPrefix + target
}

// Emitter returns the emitter registered for a target name.
func (r *Registry) Emitter(name string) (sdk.Emitter, bool) {
	r.mu.RLock()
	defer r.mu.RUnlock()
	e, ok := r.emitters[name]
	return e, ok
}

// Info returns what a registered target reported about itself.
func (r *Registry) Info(name string) (Info, bool) {
	r.mu.RLock()
	defer r.mu.RUnlock()
	i, ok := r.info[name]
	return i, ok
}

// Names returns every registered target name, sorted.
func (r *Registry) Names() []string {
	r.mu.RLock()
	defer r.mu.RUnlock()
	out := make([]string, 0, len(r.emitters))
	for name := range r.emitters {
		out = append(out, name)
	}
	sort.Strings(out)
	return out
}

// Close shuts down every plugin process the registry started.
//
// It closes all of them even if one fails, because leaving processes running
// because a sibling misbehaved is the leak this method exists to prevent.
func (r *Registry) Close() error {
	r.mu.Lock()
	closers := r.closers
	r.closers = nil
	r.emitters = map[string]sdk.Emitter{}
	r.info = map[string]Info{}
	r.mu.Unlock()

	var errs []string
	for _, c := range closers {
		if err := c.Close(); err != nil {
			errs = append(errs, err.Error())
		}
	}
	if len(errs) > 0 {
		return fmt.Errorf("shutting down plugins: %s", strings.Join(errs, "; "))
	}
	return nil
}
