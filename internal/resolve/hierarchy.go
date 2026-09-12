// Package resolve implements Meridian's hierarchical data resolution: a
// Hiera-style layered merge owned natively by the core, with no dependency on
// Puppet's Hiera or PowerShell's Datum.
//
// Resolution is a pure function of (hierarchy, scope, layer contents). Given
// the same inputs it produces byte-identical output, which is what makes
// compilation deterministic downstream.
package resolve

import (
	"fmt"
	"path"
	"sort"
	"strings"
)

// Strategy names a per-key merge behaviour.
type Strategy string

const (
	// FirstFound takes the value from the highest-priority layer that declares
	// the key and ignores every lower layer. It is the default.
	FirstFound Strategy = "first-found"
	// DeepMerge merges nested maps recursively, so a higher layer can override
	// one nested key without clobbering its siblings.
	DeepMerge Strategy = "deep-merge"
	// UniqueArrayMerge concatenates arrays from every layer and removes
	// duplicates, keeping the first occurrence in priority order.
	UniqueArrayMerge Strategy = "unique-array-merge"
)

// KeyRule configures how one top-level key is merged.
type KeyRule struct {
	Strategy Strategy `yaml:"strategy" json:"strategy"`
	// DedupKey names the field that identifies an object element under
	// unique-array-merge. Without it, object elements are deduplicated by
	// their full canonical value, which is rarely what an author wants.
	DedupKey string `yaml:"dedupKey,omitempty" json:"dedupKey,omitempty"`
}

// Strictness decides what a missing hierarchy layer means.
type Strictness string

const (
	// Lenient treats a missing layer file as an empty layer. This is the usual
	// mode: most nodes have no node-specific file.
	Lenient Strictness = "lenient"
	// Strict treats a missing layer file as an error, for deployments that
	// require every declared layer to exist.
	Strict Strictness = "strict"
)

// Hierarchy is the parsed hierarchy.yaml: an ordered list of layer paths plus
// the merge rules applied across them.
//
// Paths are listed lowest-priority first, matching the design plan's example
// (common, environment, role, node), so the last matching layer wins.
type Hierarchy struct {
	Paths      []string           `yaml:"hierarchy" json:"hierarchy"`
	Default    KeyRule            `yaml:"default,omitempty" json:"default,omitempty"`
	Keys       map[string]KeyRule `yaml:"keys,omitempty" json:"keys,omitempty"`
	Strictness Strictness         `yaml:"strictness,omitempty" json:"strictness,omitempty"`
}

// ruleFor returns the merge rule governing a top-level key.
func (h *Hierarchy) ruleFor(key string) KeyRule {
	if rule, ok := h.Keys[key]; ok {
		if rule.Strategy == "" {
			rule.Strategy = h.defaultStrategy()
		}
		return rule
	}
	return KeyRule{Strategy: h.defaultStrategy(), DedupKey: h.Default.DedupKey}
}

func (h *Hierarchy) defaultStrategy() Strategy {
	if h.Default.Strategy == "" {
		return FirstFound
	}
	return h.Default.Strategy
}

// Validate reports configuration errors in the hierarchy itself, independently
// of any node being resolved.
func (h *Hierarchy) Validate() error {
	if len(h.Paths) == 0 {
		return fmt.Errorf("hierarchy: no layers declared")
	}
	switch h.Strictness {
	case "", Lenient, Strict:
	default:
		return fmt.Errorf("hierarchy: unknown strictness %q (expected %q or %q)", h.Strictness, Lenient, Strict)
	}
	if err := validStrategy(h.defaultStrategy()); err != nil {
		return fmt.Errorf("hierarchy: default: %w", err)
	}
	names := make([]string, 0, len(h.Keys))
	for name := range h.Keys {
		names = append(names, name)
	}
	sort.Strings(names)
	for _, name := range names {
		if err := validStrategy(h.ruleFor(name).Strategy); err != nil {
			return fmt.Errorf("hierarchy: keys.%s: %w", name, err)
		}
	}
	return nil
}

func validStrategy(s Strategy) error {
	switch s {
	case FirstFound, DeepMerge, UniqueArrayMerge:
		return nil
	}
	return fmt.Errorf("unknown merge strategy %q (expected %q, %q or %q)",
		s, FirstFound, DeepMerge, UniqueArrayMerge)
}

// Scope holds the variables a hierarchy path may interpolate with %{name}.
type Scope map[string]string

// maxInterpolationDepth bounds recursive %{var} expansion. A scope value may
// itself reference another variable; a chain that comes back to a variable
// already being expanded is a configuration error, not a stack overflow.
const maxInterpolationDepth = 32

// ExpandPaths interpolates every hierarchy path against scope, returning them
// in priority order (lowest first).
func (h *Hierarchy) ExpandPaths(scope Scope) ([]string, error) {
	out := make([]string, 0, len(h.Paths))
	seen := make(map[string]int, len(h.Paths))
	for i, raw := range h.Paths {
		expanded, err := interpolate(raw, scope, nil)
		if err != nil {
			return nil, fmt.Errorf("hierarchy[%d] %q: %w", i, raw, err)
		}
		// Clean the path so two spellings of the same layer (for example
		// "role/x.yaml" and "node/../role/x.yaml") compare equal and a layer
		// cannot be loaded twice under different names.
		expanded = path.Clean(expanded)
		if prev, dup := seen[expanded]; dup {
			return nil, fmt.Errorf("hierarchy[%d] %q resolves to %q, which hierarchy[%d] already resolves to: "+
				"a layer cannot appear twice in one hierarchy", i, raw, expanded, prev)
		}
		seen[expanded] = i
		out = append(out, expanded)
	}
	return out, nil
}

// interpolate replaces every %{name} in s with its scope value, recursively, so
// a variable whose value itself contains %{...} is expanded too. visiting
// carries the chain of variables currently being expanded, which is what makes
// a circular reference a reported error instead of infinite recursion.
func interpolate(s string, scope Scope, visiting []string) (string, error) {
	if len(visiting) > maxInterpolationDepth {
		return "", fmt.Errorf("interpolation nested more than %d levels deep (chain: %s)",
			maxInterpolationDepth, strings.Join(visiting, " -> "))
	}
	var b strings.Builder
	for i := 0; i < len(s); {
		if s[i] != '%' || i+1 >= len(s) || s[i+1] != '{' {
			b.WriteByte(s[i])
			i++
			continue
		}
		end := strings.IndexByte(s[i+2:], '}')
		if end < 0 {
			return "", fmt.Errorf("unterminated %%{ at offset %d", i)
		}
		name := strings.TrimSpace(s[i+2 : i+2+end])
		i += 2 + end + 1

		if name == "" {
			return "", fmt.Errorf("empty interpolation %%{}")
		}
		for _, active := range visiting {
			if active == name {
				return "", fmt.Errorf("circular hierarchy reference: %s",
					strings.Join(append(append([]string{}, visiting...), name), " -> "))
			}
		}
		value, ok := scope[name]
		if !ok {
			return "", fmt.Errorf("undefined variable %%{%s}", name)
		}
		nested, err := interpolate(value, scope, append(visiting, name))
		if err != nil {
			return "", err
		}
		b.WriteString(nested)
	}
	return b.String(), nil
}
