package resolve

import (
	"encoding/json"
	"fmt"
	"sort"
	"strings"
)

// Data is one layer's contents, or a fully resolved result.
type Data map[string]any

// Canonical renders data in a stable, sorted-key form. Two Data values that
// compare equal always render identically, which is how determinism is asserted
// without depending on Go map iteration order.
func (d Data) Canonical() string {
	var b strings.Builder
	writeCanonical(&b, map[string]any(d))
	return b.String()
}

func writeCanonical(b *strings.Builder, v any) {
	switch t := v.(type) {
	case map[string]any:
		keys := make([]string, 0, len(t))
		for k := range t {
			keys = append(keys, k)
		}
		sort.Strings(keys)
		b.WriteByte('{')
		for i, k := range keys {
			if i > 0 {
				b.WriteByte(',')
			}
			enc, _ := json.Marshal(k)
			b.Write(enc)
			b.WriteByte(':')
			writeCanonical(b, t[k])
		}
		b.WriteByte('}')
	case map[any]any:
		// YAML decoders that predate typed keys can produce this shape.
		writeCanonical(b, normalizeMap(t))
	case []any:
		b.WriteByte('[')
		for i, e := range t {
			if i > 0 {
				b.WriteByte(',')
			}
			writeCanonical(b, e)
		}
		b.WriteByte(']')
	default:
		enc, err := json.Marshal(t)
		if err != nil {
			fmt.Fprintf(b, "%q", fmt.Sprint(t))
			return
		}
		b.Write(enc)
	}
}

// normalizeMap converts a map[any]any into map[string]any so every value in the
// pipeline has one shape.
func normalizeMap(m map[any]any) map[string]any {
	out := make(map[string]any, len(m))
	for k, v := range m {
		out[fmt.Sprint(k)] = normalize(v)
	}
	return out
}

// normalize rewrites a decoded value into the canonical Go shapes the merge
// functions expect, recursively.
func normalize(v any) any {
	switch t := v.(type) {
	case map[any]any:
		return normalizeMap(t)
	case map[string]any:
		out := make(map[string]any, len(t))
		for k, e := range t {
			out[k] = normalize(e)
		}
		return out
	case []any:
		out := make([]any, len(t))
		for i, e := range t {
			out[i] = normalize(e)
		}
		return out
	default:
		return v
	}
}

// deepMerge returns the recursive merge of base and override, with override
// winning on conflict. Neither input is mutated: nested maps are copied, so a
// merged result never aliases a layer's own data.
func deepMerge(base, override any) any {
	bm, baseIsMap := asMap(base)
	om, overIsMap := asMap(override)
	if !baseIsMap || !overIsMap {
		return normalize(override)
	}
	out := make(map[string]any, len(bm)+len(om))
	for k, v := range bm {
		out[k] = normalize(v)
	}
	for k, v := range om {
		if existing, ok := out[k]; ok {
			out[k] = deepMerge(existing, v)
			continue
		}
		out[k] = normalize(v)
	}
	return out
}

func asMap(v any) (map[string]any, bool) {
	switch t := v.(type) {
	case map[string]any:
		return t, true
	case map[any]any:
		return normalizeMap(t), true
	}
	return nil, false
}

// uniqueArrayMerge concatenates values in priority order and drops duplicates,
// keeping the first occurrence. dedupKey, when set, identifies object elements
// by that field; without it an object element is identified by its whole
// canonical value.
func uniqueArrayMerge(values []any, dedupKey string) ([]any, error) {
	out := make([]any, 0, len(values))
	seen := make(map[string]struct{}, len(values))
	for _, v := range values {
		items, ok := asSlice(v)
		if !ok {
			return nil, fmt.Errorf("unique-array-merge requires every layer to provide an array, got %T", v)
		}
		for _, item := range items {
			item = normalize(item)
			key, err := dedupIdentity(item, dedupKey)
			if err != nil {
				return nil, err
			}
			if _, dup := seen[key]; dup {
				continue
			}
			seen[key] = struct{}{}
			out = append(out, item)
		}
	}
	return out, nil
}

func asSlice(v any) ([]any, bool) {
	if s, ok := v.([]any); ok {
		return s, true
	}
	return nil, false
}

// dedupIdentity computes the string identity used to detect duplicates.
func dedupIdentity(item any, dedupKey string) (string, error) {
	if dedupKey == "" {
		return canonicalOf(item), nil
	}
	m, ok := asMap(item)
	if !ok {
		// A primitive in a list configured with a dedup key is identified by
		// its own value; mixing shapes is legal and should not error.
		return canonicalOf(item), nil
	}
	v, ok := m[dedupKey]
	if !ok {
		return "", fmt.Errorf("unique-array-merge: element %s has no dedup key %q", canonicalOf(item), dedupKey)
	}
	return "key:" + canonicalOf(v), nil
}

func canonicalOf(v any) string {
	var b strings.Builder
	writeCanonical(&b, normalize(v))
	return b.String()
}
