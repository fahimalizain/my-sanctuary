package repository

import "errors"

// ErrNotFound is returned when a repository query finds no matching record.
// Handlers should use errors.Is(err, repository.ErrNotFound) to detect this.
var ErrNotFound = errors.New("repository: record not found")

// ErrConflict is returned when a repository upsert violates a unique
// constraint that the implementation cannot transparently resolve.
var ErrConflict = errors.New("repository: record conflict")