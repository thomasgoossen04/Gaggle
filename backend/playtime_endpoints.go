package main

import (
	"net/http"
	"time"

	"github.com/gin-gonic/gin"
)

func (s *Store) getPlaytimeEp(c *gin.Context) {
	userID, ok := getUserID(c)
	if !ok {
		return
	}
	items, err := s.ListPlaytime(userID)
	if err != nil {
		c.JSON(http.StatusInternalServerError, gin.H{"error": "playtime list failed"})
		return
	}
	c.JSON(http.StatusOK, items)
}

func (s *Store) postPlaytimeEp(c *gin.Context) {
	userID, ok := getUserID(c)
	if !ok {
		return
	}
	appID := c.Param("id")
	if !isSafeAppID(appID) {
		c.JSON(http.StatusBadRequest, gin.H{"error": "invalid app id"})
		return
	}
	var body struct {
		Seconds int64 `json:"seconds"`
		EndedAt int64 `json:"ended_at"`
	}
	if err := c.ShouldBindJSON(&body); err != nil || body.Seconds <= 0 {
		c.JSON(http.StatusBadRequest, gin.H{"error": "invalid playtime payload"})
		return
	}

	endedAt := time.Now()
	if body.EndedAt > 0 {
		endedAt = time.Unix(body.EndedAt, 0)
	}
	entry, err := s.AddPlaytime(userID, appID, body.Seconds, endedAt)
	if err != nil {
		c.JSON(http.StatusInternalServerError, gin.H{"error": "playtime update failed"})
		return
	}
	c.JSON(http.StatusOK, entry)
}
