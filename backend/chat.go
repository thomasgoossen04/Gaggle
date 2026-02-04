package main

import (
	"encoding/json"
	"sort"
	"time"

	"github.com/dgraph-io/badger"
	"github.com/google/uuid"
)

type ChatMessage struct {
	ID        string `json:"id"`
	UserID    string `json:"user_id"`
	Username  string `json:"username"`
	Message   string `json:"message"`
	Timestamp int64  `json:"timestamp"`
}

func chatKey(ts int64, id string) []byte {
	return []byte("chat:" + formatTimestamp(ts) + ":" + id)
}

func formatTimestamp(ts int64) string {
	return time.UnixMilli(ts).UTC().Format("20060102150405.000000000")
}

func (s *Store) AddChatMessage(msg ChatMessage) error {
	return s.db.Update(func(txn *badger.Txn) error {
		data, err := json.Marshal(msg)
		if err != nil {
			return err
		}
		return txn.Set(chatKey(msg.Timestamp, msg.ID), data)
	})
}

func (s *Store) ListChatMessages(limit int) ([]ChatMessage, error) {
	messages := []ChatMessage{}

	err := s.db.View(func(txn *badger.Txn) error {
		it := txn.NewIterator(badger.DefaultIteratorOptions)
		defer it.Close()

		prefix := []byte("chat:")
		for it.Seek(prefix); it.ValidForPrefix(prefix); it.Next() {
			item := it.Item()
			err := item.Value(func(v []byte) error {
				var msg ChatMessage
				if err := json.Unmarshal(v, &msg); err != nil {
					return err
				}
				messages = append(messages, msg)
				return nil
			})
			if err != nil {
				return err
			}
		}
		return nil
	})

	if err != nil {
		return nil, err
	}

	sort.Slice(messages, func(i, j int) bool {
		return messages[i].Timestamp < messages[j].Timestamp
	})

	if limit > 0 && len(messages) > limit {
		messages = messages[len(messages)-limit:]
	}

	return messages, nil
}

func NewChatMessage(user *User, content string) ChatMessage {
	return ChatMessage{
		ID:        uuid.NewString(),
		UserID:    user.ID,
		Username:  user.Username,
		Message:   content,
		Timestamp: time.Now().UnixMilli(),
	}
}
