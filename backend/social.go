package main

import (
	"encoding/json"
	"sync"
	"time"

	"github.com/gorilla/websocket"
)

type SocialUser struct {
	UserID    string `json:"user_id"`
	Username  string `json:"username"`
	Status    string `json:"status"`
	AppID     string `json:"app_id,omitempty"`
	AppName   string `json:"app_name,omitempty"`
	UpdatedAt int64  `json:"updated_at"`
}

type SocialEvent struct {
	Type  string       `json:"type"`
	Users []SocialUser `json:"users,omitempty"`
}

type SocialStatusUpdate struct {
	UserID   string
	Username string
	Status   string
	AppID    string
	AppName  string
}

type SocialHub struct {
	mu         sync.Mutex
	clients    map[*websocket.Conn]*SocialClient
	userCounts map[string]int
	users      map[string]SocialUser
	register   chan SocialClient
	unregister chan *websocket.Conn
	status     chan SocialStatusUpdate
}

type SocialClient struct {
	Conn     *websocket.Conn
	UserID   string
	Username string
	Send     chan SocialEvent
}

func NewSocialHub() *SocialHub {
	h := &SocialHub{
		clients:    make(map[*websocket.Conn]*SocialClient),
		userCounts: make(map[string]int),
		users:      make(map[string]SocialUser),
		register:   make(chan SocialClient),
		unregister: make(chan *websocket.Conn),
		status:     make(chan SocialStatusUpdate, 32),
	}
	go h.run()
	return h
}

func (h *SocialHub) run() {
	for {
		select {
		case client := <-h.register:
			h.mu.Lock()
			h.clients[client.Conn] = &client
			h.userCounts[client.UserID]++
			if _, ok := h.users[client.UserID]; !ok {
				h.users[client.UserID] = SocialUser{
					UserID:    client.UserID,
					Username:  client.Username,
					Status:    "online",
					UpdatedAt: time.Now().Unix(),
				}
			}
			h.mu.Unlock()
			go h.writePump(&client)
			h.broadcastSnapshot()
		case conn := <-h.unregister:
			h.mu.Lock()
			if client, ok := h.clients[conn]; ok {
				delete(h.clients, conn)
				close(client.Send)
				_ = conn.Close()
				if h.userCounts[client.UserID] > 0 {
					h.userCounts[client.UserID]--
					if h.userCounts[client.UserID] == 0 {
						delete(h.userCounts, client.UserID)
						delete(h.users, client.UserID)
					}
				}
			}
			h.mu.Unlock()
			h.broadcastSnapshot()
		case update := <-h.status:
			h.applyStatus(update)
		}
	}
}

func (h *SocialHub) applyStatus(update SocialStatusUpdate) {
	h.mu.Lock()
	_, online := h.userCounts[update.UserID]
	if !online {
		h.mu.Unlock()
		return
	}
	entry := h.users[update.UserID]
	entry.UserID = update.UserID
	entry.Username = update.Username
	entry.Status = update.Status
	entry.AppID = update.AppID
	entry.AppName = update.AppName
	entry.UpdatedAt = time.Now().Unix()
	h.users[update.UserID] = entry
	h.mu.Unlock()
	h.broadcastSnapshot()
}

func (h *SocialHub) UpdateStatus(update SocialStatusUpdate) {
	h.status <- update
}

func (h *SocialHub) snapshot() []SocialUser {
	h.mu.Lock()
	defer h.mu.Unlock()
	users := make([]SocialUser, 0, len(h.users))
	for _, user := range h.users {
		users = append(users, user)
	}
	return users
}

func (h *SocialHub) broadcastSnapshot() {
	users := h.snapshot()
	event := SocialEvent{Type: "snapshot", Users: users}
	h.mu.Lock()
	for _, client := range h.clients {
		select {
		case client.Send <- event:
		default:
			close(client.Send)
			_ = client.Conn.Close()
			delete(h.clients, client.Conn)
		}
	}
	h.mu.Unlock()
}

func (h *SocialHub) writePump(client *SocialClient) {
	for event := range client.Send {
		if err := client.Conn.WriteJSON(event); err != nil {
			break
		}
	}
}

func decodeSocialStatus(data []byte) (SocialStatusUpdate, bool) {
	var payload struct {
		Type    string `json:"type"`
		Status  string `json:"status"`
		AppID   string `json:"app_id"`
		AppName string `json:"app_name"`
	}
	if err := json.Unmarshal(data, &payload); err != nil {
		return SocialStatusUpdate{}, false
	}
	if payload.Type != "status" || payload.Status == "" {
		return SocialStatusUpdate{}, false
	}
	return SocialStatusUpdate{
		Status:  payload.Status,
		AppID:   payload.AppID,
		AppName: payload.AppName,
	}, true
}
