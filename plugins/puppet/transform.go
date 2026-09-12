package puppet

import (
	"fmt"
	"sort"
	"strings"

	"github.com/ZanattaMichael/meridian-core/pkg/sdk"
)

// Error is a compile failure for this target.
//
// It is sdk.CompileError rather than a type of this package's own because the
// error has to survive a process boundary with the resource id, the rule name
// and the source position intact.
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

// typeMapping describes how one IR resource type becomes a native Puppet
// resource declaration.
//
// The table is explicit rather than reflective on purpose: an IR type with no
// entry is a hard error, and a param with no entry is a hard error, so a target
// can never silently drop something the author asked for.
type typeMapping struct {
	// puppetType is the native resource type as it is written in a declaration.
	puppetType string

	// ensure maps every accepted IR state value to its native ensure value. An
	// empty map means the type takes no ensure at all.
	ensure map[string]string
	// defaultState is the IR state assumed when a resource declares none.
	defaultState string

	// params maps each accepted IR param name to its native attribute name. A
	// param outside this map is an error. An entry mapping to the empty string
	// steers the mapping rather than becoming an attribute.
	params map[string]string
	// required lists IR param names that must be present.
	required []string
	// namevar is the attribute Puppet identifies the managed thing by. Two
	// resources of one type sharing a namevar manage the same thing under two
	// titles, which Puppet only discovers at apply time; validate catches it
	// here instead.
	namevar string

	// refreshable reports whether Puppet can do anything at all when this type
	// is notified. Only a refreshable type may be the target of a notifies edge.
	refreshable bool
	// actions lists the notify actions this type can perform natively. Puppet's
	// refresh carries no argument, so an action is either something the type
	// already does on refresh or something this target must refuse.
	actions map[string]bool
	// refusedActions explains, per action, why Puppet cannot express it. Being
	// able to say what a target cannot do is the whole point of the
	// no-silent-capability-loss principle.
	refusedActions map[string]string
}

var mappings = map[string]typeMapping{
	"package": {
		puppetType:   "package",
		ensure:       map[string]string{"present": "present", "absent": "absent", "latest": "latest"},
		defaultState: "present",
		params:       map[string]string{"name": "name"},
		required:     []string{"name"},
		namevar:      "name",
	},
	"service": {
		puppetType: "service",
		// A catalog describes desired state, and "restarted" is not a state a
		// node can be left in. Ansible accepts it because a task is an action;
		// here the equivalent is a notifies edge, so the two values Ansible maps
		// to actions are deliberately absent from this table.
		ensure: map[string]string{
			"running": "running",
			"started": "running",
			"stopped": "stopped",
		},
		defaultState: "running",
		params:       map[string]string{"name": "name", "enabled": "enable"},
		required:     []string{"name"},
		namevar:      "name",
		refreshable:  true,
		actions:      map[string]bool{"restart": true, "refresh": true},
		refusedActions: map[string]string{
			"reload": "Puppet's refresh restarts a service; it takes no argument and " +
				"cannot be told to reload instead. Emitting a restart here would do " +
				"more than the document asked for. Set the service's own restart " +
				"attribute in a Puppet module if a reload command exists for it",
			"start": "a service that should be running is already declared with " +
				"state: running; Puppet converges on that without being notified",
			"stop": "Puppet refreshes a service by restarting it and has no way to " +
				"stop one in response to a change; declare state: stopped instead",
		},
	},
	"file": {
		puppetType: "file",
		ensure: map[string]string{
			"present":   "file",
			"file":      "file",
			"directory": "directory",
			"absent":    "absent",
		},
		defaultState: "present",
		params: map[string]string{
			"path": "path", "content": "content",
			"owner": "owner", "group": "group", "mode": "mode",
		},
		required: []string{"path"},
		namevar:  "path",
	},
	"user": {
		puppetType:   "user",
		ensure:       map[string]string{"present": "present", "absent": "absent"},
		defaultState: "present",
		params: map[string]string{
			"name": "name", "uid": "uid", "shell": "shell",
			"home": "home", "groups": "groups", "system": "system",
		},
		required: []string{"name"},
		namevar:  "name",
	},
	"group": {
		puppetType:   "group",
		ensure:       map[string]string{"present": "present", "absent": "absent"},
		defaultState: "present",
		params:       map[string]string{"name": "name", "gid": "gid", "system": "system"},
		required:     []string{"name"},
		namevar:      "name",
	},
	"exec": {
		// exec is the documented escape hatch, so it takes no ensure: there is
		// no desired state to converge on, only a command to run.
		puppetType: "exec",
		params: map[string]string{
			"command": "command", "creates": "creates",
			"chdir": "cwd", "shell": "",
		},
		required:    []string{"command"},
		namevar:     "command",
		refreshable: true,
		actions:     map[string]bool{"run": true},
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

// transform maps the target-agnostic tree onto native declarations and the
// notify metaparameters its notifies edges require. It does no ordering work:
// require metaparameters are the sort stage's job, because whether one is
// needed depends on what notify already implies, and that is only known once
// every resource has been mapped.
func transform(g *sdk.ResourceGraph) (*manifest, error) {
	class, err := className(g.Name)
	if err != nil {
		return nil, err
	}
	m := &manifest{Class: class, Host: g.Host}

	byID := make(map[string]resource, len(g.Resources))
	for _, r := range g.Resources {
		res, err := resourceFor(r)
		if err != nil {
			return nil, err
		}
		m.Resources = append(m.Resources, res)
		byID[r.ID] = res
	}

	notify, err := notifications(g, byID)
	if err != nil {
		return nil, err
	}
	for i := range m.Resources {
		m.Resources[i].Notify = notify[m.Resources[i].ResourceID]
	}
	return m, nil
}

// resourceFor maps one IR resource onto its native declaration.
func resourceFor(r sdk.Resource) (resource, error) {
	m, ok := mappings[r.Type]
	if !ok {
		return resource{}, errorf(r, "unsupported-resource-type",
			"unsupported resource type %q on this target (supported: %s)",
			r.Type, strings.Join(SupportedResourceTypes(), ", "))
	}

	// A catalog is compiled on the primary and applied later, so there is no
	// moment at which an apply-time expression could be evaluated. Compiling it
	// away would produce a manifest that does the wrong thing on every node the
	// condition was meant to exclude.
	if r.RuntimeWhen != "" {
		return resource{}, errorf(r, "unsupported-runtime-condition",
			"resource carries runtimeWhen %q, which this target cannot express: "+
				"a Puppet catalog is compiled before it reaches the node, so there is "+
				"no apply-time conditional to compile it into", r.RuntimeWhen)
	}

	attrs := make(map[string]any, len(r.Params)+2)
	for _, name := range sortedParamNames(r.Params) {
		native, ok := m.params[name]
		if !ok {
			return resource{}, errorf(r, "unsupported-param",
				"resource type %q does not accept param %q on this target (accepted: %s)",
				r.Type, name, strings.Join(sortedKeys(m.params), ", "))
		}
		if r.Params[name] == nil {
			return resource{}, errorf(r, "empty-param",
				"param %q on resource type %q has no value", name, r.Type)
		}
		if native == "" {
			// The param steers the mapping itself rather than becoming an
			// attribute: exec's shell selects Puppet's shell provider.
			if name == "shell" && r.Params[name] == true {
				attrs["provider"] = bareword("shell")
			}
			continue
		}
		attrs[native] = r.Params[name]
	}

	for _, need := range m.required {
		if _, ok := r.Params[need]; !ok {
			return resource{}, errorf(r, "missing-param",
				"resource type %q requires param %q on this target", r.Type, need)
		}
	}

	if err := applyEnsure(r, m, attrs); err != nil {
		return resource{}, err
	}

	return resource{
		Type:       m.puppetType,
		Title:      r.ID,
		Attrs:      attrs,
		ResourceID: r.ID,
		Pos:        r.Pos,
	}, nil
}

// applyEnsure maps the IR state vocabulary onto Puppet's ensure values. Every
// accepted value is listed in the mapping table, so an unrecognised one names
// the values that would have worked rather than failing generically.
func applyEnsure(r sdk.Resource, m typeMapping, attrs map[string]any) error {
	if len(m.ensure) == 0 {
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
	native, ok := m.ensure[state]
	if !ok {
		return errorf(r, "unsupported-state",
			"resource type %q does not accept state %q on this target (accepted: %s)",
			r.Type, state, strings.Join(sortedKeys(m.ensure), ", "))
	}
	attrs["ensure"] = bareword(native)
	return nil
}

// notifications returns, per notifying resource, the sorted references its
// declaration must list under notify.
//
// Puppet's notify does double duty: it refreshes the target and it orders the
// notifier before it. Only the refresh is checked here; the ordering it implies
// is the sort stage's concern.
func notifications(g *sdk.ResourceGraph, byID map[string]resource) (map[string][]reference, error) {
	out := make(map[string][]reference)
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
		from, ok := g.Resource(e.From)
		if !ok {
			return nil, &Error{
				Target: Name, Resource: e.From, Rule: "unknown-notifier", Pos: e.Pos,
				Msg: fmt.Sprintf("a notifies edge starts at resource %q, which this document does not declare", e.From),
			}
		}
		if err := checkAction(from, to, e); err != nil {
			return nil, err
		}

		ref := reference{Type: byID[e.To].Type, Title: e.To}
		key := ref.String()
		if seen[e.From] == nil {
			seen[e.From] = make(map[string]bool)
		}
		if seen[e.From][key] {
			// Two edges to one resource collapse onto a single notify: Puppet's
			// refresh happens once however many times it was triggered.
			continue
		}
		seen[e.From][key] = true
		out[e.From] = append(out[e.From], ref)
	}
	for id := range out {
		refs := out[id]
		sort.Slice(refs, func(i, j int) bool { return refs[i].String() < refs[j].String() })
	}
	return out, nil
}

// checkAction decides whether Puppet can perform one notify action on one
// resource.
//
// The failure is reported against the resource that declared the notifies edge,
// at the edge's own position, rather than against the resource being notified.
// The author wrote the action on the notifier, so that is the line they have to
// change; naming the notified resource instead would send them to a declaration
// that is perfectly fine on its own.
//
// A refusal says what Puppet would have done instead, because the author's
// alternative depends on which of the two it is.
func checkAction(from, to sdk.Resource, e sdk.Edge) error {
	pos := e.Pos
	if !pos.Known() {
		pos = from.Pos
	}
	fail := func(rule, format string, args ...any) error {
		return &Error{
			Target: Name, Resource: from.ID, Rule: rule, Pos: pos,
			Msg: fmt.Sprintf(format, args...),
		}
	}

	m, ok := mappings[to.Type]
	if !ok {
		return fail("unsupported-resource-type",
			"notifies resource %q, whose type %q this target does not support (supported: %s)",
			to.ID, to.Type, strings.Join(SupportedResourceTypes(), ", "))
	}
	if !m.refreshable {
		return fail("unsupported-action",
			"notifies resource %q, which this target cannot refresh: Puppet refreshes "+
				"only the types that define a refresh, and %s is not one of them",
			to.ID, m.puppetType)
	}
	if m.actions[e.Action] {
		return nil
	}
	if why, refused := m.refusedActions[e.Action]; refused {
		return fail("unsupported-action",
			"notifies resource %q with action %q, which this target cannot express: %s",
			to.ID, e.Action, why)
	}
	return fail("unsupported-action",
		"notifies resource %q with action %q, which resource type %q does not support "+
			"on this target (supported: %s)",
		to.ID, e.Action, to.Type, strings.Join(sortedBools(m.actions), ", "))
}

// className derives the manifest's class name from the document name. Puppet
// names a class with a restricted vocabulary, so a document name that cannot
// become one is refused rather than silently rewritten: a class whose name does
// not match the document is a class an operator cannot find.
func className(doc string) (string, error) {
	name := "meridian_" + doc
	for i, c := range name {
		ok := c == '_' || (c >= 'a' && c <= 'z') || (i > 0 && c >= '0' && c <= '9')
		if !ok {
			return "", &Error{
				Target: Name, Rule: "invalid-class-name",
				Msg: fmt.Sprintf("document name %q cannot become a Puppet class name: "+
					"%q is not allowed, and a class name may hold only lower-case "+
					"letters, digits and underscores", doc, string(c)),
			}
		}
	}
	return name, nil
}

// String renders a reference the way Puppet writes one: the type capitalised,
// the title quoted.
func (r reference) String() string {
	if r.Type == "" {
		return "['" + r.Title + "']"
	}
	return strings.ToUpper(r.Type[:1]) + r.Type[1:] + "['" + r.Title + "']"
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

func sortedBools(m map[string]bool) []string {
	out := make([]string, 0, len(m))
	for k := range m {
		out = append(out, k)
	}
	sort.Strings(out)
	return out
}
