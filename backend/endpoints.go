package main

import (
	"net/http"
	"strconv"

	"github.com/gin-gonic/gin"
)

func StartServer(router *gin.Engine, store *Store, cfg *Config) {
	router.GET("/ping", func(c *gin.Context) {
		c.JSON(http.StatusOK, gin.H{
			"message": "pong",
		})
	})

	users := router.Group("/users")
	{
		users.GET("/:id", store.getUserEp)
	}

	router.Run(":" + strconv.Itoa(cfg.Port))
}

func (s *Store) getUserEp(c *gin.Context) {
	id := c.Param("id")
	user, err := s.GetUser(id)
	if err != nil {
		c.JSON(http.StatusNotFound, gin.H{"error": "user not found"})
		return
	}

	c.JSON(http.StatusOK, user)
}
