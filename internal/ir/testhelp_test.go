package ir

import "errors"

// asError is errors.As with the argument order these tests read best in.
func asError(err error, target **Error) bool {
	var list ErrorList
	if errors.As(err, &list) && len(list) > 0 {
		*target = list[0]
		return true
	}
	return errors.As(err, target)
}
