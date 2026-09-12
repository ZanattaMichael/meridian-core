package ansible

import (
	"fmt"
	"strings"
)

// validate applies the constraints that are Ansible's rather than Meridian's,
// before anything is serialised. Every failure names the offending resource and
// the rule it broke, so the message points at the line the author must change.
func validate(pb *playbook) error {
	if pb.Hosts == "" {
		return &Error{Rule: "missing-host", Msg: "play has no target host"}
	}

	// Handler notification in Ansible is by task name, so a duplicate name makes
	// the playbook ambiguous rather than merely untidy.
	names := make(map[string]string, len(pb.Tasks))
	for _, t := range pb.Tasks {
		if err := checkName(t, "task"); err != nil {
			return err
		}
		if prev, dup := names[t.Name]; dup {
			return &Error{
				Resource: t.ResourceID, Rule: "duplicate-task-name", Pos: t.Pos,
				Msg: fmt.Sprintf("task name %q is already used by resource %q", t.Name, prev),
			}
		}
		names[t.Name] = t.ResourceID
	}

	handlerNames := make(map[string]bool, len(pb.Handlers))
	for _, h := range pb.Handlers {
		if err := checkName(h, "handler"); err != nil {
			return err
		}
		if handlerNames[h.Name] {
			return &Error{
				Resource: h.ResourceID, Rule: "duplicate-handler-name", Pos: h.Pos,
				Msg: fmt.Sprintf("handler name %q is declared twice", h.Name),
			}
		}
		handlerNames[h.Name] = true
		if h.When != "" {
			return &Error{
				Resource: h.ResourceID, Rule: "conditional-handler", Pos: h.Pos,
				Msg: fmt.Sprintf("handler %q carries a condition; a handler runs because "+
					"something changed, not because a condition holds", h.Name),
			}
		}
	}

	for _, t := range pb.Tasks {
		for _, n := range t.Notify {
			if !handlerNames[n] {
				return &Error{
					Resource: t.ResourceID, Rule: "missing-handler", Pos: t.Pos,
					Msg: fmt.Sprintf("notifies handler %q, which no handler block defines", n),
				}
			}
		}
		if t.Module == "" {
			return &Error{
				Resource: t.ResourceID, Rule: "missing-module", Pos: t.Pos,
				Msg: "task has no module",
			}
		}
		if len(t.Args) == 0 {
			return &Error{
				Resource: t.ResourceID, Rule: "empty-module-args", Pos: t.Pos,
				Msg: fmt.Sprintf("module %s was given no arguments", t.Module),
			}
		}
	}
	return nil
}

func checkName(t task, what string) error {
	switch {
	case strings.TrimSpace(t.Name) == "":
		return &Error{
			Resource: t.ResourceID, Rule: "empty-name", Pos: t.Pos,
			Msg: what + " has an empty name",
		}
	case strings.ContainsAny(t.Name, "\n\r"):
		return &Error{
			Resource: t.ResourceID, Rule: "multiline-name", Pos: t.Pos,
			Msg: fmt.Sprintf("%s name %q spans more than one line", what, t.Name),
		}
	}
	return nil
}
