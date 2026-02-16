package main

import (
	"encoding/json"
	"sort"
	"time"

	"github.com/dgraph-io/badger"
	"github.com/google/uuid"
	"github.com/gorilla/websocket"
)

type ChatMessage struct {
	ID        string `json:"id"`
	UserID    string `json:"user_id"`
	Username  string `json:"username"`
	Message   string `json:"message"`
	Timestamp int64  `json:"timestamp"`
}

type ChatEvent struct {
	Type      string        `json:"type"`
	Message   *ChatMessage  `json:"message,omitempty"`
	Messages  []ChatMessage `json:"messages"`
	DeletedID string        `json:"deleted_id,omitempty"`
	Users     []string      `json:"users,omitempty"`
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

type ChatHub struct {
	clients    map[*websocket.Conn]*ChatClient
	register   chan ChatClient
	unregister chan *websocket.Conn
	broadcast  chan ChatEvent
}

type ChatClient struct {
	Conn     *websocket.Conn
	UserID   string
	Username string
	Send     chan ChatEvent
}

func NewChatHub() *ChatHub {
	h := &ChatHub{
		clients:    make(map[*websocket.Conn]*ChatClient),
		register:   make(chan ChatClient),
		unregister: make(chan *websocket.Conn),
		broadcast:  make(chan ChatEvent, 32),
	}
	go h.run()
	return h
}

func (h *ChatHub) run() {
	for {
		select {
		case client := <-h.register:
			h.clients[client.Conn] = &client
			go h.writePump(&client)
			h.broadcastPresence()
		case conn := <-h.unregister:
			if client, ok := h.clients[conn]; ok {
				delete(h.clients, conn)
				close(client.Send)
				_ = conn.Close()
				h.broadcastPresence()
			}
		case event := <-h.broadcast:
			for _, client := range h.clients {
				select {
				case client.Send <- event:
				default:
					close(client.Send)
					_ = client.Conn.Close()
					delete(h.clients, client.Conn)
				}
			}
		}
	}
}

func (h *ChatHub) broadcastPresence() {
	users := make([]string, 0, len(h.clients))
	seen := make(map[string]struct{})

	// Prevent duplicates
	for _, client := range h.clients {
		if _, ok := seen[client.UserID]; ok {
			continue
		}
		seen[client.UserID] = struct{}{}
		users = append(users, client.Username)
	}

	event := ChatEvent{Type: "presence", Users: users}
	for _, client := range h.clients {
		select {
		case client.Send <- event:
		default:
			close(client.Send)
			_ = client.Conn.Close()
			delete(h.clients, client.Conn)
		}
	}
}

func (h *ChatHub) writePump(client *ChatClient) {
	for event := range client.Send {
		if err := client.Conn.WriteJSON(event); err != nil {
			break
		}
	}
}

func (s *Store) DeleteChatMessage(id string) error {
	return s.db.Update(func(txn *badger.Txn) error {
		it := txn.NewIterator(badger.DefaultIteratorOptions)
		defer it.Close()

		prefix := []byte("chat:")
		for it.Seek(prefix); it.ValidForPrefix(prefix); it.Next() {
			item := it.Item()
			var msg ChatMessage
			err := item.Value(func(v []byte) error {
				return json.Unmarshal(v, &msg)
			})
			if err != nil {
				return err
			}
			if msg.ID == id {
				return txn.Delete(item.KeyCopy(nil))
			}
		}
		return badger.ErrKeyNotFound
	})
}

func (s *Store) ClearChatMessages() (int, error) {
	deleted := 0
	err := s.db.Update(func(txn *badger.Txn) error {
		it := txn.NewIterator(badger.DefaultIteratorOptions)
		defer it.Close()

		prefix := []byte("chat:")
		for it.Seek(prefix); it.ValidForPrefix(prefix); it.Next() {
			key := it.Item().KeyCopy(nil)
			if err := txn.Delete(key); err != nil {
				return err
			}
			deleted++
		}
		return nil
	})
	return deleted, err
}
