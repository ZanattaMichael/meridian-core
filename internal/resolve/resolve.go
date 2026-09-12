package resolve

import (
	"errors"
	"fmt"
	"io/fs"
	"sort"

	"gopkg.in/yaml.v3"
)

// ErrLayerNotFound is what a Loader returns for a layer that does not exist.
// Whether that is fatal depends on the hierarchy's strictness.
var ErrLayerNotFound = errors.New("hierarchy layer not found")

// Loader reads one hierarchy layer by its expanded path. Keeping this an
// interface is what lets resolution stay a pure, disk-free unit under test.
type Loader interface {
	Load(path string) (Data, error)
}

// FSLoader reads layers as YAML (or JSON, which YAML accepts) from a
// filesystem.
type FSLoader struct {
	FS fs.FS
}

func (l FSLoader) Load(path string) (Data, error) {
	raw, err := fs.ReadFile(l.FS, path)
	if err != nil {
		if errors.Is(err, fs.ErrNotExist) {
			return nil, fmt.Errorf("%q: %w", path, ErrLayerNotFound)
		}
		return nil, fmt.Errorf("%q: %w", path, err)
	}
	var out map[string]any
	if err := yaml.Unmarshal(raw, &out); err != nil {
		return nil, fmt.Errorf("%q: %w", path, err)
	}
	data := Data{}
	for k, v := range out {
		data[k] = normalize(v)
	}
	return data, nil
}

// MapLoader serves layers from memory.
type MapLoader map[string]Data

func (l MapLoader) Load(path string) (Data, error) {
	data, ok := l[path]
	if !ok {
		return nil, fmt.Errorf("%q: %w", path, ErrLayerNotFound)
	}
	return data, nil
}

// Layer records what one hierarchy level contributed, in priority order. It is
// kept for `meridian explain` and for debugging a surprising merge.
type Layer struct {
	Path  string
	Found bool
	Keys  []string
}

// Result is a completed resolution.
type Result struct {
	Data   Data
	Layers []Layer
}

// Resolve merges every hierarchy layer for one node into a single flat map.
//
// Layers are listed lowest-priority first, so a later layer overrides an
// earlier one. The result depends only on the hierarchy, the scope and the
// layer contents: repeated runs over the same inputs produce identical output.
func Resolve(h *Hierarchy, scope Scope, loader Loader) (*Result, error) {
	if h == nil {
		return nil, errors.New("resolve: nil hierarchy")
	}
	if err := h.Validate(); err != nil {
		return nil, err
	}
	paths, err := h.ExpandPaths(scope)
	if err != nil {
		return nil, err
	}

	strict := h.Strictness == Strict
	layers := make([]Layer, 0, len(paths))
	loaded := make([]Data, 0, len(paths))
	for _, path := range paths {
		data, err := loader.Load(path)
		switch {
		case err == nil:
		case errors.Is(err, ErrLayerNotFound):
			if strict {
				return nil, fmt.Errorf("resolve: %w", err)
			}
			layers = append(layers, Layer{Path: path, Found: false})
			loaded = append(loaded, nil)
			continue
		default:
			return nil, fmt.Errorf("resolve: %w", err)
		}
		layers = append(layers, Layer{Path: path, Found: true, Keys: sortedKeys(data)})
		loaded = append(loaded, data)
	}

	// Collect every key that any layer declares, in sorted order, so the merge
	// walks keys deterministically regardless of map iteration.
	keySet := map[string]struct{}{}
	for _, data := range loaded {
		for k := range data {
			keySet[k] = struct{}{}
		}
	}
	keys := make([]string, 0, len(keySet))
	for k := range keySet {
		keys = append(keys, k)
	}
	sort.Strings(keys)

	out := make(Data, len(keys))
	for _, key := range keys {
		rule := h.ruleFor(key)
		// Values in priority order, lowest first, from layers declaring the key.
		var values []any
		for _, data := range loaded {
			if v, ok := data[key]; ok {
				values = append(values, normalize(v))
			}
		}
		if len(values) == 0 {
			continue
		}
		switch rule.Strategy {
		case FirstFound:
			out[key] = values[len(values)-1]
		case DeepMerge:
			merged := values[0]
			for _, v := range values[1:] {
				merged = deepMerge(merged, v)
			}
			out[key] = merged
		case UniqueArrayMerge:
			merged, err := uniqueArrayMerge(values, rule.DedupKey)
			if err != nil {
				return nil, fmt.Errorf("resolve: key %q: %w", key, err)
			}
			out[key] = merged
		default:
			return nil, fmt.Errorf("resolve: key %q: %w", key, validStrategy(rule.Strategy))
		}
	}
	return &Result{Data: out, Layers: layers}, nil
}

func sortedKeys(d Data) []string {
	keys := make([]string, 0, len(d))
	for k := range d {
		keys = append(keys, k)
	}
	sort.Strings(keys)
	return keys
}
