package main

import (
	"net/http"

	"github.com/gin-gonic/gin"
)

func (s *Store) socialWsEp(c *gin.Context, hub *SocialHub) {
	if hub == nil {
		c.JSON(http.StatusNotFound, gin.H{"error": "social disabled"})
		return
	}

	token := c.Query("token")
	if token == "" {
		c.JSON(http.StatusUnauthorized, gin.H{"error": "missing token"})
		return
	}

	userID, err := s.GetUserFromSession(token)
	if err != nil {
		c.JSON(http.StatusUnauthorized, gin.H{"error": "invalid session"})
		return
	}

	conn, err := chatWsUpgrader.Upgrade(c.Writer, c.Request, nil)
	if err != nil {
		return
	}

	user, err := s.GetUser(userID)
	if err != nil {
		_ = conn.Close()
		return
	}

	client := SocialClient{
		Conn:     conn,
		UserID:   userID,
		Username: user.Username,
		Send:     make(chan SocialEvent, 16),
	}
	hub.register <- client

	go func() {
		defer func() {
			hub.unregister <- conn
		}()
		for {
			_, data, err := conn.ReadMessage()
			if err != nil {
				break
			}
			update, ok := decodeSocialStatus(data)
			if !ok {
				continue
			}
			update.UserID = userID
			update.Username = user.Username
			hub.UpdateStatus(update)
		}
	}()
}

func (s *Store) postSocialStatusEp(c *gin.Context, hub *SocialHub) {
	if hub == nil {
		c.JSON(http.StatusNotFound, gin.H{"error": "social disabled"})
		return
	}
	userID := c.MustGet("user_id").(string)
	user, err := s.GetUser(userID)
	if err != nil {
		c.JSON(http.StatusInternalServerError, gin.H{"error": "user lookup failed"})
		return
	}
	var body struct {
		Status  string `json:"status"`
		AppID   string `json:"app_id"`
		AppName string `json:"app_name"`
	}
	if err := c.ShouldBindJSON(&body); err != nil || body.Status == "" {
		c.JSON(http.StatusBadRequest, gin.H{"error": "invalid status payload"})
		return
	}
	hub.UpdateStatus(SocialStatusUpdate{
		UserID:   userID,
		Username: user.Username,
		Status:   body.Status,
		AppID:    body.AppID,
		AppName:  body.AppName,
	})
	c.JSON(http.StatusOK, gin.H{"status": "ok"})
}
