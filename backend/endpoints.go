package main

import (
	"context"
	"fmt"
	"log"
	"net/http"
	"os"
	"os/signal"
	"strconv"
	"time"

	"github.com/dgraph-io/badger"
	"github.com/gin-gonic/gin"
)

func StartServer(router *gin.Engine, store *Store, cfg *Config) {
	// Health check
	router.GET("/ping", func(c *gin.Context) {
		c.JSON(http.StatusOK, gin.H{
			"message": "pong",
		})
	})

	// Init OAuth
	InitDiscordOAuth(cfg)

	// OAuth routes
	auth := router.Group("/auth")
	{
		auth.GET("/discord/login", DiscordLoginHandler)
		auth.GET("/discord/callback", DiscordCallbackHandler(store, cfg))
		auth.POST("/logout", LogoutHandler(store))
	}

	// Feature flags
	router.GET("/features", func(c *gin.Context) {
		c.JSON(http.StatusOK, gin.H{
			"chat_enabled": cfg.Features.ChatEnabled,
		})
	})

	// Protected user routes
	users := router.Group("/users")
	users.Use(AuthMiddleware(store))
	{
		users.GET("/me", MeHandler(store, cfg))
		users.GET("/:id", store.getUserEp)
	}

	// Protected chat routes (optional)
	if cfg.Features.ChatEnabled {
		chat := router.Group("/chat")
		chat.Use(AuthMiddleware(store))
		{
			chat.GET("/messages", store.getChatMessagesEp)
			chat.POST("/messages", store.postChatMessageEp)
		}
	}

	// Admin routes
	admin := router.Group("/admin")
	admin.Use(AuthMiddleware(store), AdminMiddleware(cfg))
	{
		admin.GET("/stats", store.getAdminStatsEp)
		admin.DELETE("/chat/messages", store.clearChatMessagesEp)
		admin.DELETE("/chat/messages/:id", store.deleteChatMessageEp)
	}

	runServer(router, store, cfg)
}

func runServer(router *gin.Engine, store *Store, cfg *Config) {
	// Create HTTP server
	srv := &http.Server{
		Addr:    ":" + strconv.Itoa(cfg.Port),
		Handler: router,
	}

	// Run server in a goroutine
	go func() {
		fmt.Printf("Server running on port %d\n", cfg.Port)
		if err := srv.ListenAndServe(); err != nil && err != http.ErrServerClosed {
			log.Fatalf("listen: %s\n", err)
		}
	}()

	// Wait for interrupt signal (Ctrl+C)
	quit := make(chan os.Signal, 1)
	signal.Notify(quit, os.Interrupt)
	<-quit
	fmt.Println("\nShutting down server...")

	// Graceful shutdown with 5s timeout
	ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
	defer cancel()
	if err := srv.Shutdown(ctx); err != nil {
		log.Fatalf("Server forced to shutdown: %v", err)
	}

	// Close DB cleanly
	store.db.Close()
	fmt.Println("Server exited cleanly")
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

func (s *Store) getChatMessagesEp(c *gin.Context) {
	limit := 100
	messages, err := s.ListChatMessages(limit)
	if err != nil {
		c.JSON(http.StatusInternalServerError, gin.H{"error": "chat list failed"})
		return
	}
	c.JSON(http.StatusOK, gin.H{"messages": messages})
}

func (s *Store) postChatMessageEp(c *gin.Context) {
	userID := c.MustGet("user_id").(string)
	user, err := s.GetUser(userID)
	if err != nil {
		c.JSON(http.StatusInternalServerError, gin.H{"error": "user lookup failed"})
		return
	}

	var body struct {
		Message string `json:"message"`
	}
	if err := c.ShouldBindJSON(&body); err != nil || body.Message == "" {
		c.JSON(http.StatusBadRequest, gin.H{"error": "invalid message"})
		return
	}

	msg := NewChatMessage(user, body.Message)
	if err := s.AddChatMessage(msg); err != nil {
		c.JSON(http.StatusInternalServerError, gin.H{"error": "chat insert failed"})
		return
	}

	c.JSON(http.StatusOK, msg)
}

func (s *Store) getAdminStatsEp(c *gin.Context) {
	sessions, err := s.CountSessions()
	if err != nil {
		c.JSON(http.StatusInternalServerError, gin.H{"error": "session count failed"})
		return
	}

	c.JSON(http.StatusOK, gin.H{
		"sessions": sessions,
	})
}

func (s *Store) clearChatMessagesEp(c *gin.Context) {
	deleted, err := s.ClearChatMessages()
	if err != nil {
		c.JSON(http.StatusInternalServerError, gin.H{"error": "chat clear failed"})
		return
	}
	c.JSON(http.StatusOK, gin.H{"deleted": deleted})
}

func (s *Store) deleteChatMessageEp(c *gin.Context) {
	id := c.Param("id")
	if id == "" {
		c.JSON(http.StatusBadRequest, gin.H{"error": "missing id"})
		return
	}
	if err := s.DeleteChatMessage(id); err != nil {
		if err == badger.ErrKeyNotFound {
			c.JSON(http.StatusNotFound, gin.H{"error": "message not found"})
			return
		}
		c.JSON(http.StatusInternalServerError, gin.H{"error": "delete failed"})
		return
	}
	c.JSON(http.StatusOK, gin.H{"deleted": id})
}
