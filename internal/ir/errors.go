package ir

import (
	"fmt"
	"sort"
	"strings"
)

// Position identifies a point inside a source document. Line and Column are
// 1-based; a zero Line means the position is unknown.
type Position struct {
	File   string
	DocIdx int
	Line   int
	Column int
}

func (p Position) String() string {
	loc := p.File
	if loc == "" {
		loc = "<input>"
	}
	if p.DocIdx > 0 {
		loc = fmt.Sprintf("%s[doc %d]", loc, p.DocIdx)
	}
	if p.Line > 0 {
		loc = fmt.Sprintf("%s:%d", loc, p.Line)
		if p.Column > 0 {
			loc = fmt.Sprintf("%s:%d", loc, p.Column)
		}
	}
	return loc
}

// Error is a single located diagnostic. Every failure surfaced by this package
// is an Error or an ErrorList of them; parsing never panics on malformed input.
type Error struct {
	Pos  Position
	Msg  string
	Path string // dotted path to the offending field, when known
	Err  error
}

func (e *Error) Error() string {
	var b strings.Builder
	b.WriteString(e.Pos.String())
	b.WriteString(": ")
	if e.Path != "" {
		b.WriteString(e.Path)
		b.WriteString(": ")
	}
	b.WriteString(e.Msg)
	return b.String()
}

func (e *Error) Unwrap() error { return e.Err }

func errorf(pos Position, path, format string, args ...any) *Error {
	return &Error{Pos: pos, Path: path, Msg: fmt.Sprintf(format, args...)}
}

// ErrorList aggregates diagnostics so a single parse or validation pass can
// report every problem it found rather than only the first.
type ErrorList []*Error

func (l ErrorList) Error() string {
	switch len(l) {
	case 0:
		return "no errors"
	case 1:
		return l[0].Error()
	}
	parts := make([]string, 0, len(l))
	for _, e := range l {
		parts = append(parts, e.Error())
	}
	return fmt.Sprintf("%d errors:\n  %s", len(l), strings.Join(parts, "\n  "))
}

func (l *ErrorList) add(e *Error) {
	if e != nil {
		*l = append(*l, e)
	}
}

// sortStable orders diagnostics by source position so repeated runs over the
// same input report them in the same order.
func (l ErrorList) sortStable() {
	sort.SliceStable(l, func(i, j int) bool {
		a, b := l[i].Pos, l[j].Pos
		if a.File != b.File {
			return a.File < b.File
		}
		if a.DocIdx != b.DocIdx {
			return a.DocIdx < b.DocIdx
		}
		if a.Line != b.Line {
			return a.Line < b.Line
		}
		return a.Column < b.Column
	})
}

// err returns nil when the list is empty, so callers can return it directly.
func (l ErrorList) err() error {
	if len(l) == 0 {
		return nil
	}
	l.sortStable()
	return l
}
