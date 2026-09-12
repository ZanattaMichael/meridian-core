package sdk

import (
	"bytes"
	"encoding/json"
	"fmt"
	"sort"
	"strings"
)

// encodeValues renders a params or data map as the JSON bytes that cross the
// wire. A nil map encodes as an empty object so the plugin side never has to
// distinguish "absent" from "empty".
func encodeValues(m map[string]any) ([]byte, error) {
	if m == nil {
		m = map[string]any{}
	}
	return json.Marshal(m)
}

// decodeValues is encodeValues reversed, with numbers restored to the shapes
// they had before encoding.
//
// Plain JSON decoding turns every number into a float64, which would rewrite a
// port of 8080 as 8080.0 and change the emitted artifact. Deterministic,
// byte-identical compilation is a guiding principle and it has to hold across
// the plugin boundary too, so integers are decoded as integers.
func decodeValues(b []byte) (map[string]any, error) {
	if len(b) == 0 {
		return map[string]any{}, nil
	}
	dec := json.NewDecoder(bytes.NewReader(b))
	dec.UseNumber()
	var raw any
	if err := dec.Decode(&raw); err != nil {
		return nil, fmt.Errorf("decoding values: %w", err)
	}
	v := restoreNumbers(raw)
	m, ok := v.(map[string]any)
	if !ok {
		return nil, fmt.Errorf("decoding values: expected an object, got %T", v)
	}
	return m, nil
}

// restoreNumbers walks a decoded value and converts json.Number back to int64
// or float64, preferring the integer whenever the literal was one.
func restoreNumbers(v any) any {
	switch t := v.(type) {
	case map[string]any:
		out := make(map[string]any, len(t))
		for k, val := range t {
			out[k] = restoreNumbers(val)
		}
		return out
	case []any:
		out := make([]any, len(t))
		for i, val := range t {
			out[i] = restoreNumbers(val)
		}
		return out
	case json.Number:
		if i, err := t.Int64(); err == nil {
			return i
		}
		if f, err := t.Float64(); err == nil {
			return f
		}
		// A number literal too large for either stays a string rather than
		// silently losing precision.
		return t.String()
	default:
		return v
	}
}

// CanonicalValue renders a value with sorted map keys. It is the determinism
// oracle the rest of the toolchain uses, exported here so a plugin's own tests
// can assert the same property without reimplementing it.
func CanonicalValue(v any) string {
	var b strings.Builder
	writeCanonical(&b, v)
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
