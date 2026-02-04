package main

import (
	"context"
	"encoding/json"
	"net/http"
	"strings"

	"github.com/dgraph-io/badger"
	"github.com/gin-gonic/gin"
	"github.com/google/uuid"
	"golang.org/x/oauth2"
)

var discordOAuthConfig *oauth2.Config

func InitDiscordOAuth(cfg *Config) {
	discordOAuthConfig = &oauth2.Config{
		ClientID:     cfg.Discord.ClientID,
		ClientSecret: cfg.Discord.ClientSecret,
		RedirectURL:  cfg.Discord.RedirectURI,
		Scopes:       cfg.Discord.Scopes,
		Endpoint: oauth2.Endpoint{
			AuthURL:  "https://discord.com/api/oauth2/authorize",
			TokenURL: "https://discord.com/api/oauth2/token",
		},
	}
}

func DiscordLoginHandler(c *gin.Context) {
	redirect := c.Query("redirect")
	if redirect == "" {
		c.JSON(400, gin.H{"error": "missing redirect"})
		return
	}

	state := uuid.NewString() + "|" + redirect
	url := discordOAuthConfig.AuthCodeURL(state)
	c.Redirect(http.StatusTemporaryRedirect, url)
}

func DiscordCallbackHandler(store *Store) gin.HandlerFunc {
	return func(c *gin.Context) {

		code := c.Query("code")
		state := c.Query("state")

		if code == "" || state == "" {
			c.JSON(400, gin.H{"error": "missing code/state"})
			return
		}

		// Extract redirect from state
		parts := strings.SplitN(state, "|", 2)
		if len(parts) != 2 {
			c.JSON(400, gin.H{"error": "invalid state"})
			return
		}

		redirect := parts[1]

		token, err := discordOAuthConfig.Exchange(context.Background(), code)
		if err != nil {
			c.JSON(500, gin.H{"error": "token exchange failed"})
			return
		}

		client := discordOAuthConfig.Client(context.Background(), token)

		resp, err := client.Get("https://discord.com/api/users/@me")
		if err != nil {
			c.JSON(500, gin.H{"error": "discord fetch failed"})
			return
		}
		defer resp.Body.Close()

		var discordUser struct {
			ID       string `json:"id"`
			Username string `json:"username"`
		}

		json.NewDecoder(resp.Body).Decode(&discordUser)

		store.UpsertUser(User{
			ID:       discordUser.ID,
			Username: discordUser.Username,
		})

		sessionToken, err := store.CreateSession(discordUser.ID)
		if err != nil {
			c.JSON(500, gin.H{"error": "session create failed"})
			return
		}

		// Redirect browser → gui application
		c.Redirect(302, redirect+"?token="+sessionToken)
	}
}

func AuthMiddleware(store *Store) gin.HandlerFunc {
	return func(c *gin.Context) {
		authHeader := c.GetHeader("Authorization")
		if authHeader == "" {
			c.JSON(http.StatusUnauthorized, gin.H{"error": "missing auth header"})
			c.Abort()
			return
		}

		const prefix = "Bearer "
		if len(authHeader) < len(prefix) || authHeader[:len(prefix)] != prefix {
			c.JSON(http.StatusUnauthorized, gin.H{"error": "invalid auth header"})
			c.Abort()
			return
		}

		token := authHeader[len(prefix):]

		userID, err := store.GetUserFromSession(token)
		if err != nil {
			c.JSON(http.StatusUnauthorized, gin.H{"error": "invalid session"})
			c.Abort()
			return
		}

		c.Set("user_id", userID)
		c.Next()
	}
}

func MeHandler(store *Store, cfg *Config) gin.HandlerFunc {
	return func(c *gin.Context) {

		userID := c.MustGet("user_id").(string)

		user, err := store.GetUser(userID)
		if err != nil {
			c.JSON(500, gin.H{"error": "user lookup failed"})
			return
		}

		user.IsAdmin = isAdmin(cfg, user.ID)
		c.JSON(200, user)
	}
}

func isAdmin(cfg *Config, userID string) bool {
	for _, id := range cfg.Admins {
		if id == userID {
			return true
		}
	}
	return false
}

func AdminMiddleware(cfg *Config) gin.HandlerFunc {
	return func(c *gin.Context) {
		userID := c.MustGet("user_id").(string)
		if !isAdmin(cfg, userID) {
			c.JSON(http.StatusForbidden, gin.H{"error": "admin access required"})
			c.Abort()
			return
		}
		c.Next()
	}
}

func (s *Store) CreateSession(userID string) (string, error) {
	token := uuid.NewString()

	err := s.db.Update(func(txn *badger.Txn) error {
		return txn.Set([]byte("session:"+token), []byte(userID))
	})

	return token, err
}

func (s *Store) GetUserFromSession(token string) (string, error) {
	var userID string

	err := s.db.View(func(txn *badger.Txn) error {
		item, err := txn.Get([]byte("session:" + token))
		if err != nil {
			return err
		}
		return item.Value(func(val []byte) error {
			userID = string(val)
			return nil
		})
	})

	return userID, err
}
