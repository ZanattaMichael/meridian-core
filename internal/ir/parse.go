package ir

import (
	"bytes"
	"encoding/json"
	"errors"
	"io"
	"os"
	"strings"

	"gopkg.in/yaml.v3"
)

// ParseFile reads path and parses every document it contains. The file
// extension is not trusted: content sniffing decides between JSON and YAML.
func ParseFile(path string) ([]Document, error) {
	data, err := os.ReadFile(path)
	if err != nil {
		return nil, &Error{Pos: Position{File: path}, Msg: "cannot read file", Err: err}
	}
	return Parse(path, data)
}

// Parse parses a single- or multi-document byte stream. name is used only for
// error messages. A YAML stream may contain several `---`-separated documents;
// a JSON stream may be one object or a top-level array of objects.
func Parse(name string, data []byte) ([]Document, error) {
	if looksLikeJSON(data) {
		return parseJSON(name, data)
	}
	return parseYAML(name, data)
}

// looksLikeJSON reports whether the first significant byte opens a JSON value.
// YAML flow style can look the same, but parsing such a document as JSON yields
// an equivalent result, so the ambiguity is harmless.
func looksLikeJSON(data []byte) bool {
	trimmed := bytes.TrimLeft(data, " \t\r\n")
	return len(trimmed) > 0 && (trimmed[0] == '{' || trimmed[0] == '[')
}

// header is the minimum needed to route a document to its concrete type.
type header struct {
	APIVersion string `yaml:"apiVersion" json:"apiVersion"`
	Kind       Kind   `yaml:"kind" json:"kind"`
}

func parseYAML(name string, data []byte) ([]Document, error) {
	// Pass one: collect raw nodes so document kinds and source positions are
	// known before anything is decoded into a concrete type.
	var nodes []*yaml.Node
	dec := yaml.NewDecoder(bytes.NewReader(data))
	for {
		var n yaml.Node
		err := dec.Decode(&n)
		if errors.Is(err, io.EOF) {
			break
		}
		if err != nil {
			return nil, yamlError(name, len(nodes), err)
		}
		nodes = append(nodes, &n)
	}
	if len(nodes) == 0 {
		return nil, errorf(Position{File: name}, "", "no documents found")
	}

	// Pass two: decode with KnownFields so an unrecognised field is an error
	// rather than data silently dropped on the floor.
	typed := yaml.NewDecoder(bytes.NewReader(data))
	typed.KnownFields(true)

	docs := make([]Document, 0, len(nodes))
	var errs ErrorList
	for i, node := range nodes {
		pos := Position{File: name, DocIdx: i, Line: nodeLine(node), Column: nodeColumn(node)}

		var h header
		if err := node.Decode(&h); err != nil {
			errs.add(yamlError(name, i, err))
			var discard yaml.Node
			_ = typed.Decode(&discard)
			continue
		}
		if err := checkHeader(h, pos); err != nil {
			errs.add(err)
			var discard yaml.Node
			_ = typed.Decode(&discard)
			continue
		}

		var doc Document
		var decodeErr error
		switch h.Kind {
		case KindInfrastructure:
			d := &Infrastructure{Pos: pos}
			decodeErr = typed.Decode(d)
			doc = d
		case KindResourceSet:
			d := &ResourceSet{Pos: pos}
			decodeErr = typed.Decode(d)
			doc = d
		}
		if decodeErr != nil {
			errs.add(yamlError(name, i, decodeErr))
			continue
		}
		annotate(doc, name, i, node)
		docs = append(docs, doc)
	}
	if err := errs.err(); err != nil {
		return nil, err
	}
	return docs, nil
}

func parseJSON(name string, data []byte) ([]Document, error) {
	// A top-level array is a document stream; anything else is one document.
	var raws []json.RawMessage
	if trimmed := bytes.TrimLeft(data, " \t\r\n"); len(trimmed) > 0 && trimmed[0] == '[' {
		if err := json.Unmarshal(data, &raws); err != nil {
			return nil, jsonError(name, 0, data, err)
		}
	} else {
		raws = []json.RawMessage{json.RawMessage(data)}
	}
	if len(raws) == 0 {
		return nil, errorf(Position{File: name}, "", "no documents found")
	}

	docs := make([]Document, 0, len(raws))
	var errs ErrorList
	for i, raw := range raws {
		pos := Position{File: name, DocIdx: i}

		var h header
		if err := json.Unmarshal(raw, &h); err != nil {
			errs.add(jsonError(name, i, raw, err))
			continue
		}
		if err := checkHeader(h, pos); err != nil {
			errs.add(err)
			continue
		}

		d := json.NewDecoder(bytes.NewReader(raw))
		d.DisallowUnknownFields()

		var doc Document
		var decodeErr error
		switch h.Kind {
		case KindInfrastructure:
			v := &Infrastructure{Pos: pos}
			decodeErr = d.Decode(v)
			doc = v
		case KindResourceSet:
			v := &ResourceSet{Pos: pos}
			decodeErr = d.Decode(v)
			doc = v
		}
		if decodeErr != nil {
			errs.add(jsonError(name, i, raw, decodeErr))
			continue
		}
		annotate(doc, name, i, nil)
		docs = append(docs, doc)
	}
	if err := errs.err(); err != nil {
		return nil, err
	}
	return docs, nil
}

// checkHeader rejects a document Meridian cannot safely interpret before any of
// its body is decoded.
func checkHeader(h header, pos Position) *Error {
	switch {
	case h.APIVersion == "":
		return errorf(pos, "apiVersion", "missing required field")
	case h.APIVersion != APIVersionV1:
		return errorf(pos, "apiVersion", "unsupported apiVersion %q (this build understands %q)", h.APIVersion, APIVersionV1)
	case h.Kind == "":
		return errorf(pos, "kind", "missing required field")
	case h.Kind != KindInfrastructure && h.Kind != KindResourceSet:
		return errorf(pos, "kind", "unknown kind %q (expected %q or %q)", h.Kind, KindInfrastructure, KindResourceSet)
	}
	return nil
}

// annotate stamps declaration indices and, for YAML, per-element source
// positions onto a freshly decoded document.
func annotate(doc Document, name string, docIdx int, node *yaml.Node) {
	docPos := Position{File: name, DocIdx: docIdx, Line: nodeLine(node), Column: nodeColumn(node)}
	spec := childNode(node, "spec")

	var resources []Resource
	switch d := doc.(type) {
	case *Infrastructure:
		resources = d.Spec.Resources
		annotateOutputs(d.Spec.Outputs, name, docIdx, childNode(spec, "outputs"), docPos)
	case *ResourceSet:
		resources = d.Spec.Resources
		if d.Spec.TargetHost != nil {
			d.Spec.TargetHost.Pos = positionOf(childNode(spec, "targetHost"), name, docIdx, docPos)
		}
	}

	resNodes := childNode(spec, "resources")
	for i := range resources {
		rn := indexNode(resNodes, i)
		resources[i].Index = i
		resources[i].Pos = positionOf(rn, name, docIdx, docPos)
		depNodes := childNode(rn, "dependsOn")
		for j := range resources[i].DependsOn {
			resources[i].DependsOn[j].Pos = positionOf(indexNode(depNodes, j), name, docIdx, resources[i].Pos)
		}
		notifyNodes := childNode(rn, "notifies")
		for j := range resources[i].Notifies {
			resources[i].Notifies[j].Pos = positionOf(indexNode(notifyNodes, j), name, docIdx, resources[i].Pos)
		}
	}
}

func annotateOutputs(outputs []Output, name string, docIdx int, seq *yaml.Node, fallback Position) {
	for i := range outputs {
		outputs[i].Pos = positionOf(indexNode(seq, i), name, docIdx, fallback)
	}
}

func positionOf(n *yaml.Node, name string, docIdx int, fallback Position) Position {
	if n == nil {
		return fallback
	}
	return Position{File: name, DocIdx: docIdx, Line: n.Line, Column: n.Column}
}

// childNode returns the value node for key in a mapping node, or nil.
func childNode(n *yaml.Node, key string) *yaml.Node {
	n = unwrapDocument(n)
	if n == nil || n.Kind != yaml.MappingNode {
		return nil
	}
	for i := 0; i+1 < len(n.Content); i += 2 {
		if n.Content[i].Value == key {
			return n.Content[i+1]
		}
	}
	return nil
}

// indexNode returns the i-th element of a sequence node, or nil.
func indexNode(n *yaml.Node, i int) *yaml.Node {
	n = unwrapDocument(n)
	if n == nil || n.Kind != yaml.SequenceNode || i < 0 || i >= len(n.Content) {
		return nil
	}
	return n.Content[i]
}

func unwrapDocument(n *yaml.Node) *yaml.Node {
	for n != nil && n.Kind == yaml.DocumentNode && len(n.Content) == 1 {
		n = n.Content[0]
	}
	return n
}

func nodeLine(n *yaml.Node) int {
	if n = unwrapDocument(n); n == nil {
		return 0
	}
	return n.Line
}

func nodeColumn(n *yaml.Node) int {
	if n = unwrapDocument(n); n == nil {
		return 0
	}
	return n.Column
}

// yamlError converts a yaml.v3 failure into a located Error. yaml.v3 reports
// the line inside its message rather than in a structured field, so the message
// is kept whole and the position carries what is known independently.
func yamlError(name string, docIdx int, err error) *Error {
	pos := Position{File: name, DocIdx: docIdx}
	msg := strings.TrimSpace(err.Error())
	var te *yaml.TypeError
	if errors.As(err, &te) {
		msg = strings.Join(te.Errors, "; ")
	}
	msg = strings.TrimPrefix(msg, "yaml: ")
	if line, ok := lineFromMessage(msg); ok {
		pos.Line = line
	}
	return &Error{Pos: pos, Msg: msg, Err: err}
}

// lineFromMessage pulls the line number out of a "line 7: ..." yaml message so
// the position is populated even though yaml.v3 does not expose it structurally.
func lineFromMessage(msg string) (int, bool) {
	idx := strings.Index(msg, "line ")
	if idx < 0 {
		return 0, false
	}
	rest := msg[idx+len("line "):]
	end := 0
	for end < len(rest) && rest[end] >= '0' && rest[end] <= '9' {
		end++
	}
	if end == 0 {
		return 0, false
	}
	line := 0
	for _, c := range rest[:end] {
		line = line*10 + int(c-'0')
	}
	return line, true
}

// jsonError converts a JSON failure into a located Error, translating the byte
// offset the decoder reports into a line and column.
func jsonError(name string, docIdx int, data []byte, err error) *Error {
	pos := Position{File: name, DocIdx: docIdx}
	var offset int64 = -1
	var syn *json.SyntaxError
	var typ *json.UnmarshalTypeError
	switch {
	case errors.As(err, &syn):
		offset = syn.Offset
	case errors.As(err, &typ):
		offset = typ.Offset
	}
	if offset >= 0 {
		pos.Line, pos.Column = lineColumn(data, int(offset))
	}
	return &Error{Pos: pos, Msg: strings.TrimSpace(err.Error()), Err: err}
}

// lineColumn converts a byte offset into a 1-based line and column.
func lineColumn(data []byte, offset int) (int, int) {
	if offset > len(data) {
		offset = len(data)
	}
	line, col := 1, 1
	for i := 0; i < offset; i++ {
		if data[i] == '\n' {
			line++
			col = 1
			continue
		}
		col++
	}
	return line, col
}
