package main

import (
	"fmt"
	"os"
	"testing"
	"time"

	"github.com/dgraph-io/badger"
)

func newTestStore(t *testing.T) *Store {
	t.Helper()

	dir, err := os.MkdirTemp("", "gaggle-badger-*")
	if err != nil {
		t.Fatalf("failed to create temp dir: %v", err)
	}
	opts := badger.DefaultOptions(dir).WithLogger(nil)
	opts.ValueDir = dir
	db, err := badger.Open(opts)
	if err != nil {
		t.Fatalf("failed to open badger: %v", err)
	}
	t.Cleanup(func() {
		_ = db.Close()
		_ = os.RemoveAll(dir)
	})

	return &Store{db: db}
}

func TestChatAddListOrder(t *testing.T) {
	store := newTestStore(t)

	msg1 := ChatMessage{
		ID:        "m1",
		UserID:    "u1",
		Username:  "alice",
		Message:   "first",
		Timestamp: time.Now().Add(2 * time.Second).UnixMilli(),
	}
	msg2 := ChatMessage{
		ID:        "m2",
		UserID:    "u2",
		Username:  "bob",
		Message:   "second",
		Timestamp: time.Now().Add(1 * time.Second).UnixMilli(),
	}

	if err := store.AddChatMessage(msg1); err != nil {
		t.Fatalf("AddChatMessage failed: %v", err)
	}
	if err := store.AddChatMessage(msg2); err != nil {
		t.Fatalf("AddChatMessage failed: %v", err)
	}

	list, err := store.ListChatMessages(10)
	if err != nil {
		t.Fatalf("ListChatMessages failed: %v", err)
	}
	if len(list) != 2 {
		t.Fatalf("expected 2 messages, got %d", len(list))
	}
	if list[0].ID != "m2" || list[1].ID != "m1" {
		t.Fatalf("messages not sorted by timestamp: got %v then %v", list[0].ID, list[1].ID)
	}
}

func TestChatLimit(t *testing.T) {
	store := newTestStore(t)

	for i := 0; i < 5; i++ {
		msg := ChatMessage{
			ID:        fmt.Sprintf("m%d", i),
			UserID:    "u",
			Username:  "user",
			Message:   "msg",
			Timestamp: time.Now().Add(time.Duration(i) * time.Second).UnixMilli(),
		}
		if err := store.AddChatMessage(msg); err != nil {
			t.Fatalf("AddChatMessage failed: %v", err)
		}
	}

	list, err := store.ListChatMessages(2)
	if err != nil {
		t.Fatalf("ListChatMessages failed: %v", err)
	}
	if len(list) != 2 {
		t.Fatalf("expected 2 messages, got %d", len(list))
	}
}

func TestChatClear(t *testing.T) {
	store := newTestStore(t)

	for i := 0; i < 3; i++ {
		msg := ChatMessage{
			ID:        fmt.Sprintf("c%d", i),
			UserID:    "u",
			Username:  "user",
			Message:   "msg",
			Timestamp: time.Now().UnixMilli(),
		}
		if err := store.AddChatMessage(msg); err != nil {
			t.Fatalf("AddChatMessage failed: %v", err)
		}
	}

	deleted, err := store.ClearChatMessages()
	if err != nil {
		t.Fatalf("ClearChatMessages failed: %v", err)
	}
	if deleted != 3 {
		t.Fatalf("expected 3 deletions, got %d", deleted)
	}

	list, err := store.ListChatMessages(10)
	if err != nil {
		t.Fatalf("ListChatMessages failed: %v", err)
	}
	if len(list) != 0 {
		t.Fatalf("expected 0 messages after clear, got %d", len(list))
	}
}

func TestChatDeleteMessage(t *testing.T) {
	store := newTestStore(t)

	msg1 := ChatMessage{
		ID:        "d1",
		UserID:    "u1",
		Username:  "alice",
		Message:   "keep",
		Timestamp: time.Now().UnixMilli(),
	}
	msg2 := ChatMessage{
		ID:        "d2",
		UserID:    "u2",
		Username:  "bob",
		Message:   "delete",
		Timestamp: time.Now().Add(1 * time.Second).UnixMilli(),
	}
	if err := store.AddChatMessage(msg1); err != nil {
		t.Fatalf("AddChatMessage failed: %v", err)
	}
	if err := store.AddChatMessage(msg2); err != nil {
		t.Fatalf("AddChatMessage failed: %v", err)
	}

	if err := store.DeleteChatMessage("d2"); err != nil {
		t.Fatalf("DeleteChatMessage failed: %v", err)
	}

	list, err := store.ListChatMessages(10)
	if err != nil {
		t.Fatalf("ListChatMessages failed: %v", err)
	}
	if len(list) != 1 || list[0].ID != "d1" {
		t.Fatalf("expected only d1 remaining, got %+v", list)
	}
}

func TestChatDeleteMissingMessage(t *testing.T) {
	store := newTestStore(t)

	err := store.DeleteChatMessage("missing")
	if err == nil {
		t.Fatalf("expected error deleting missing message")
	}
	if err != badger.ErrKeyNotFound {
		t.Fatalf("expected ErrKeyNotFound, got %v", err)
	}
}
