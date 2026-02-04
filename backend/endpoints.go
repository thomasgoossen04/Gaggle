package main

import (
	"context"
	"fmt"
	"log"
	"net/http"
	"os"
	"os/signal"
	"strconv"
	"sync"
	"time"

	"github.com/dgraph-io/badger"
	"github.com/gin-gonic/gin"
	"github.com/gorilla/websocket"
)

var chatHub *ChatHub
var httpServer *http.Server
var shutdownOnce sync.Once

var chatWsUpgrader = websocket.Upgrader{
	CheckOrigin: func(r *http.Request) bool { return true },
}

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
			"chat_enabled": cfg.ChatEnabled(),
		})
	})

	// Theme (optional)
	router.GET("/theme", func(c *gin.Context) {
		theme := cfg.GetTheme()
		if theme == nil {
			c.Status(http.StatusNoContent)
			return
		}
		c.JSON(http.StatusOK, theme)
	})

	// Protected user routes
	users := router.Group("/users")
	users.Use(AuthMiddleware(store))
	{
		users.GET("/me", MeHandler(store, cfg))
		users.GET("/:id", store.getUserEp)
	}

	// Protected chat routes (optional)
	if cfg.ChatEnabled() {
		chatHub = NewChatHub()
	}
	chat := router.Group("/chat")
	chat.Use(ChatEnabledMiddleware(cfg))
	{
		chat.GET("/ws", func(c *gin.Context) {
			store.chatWsEp(c, chatHub)
		})
	}

	chatAuth := router.Group("/chat")
	chatAuth.Use(AuthMiddleware(store), ChatEnabledMiddleware(cfg))
	{
		chatAuth.GET("/messages", store.getChatMessagesEp)
		chatAuth.POST("/messages", func(c *gin.Context) {
			store.postChatMessageEp(c, chatHub)
		})
	}

	// Admin routes
	admin := router.Group("/admin")
	admin.Use(AuthMiddleware(store), AdminMiddleware(cfg))
	{
		admin.GET("/stats", store.getAdminStatsEp)
		admin.DELETE("/chat/messages", store.clearChatMessagesEp)
		admin.DELETE("/chat/messages/:id", store.deleteChatMessageEp)
		admin.POST("/restart", func(c *gin.Context) {
			c.JSON(http.StatusOK, gin.H{"status": "restarting"})
			go gracefulShutdown(store)
		})
		admin.POST("/reload-config", func(c *gin.Context) {
			if err := cfg.Reload(); err != nil {
				c.JSON(http.StatusInternalServerError, gin.H{"error": "reload failed"})
				return
			}
			c.JSON(http.StatusOK, gin.H{"status": "reloaded"})
		})
	}

	runServer(router, store, cfg)
}

func runServer(router *gin.Engine, store *Store, cfg *Config) {
	// Create HTTP server
	srv := &http.Server{
		Addr:    ":" + strconv.Itoa(cfg.Port),
		Handler: router,
	}
	httpServer = srv

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
	gracefulShutdown(store)
}

func gracefulShutdown(store *Store) {
	shutdownOnce.Do(func() {
		// Graceful shutdown with 5s timeout
		ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
		defer cancel()
		if httpServer != nil {
			if err := httpServer.Shutdown(ctx); err != nil {
				log.Printf("Server forced to shutdown: %v", err)
			}
		}

		// Close DB cleanly
		store.db.Close()
		log.Println("Server exited cleanly")

		// Exit process (supervisor should restart)
		os.Exit(0)
	})
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

func (s *Store) postChatMessageEp(c *gin.Context, chatHub *ChatHub) {
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

	if chatHub != nil {
		chatHub.broadcast <- ChatEvent{Type: "message", Message: &msg}
	}

	c.JSON(http.StatusOK, msg)
}

func (s *Store) chatWsEp(c *gin.Context, chatHub *ChatHub) {
	if chatHub == nil {
		c.JSON(http.StatusNotFound, gin.H{"error": "chat disabled"})
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

	client := ChatClient{
		Conn:     conn,
		UserID:   userID,
		Username: user.Username,
		Send:     make(chan ChatEvent, 16),
	}
	chatHub.register <- client

	if messages, err := s.ListChatMessages(100); err == nil {
		client.Send <- ChatEvent{Type: "snapshot", Messages: messages}
	}

	go func() {
		defer func() {
			chatHub.unregister <- conn
		}()
		for {
			if _, _, err := conn.ReadMessage(); err != nil {
				break
			}
		}
	}()
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
	if chatHub != nil {
		chatHub.broadcast <- ChatEvent{Type: "clear"}
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
	if chatHub != nil {
		chatHub.broadcast <- ChatEvent{Type: "delete", DeletedID: id}
	}
	c.JSON(http.StatusOK, gin.H{"deleted": id})
}
