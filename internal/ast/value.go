package ast

import (
	"encoding/json"
	"fmt"
	"sort"
	"strings"
)

// copyValue deep-copies a decoded YAML/JSON value and normalises it on the way
// through: a YAML map decoded as map[any]any becomes map[string]any so every
// emitter sees one shape. Copying matters because a resource's params must not
// alias the document they were parsed from; a transform that rewrites a param
// would otherwise mutate the source IR.
func copyValue(v any) any {
	switch t := v.(type) {
	case map[string]any:
		out := make(map[string]any, len(t))
		for k, val := range t {
			out[k] = copyValue(val)
		}
		return out
	case map[any]any:
		out := make(map[string]any, len(t))
		for k, val := range t {
			out[fmt.Sprint(k)] = copyValue(val)
		}
		return out
	case []any:
		out := make([]any, len(t))
		for i, val := range t {
			out[i] = copyValue(val)
		}
		return out
	case nil:
		return nil
	default:
		return v
	}
}

// canonicalValue renders a value with sorted map keys, the same determinism
// oracle the resolve package uses.
func canonicalValue(v any) string {
	var b strings.Builder
	writeCanonical(&b, copyValue(v))
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
	case []any:
		b.WriteByte('[')
		for i, item := range t {
			if i > 0 {
				b.WriteByte(',')
			}
			writeCanonical(b, item)
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
