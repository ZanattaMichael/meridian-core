package ansible

import (
	"fmt"
	"sort"
	"strings"

	"gopkg.in/yaml.v3"
)

// Artifact file paths. They are constants rather than inline strings so a test
// and a runner can refer to the same names.
const (
	PlaybookPath  = "playbook.yml"
	InventoryPath = "inventory.ini"
)

// serialise renders a playbook into its final files. This stage performs no
// mapping and no ordering: everything it writes was decided by transform and
// sort, so the same playbook always renders to the same bytes.
func serialise(pb *playbook) (map[string]string, error) {
	doc := seqNode(playNode(pb))
	var b strings.Builder
	enc := yaml.NewEncoder(&b)
	enc.SetIndent(2)
	if err := enc.Encode(doc); err != nil {
		return nil, &Error{Rule: "serialise", Msg: fmt.Sprintf("encoding the playbook failed: %v", err)}
	}
	if err := enc.Close(); err != nil {
		return nil, &Error{Rule: "serialise", Msg: fmt.Sprintf("encoding the playbook failed: %v", err)}
	}

	return map[string]string{
		PlaybookPath:  "---\n" + b.String(),
		InventoryPath: inventory(pb),
	}, nil
}

// inventory writes the single host this play targets. Meridian resolves the
// host itself, so the inventory is a literal rather than a lookup.
func inventory(pb *playbook) string {
	return "[meridian]\n" + pb.Hosts + "\n"
}

func playNode(pb *playbook) *yaml.Node {
	pairs := []pair{
		{"name", scalarNode(pb.Name)},
		{"hosts", scalarNode(pb.Hosts)},
	}
	pairs = append(pairs, pair{"tasks", taskListNode(pb.Tasks)})
	if len(pb.Handlers) > 0 {
		pairs = append(pairs, pair{"handlers", taskListNode(pb.Handlers)})
	}
	return mappingNode(pairs...)
}

func taskListNode(tasks []task) *yaml.Node {
	nodes := make([]*yaml.Node, 0, len(tasks))
	for _, t := range tasks {
		nodes = append(nodes, taskNode(t))
	}
	return seqNode(nodes...)
}

// taskNode lays a task out in a fixed key order: name, module, then the
// structural keys. Key order is part of the output's identity, so it is decided
// here once rather than left to map iteration.
func taskNode(t task) *yaml.Node {
	pairs := []pair{
		{"name", scalarNode(t.Name)},
		{t.Module, valueNode(t.Args)},
	}
	if len(t.Notify) > 0 {
		items := make([]*yaml.Node, 0, len(t.Notify))
		for _, n := range t.Notify {
			items = append(items, scalarNode(n))
		}
		pairs = append(pairs, pair{"notify", seqNode(items...)})
	}
	if t.When != "" {
		pairs = append(pairs, pair{"when", scalarNode(t.When)})
	}
	return mappingNode(pairs...)
}

// valueNode renders an arbitrary param value. Map keys are sorted so a value
// that came out of a Go map still serialises identically every time.
func valueNode(v any) *yaml.Node {
	switch t := v.(type) {
	case map[string]any:
		keys := make([]string, 0, len(t))
		for k := range t {
			keys = append(keys, k)
		}
		sort.Strings(keys)
		pairs := make([]pair, 0, len(keys))
		for _, k := range keys {
			pairs = append(pairs, pair{k, valueNode(t[k])})
		}
		return mappingNode(pairs...)
	case []any:
		items := make([]*yaml.Node, 0, len(t))
		for _, item := range t {
			items = append(items, valueNode(item))
		}
		return seqNode(items...)
	default:
		n := &yaml.Node{}
		// Encoding through yaml itself keeps scalar quoting rules (numbers,
		// booleans, strings that look like either) in one place.
		if err := n.Encode(t); err != nil {
			return scalarNode(fmt.Sprint(t))
		}
		return n
	}
}

type pair struct {
	key   string
	value *yaml.Node
}

func mappingNode(pairs ...pair) *yaml.Node {
	n := &yaml.Node{Kind: yaml.MappingNode, Tag: "!!map"}
	for _, p := range pairs {
		n.Content = append(n.Content, scalarNode(p.key), p.value)
	}
	return n
}

func seqNode(items ...*yaml.Node) *yaml.Node {
	return &yaml.Node{Kind: yaml.SequenceNode, Tag: "!!seq", Content: items}
}

func scalarNode(s string) *yaml.Node {
	n := &yaml.Node{}
	_ = n.Encode(s)
	return n
}
