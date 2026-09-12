package ansible

import (
	"fmt"
	"sort"
	"strings"

	"github.com/ZanattaMichael/meridian-core/pkg/sdk"
)

// Error is a compile failure for this target.
//
// It is sdk.CompileError rather than a type of this package's own because the
// error has to survive a process boundary. A plugin that flattened its failures
// to strings would strip the resource id, the rule name and the source position
// on the way out, and the host would have nothing to report against the line the
// author actually wrote.
type Error = sdk.CompileError

// errorf builds a failure against one resource, stamped with this target.
func errorf(r sdk.Resource, rule, format string, args ...any) *Error {
	return &Error{
		Target:   Name,
		Resource: r.ID,
		Rule:     rule,
		Msg:      fmt.Sprintf(format, args...),
		Pos:      r.Pos,
	}
}

// typeMapping describes how one IR resource type becomes a native module call.
//
// The table is explicit rather than reflective on purpose: an IR type with no
// entry is a hard error, and a param with no entry is a hard error, so a target
// can never silently drop something the author asked for.
type typeMapping struct {
	// module is the native module name. A type whose module depends on its
	// params leaves this empty and sets moduleFor.
	module    string
	moduleFor func(r sdk.Resource) string

	// stateArg is the module argument the IR `state` maps onto; empty means the
	// type accepts no state at all.
	stateArg string
	// stateArgFor overrides stateArg when the selected module decides it. A
	// module that returns "" here implies its own state and emits no argument.
	stateArgFor func(module string) string
	// statesWithoutArg lists the native state values a module may imply rather
	// than emit. A state outside it is an error on such a module, because
	// silently dropping it would change what the resource means.
	statesWithoutArg map[string]bool
	// states maps every accepted IR state value to its native equivalent.
	states map[string]string
	// defaultState is the IR state assumed when a resource declares none.
	defaultState string

	// params maps each accepted IR param name to its native argument name. A
	// param outside this map is an error.
	params map[string]string
	// paramsFor overrides params when the native argument names depend on which
	// module was selected.
	paramsFor func(module string) map[string]string
	// required lists IR param names that must be present.
	required []string

	// actions maps a notify action to the IR state the synthesised handler
	// applies. An action outside this map is an error.
	actions map[string]string
}

var mappings = map[string]typeMapping{
	"package": {
		module:       "ansible.builtin.package",
		stateArg:     "state",
		states:       map[string]string{"present": "present", "absent": "absent", "latest": "latest"},
		defaultState: "present",
		params:       map[string]string{"name": "name"},
		required:     []string{"name"},
	},
	"service": {
		module:   "ansible.builtin.service",
		stateArg: "state",
		states: map[string]string{
			"running":   "started",
			"started":   "started",
			"stopped":   "stopped",
			"restarted": "restarted",
			"reloaded":  "reloaded",
		},
		defaultState: "running",
		params:       map[string]string{"name": "name", "enabled": "enabled"},
		required:     []string{"name"},
		actions: map[string]string{
			"restart": "restarted",
			"reload":  "reloaded",
			"start":   "started",
			"stop":    "stopped",
		},
	},
	"file": {
		// A file with literal content is a copy, not a file stat change; the two
		// take different native argument names, which is why this type needs
		// both moduleFor and paramsFor.
		moduleFor: func(r sdk.Resource) string {
			if _, ok := r.Params["content"]; ok {
				return "ansible.builtin.copy"
			}
			return "ansible.builtin.file"
		},
		stateArg: "state",
		// The copy module writes a file by definition and rejects a state
		// argument, so "file" is implied there and any other state is a
		// contradiction with literal content.
		stateArgFor: func(module string) string {
			if module == "ansible.builtin.copy" {
				return ""
			}
			return "state"
		},
		statesWithoutArg: map[string]bool{"file": true},
		states: map[string]string{
			"present":   "file",
			"file":      "file",
			"directory": "directory",
			"absent":    "absent",
		},
		defaultState: "present",
		paramsFor: func(module string) map[string]string {
			common := map[string]string{"owner": "owner", "group": "group", "mode": "mode"}
			if module == "ansible.builtin.copy" {
				common["path"] = "dest"
				common["content"] = "content"
			} else {
				common["path"] = "path"
			}
			return common
		},
		required: []string{"path"},
	},
	"user": {
		module:       "ansible.builtin.user",
		stateArg:     "state",
		states:       map[string]string{"present": "present", "absent": "absent"},
		defaultState: "present",
		params: map[string]string{
			"name": "name", "uid": "uid", "shell": "shell",
			"home": "home", "groups": "groups", "system": "system",
		},
		required: []string{"name"},
	},
	"group": {
		module:       "ansible.builtin.group",
		stateArg:     "state",
		states:       map[string]string{"present": "present", "absent": "absent"},
		defaultState: "present",
		params:       map[string]string{"name": "name", "gid": "gid", "system": "system"},
		required:     []string{"name"},
	},
	"exec": {
		// exec is the documented escape hatch, so it takes no state: there is no
		// desired state to converge on, only a command to run.
		moduleFor: func(r sdk.Resource) string {
			if shell, ok := r.Params["shell"]; ok && shell == true {
				return "ansible.builtin.shell"
			}
			return "ansible.builtin.command"
		},
		params: map[string]string{
			"command": "cmd", "creates": "creates",
			"removes": "removes", "chdir": "chdir", "shell": "",
		},
		required: []string{"command"},
		actions:  map[string]string{"run": ""},
	},
}

// SupportedResourceTypes lists every IR resource type this target maps.
func SupportedResourceTypes() []string {
	out := make([]string, 0, len(mappings))
	for t := range mappings {
		out = append(out, t)
	}
	sort.Strings(out)
	return out
}

// transform maps the target-agnostic tree onto native tasks and the handlers
// its notify edges require. It does no ordering work: the resources arrive in
// apply order and leave in the same order.
func transform(g *sdk.ResourceGraph) (*playbook, error) {
	pb := &playbook{Name: g.Name, Hosts: g.Host}

	notify, err := handlerNames(g)
	if err != nil {
		return nil, err
	}

	for _, r := range g.Resources {
		t, err := taskFor(r)
		if err != nil {
			return nil, err
		}
		t.Notify = notify[r.ID]
		pb.Tasks = append(pb.Tasks, t)
	}

	pb.Handlers, err = handlers(g)
	if err != nil {
		return nil, err
	}
	return pb, nil
}

// taskFor maps one resource onto its native module call.
func taskFor(r sdk.Resource) (task, error) {
	m, ok := mappings[r.Type]
	if !ok {
		return task{}, errorf(r, "unsupported-resource-type",
			"unsupported resource type %q on this target (supported: %s)",
			r.Type, strings.Join(SupportedResourceTypes(), ", "))
	}

	module := m.module
	if m.moduleFor != nil {
		module = m.moduleFor(r)
	}

	allowed := m.params
	if m.paramsFor != nil {
		allowed = m.paramsFor(module)
	}

	args := make(map[string]any, len(r.Params)+1)
	for _, name := range sortedParamNames(r.Params) {
		native, ok := allowed[name]
		if !ok {
			return task{}, errorf(r, "unsupported-param",
				"resource type %q does not accept param %q on this target (accepted: %s)",
				r.Type, name, strings.Join(sortedKeys(allowed), ", "))
		}
		if native == "" {
			// The param steers the mapping itself (exec's `shell`) rather than
			// becoming an argument.
			continue
		}
		if r.Params[name] == nil {
			return task{}, errorf(r, "empty-param",
				"param %q on resource type %q has no value", name, r.Type)
		}
		args[native] = r.Params[name]
	}

	for _, need := range m.required {
		if _, ok := r.Params[need]; !ok {
			return task{}, errorf(r, "missing-param",
				"resource type %q requires param %q on this target", r.Type, need)
		}
	}

	if err := applyState(r, m, module, args); err != nil {
		return task{}, err
	}

	return task{
		Name:       r.ID,
		Module:     module,
		Args:       args,
		When:       r.RuntimeWhen,
		ResourceID: r.ID,
		Pos:        r.Pos,
	}, nil
}

// applyState maps the IR state vocabulary onto the module's own. Every accepted
// value is listed in the mapping table, so an unrecognised one names the values
// that would have worked rather than failing generically.
func applyState(r sdk.Resource, m typeMapping, module string, args map[string]any) error {
	if m.stateArg == "" && m.stateArgFor == nil {
		if r.State != "" {
			return errorf(r, "unsupported-state",
				"resource type %q accepts no state on this target, got %q", r.Type, r.State)
		}
		return nil
	}

	state := r.State
	if state == "" {
		state = m.defaultState
	}
	native, ok := m.states[state]
	if !ok {
		return errorf(r, "unsupported-state",
			"resource type %q does not accept state %q on this target (accepted: %s)",
			r.Type, state, strings.Join(sortedKeys(m.states), ", "))
	}
	arg := m.stateArg
	if m.stateArgFor != nil {
		arg = m.stateArgFor(module)
	}
	if arg == "" {
		if !m.statesWithoutArg[native] {
			return errorf(r, "unsupported-state",
				"resource type %q cannot have state %q as compiled to %s on this target",
				r.Type, state, module)
		}
		return nil
	}
	args[arg] = native
	return nil
}

// handlerName is the name a notify edge compiles to. It is derived from the
// action and the notified resource so two notifiers of the same change collapse
// onto one handler, which is what Ansible's own dedupe expects.
func handlerName(action, resource string) string {
	return action + " " + resource
}

// handlerNames returns, per notifying resource, the sorted handler names its
// task must list under `notify:`.
func handlerNames(g *sdk.ResourceGraph) (map[string][]string, error) {
	out := make(map[string][]string)
	seen := make(map[string]map[string]bool)
	for _, e := range g.EdgesOfKind(sdk.Notify) {
		to, ok := g.Resource(e.To)
		if !ok {
			// Build already rejects an edge to an unknown resource; this guards
			// against a tree assembled some other way.
			from, _ := g.Resource(e.From)
			return nil, errorf(from, "unknown-notify-target",
				"notifies an unknown resource %q", e.To)
		}
		if _, err := handlerFor(to, e.Action); err != nil {
			return nil, err
		}
		name := handlerName(e.Action, e.To)
		if seen[e.From] == nil {
			seen[e.From] = make(map[string]bool)
		}
		if seen[e.From][name] {
			continue
		}
		seen[e.From][name] = true
		out[e.From] = append(out[e.From], name)
	}
	for id := range out {
		sort.Strings(out[id])
	}
	return out, nil
}

// handlers builds the deduped handler block for every distinct notify target
// and action in the tree.
func handlers(g *sdk.ResourceGraph) ([]task, error) {
	seen := make(map[string]bool)
	var out []task
	for _, e := range g.EdgesOfKind(sdk.Notify) {
		name := handlerName(e.Action, e.To)
		if seen[name] {
			continue
		}
		seen[name] = true
		to, ok := g.Resource(e.To)
		if !ok {
			continue
		}
		h, err := handlerFor(to, e.Action)
		if err != nil {
			return nil, err
		}
		out = append(out, h)
	}
	sort.SliceStable(out, func(i, j int) bool { return out[i].Name < out[j].Name })
	return out, nil
}

// handlerFor synthesises the handler that performs one action on one resource.
// The handler is the resource's own module call with the action's state applied,
// rather than a shell-out, so the notified change stays idempotent.
func handlerFor(r sdk.Resource, action string) (task, error) {
	m, ok := mappings[r.Type]
	if !ok {
		return task{}, errorf(r, "unsupported-resource-type",
			"unsupported resource type %q on this target (supported: %s)",
			r.Type, strings.Join(SupportedResourceTypes(), ", "))
	}
	state, ok := m.actions[action]
	if !ok {
		accepted := strings.Join(sortedKeys(m.actions), ", ")
		if accepted == "" {
			return task{}, errorf(r, "unsupported-action",
				"resource type %q cannot be notified on this target, got action %q", r.Type, action)
		}
		return task{}, errorf(r, "unsupported-action",
			"resource type %q does not support notify action %q on this target (supported: %s)",
			r.Type, action, accepted)
	}

	notified := r
	notified.State = state
	t, err := taskFor(notified)
	if err != nil {
		return task{}, err
	}
	t.Name = handlerName(action, r.ID)
	// A handler must not carry the notifying task's own runtime condition: it
	// runs because something changed, not because the condition still holds.
	t.When = ""
	return t, nil
}

func sortedParamNames(params map[string]any) []string {
	out := make([]string, 0, len(params))
	for k := range params {
		out = append(out, k)
	}
	sort.Strings(out)
	return out
}

func sortedKeys(m map[string]string) []string {
	out := make([]string, 0, len(m))
	for k := range m {
		out = append(out, k)
	}
	sort.Strings(out)
	return out
}
