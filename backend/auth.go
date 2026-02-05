package main

import (
	"context"
	"encoding/json"
	"net/http"
	"sync"
	"strings"
	"time"

	"github.com/dgraph-io/badger"
	"github.com/gin-gonic/gin"
	"github.com/google/uuid"
	"golang.org/x/oauth2"
)

var discordOAuthConfig *oauth2.Config
var loginChallenges = newLoginChallengeStore()

const loginChallengeTTL = 10 * time.Minute

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

func DiscordLoginHandler(cfg *Config) gin.HandlerFunc {
	return func(c *gin.Context) {
		redirect := c.Query("redirect")
		if redirect == "" {
			c.JSON(400, gin.H{"error": "missing redirect"})
			return
		}

		if cfg.LoginPasswordRequired() {
			password := c.Query("password")
			if password == "" || password != cfg.LoginPassword() {
				c.JSON(http.StatusUnauthorized, gin.H{"error": "invalid access password"})
				return
			}
		}

		nonce := loginChallenges.Create(loginChallengeTTL)
		state := nonce + "|" + redirect
		url := discordOAuthConfig.AuthCodeURL(state)
		c.Redirect(http.StatusTemporaryRedirect, url)
	}
}

func DiscordCallbackHandler(store *Store, cfg *Config) gin.HandlerFunc {
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
		nonce := parts[0]
		redirect := parts[1]
		if !loginChallenges.Consume(nonce) {
			c.JSON(http.StatusUnauthorized, gin.H{"error": "invalid login request"})
			return
		}

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

		sessionToken, err := store.CreateSession(discordUser.ID, cfg.SessionTTL())
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
		token, ok := getBearerToken(c)
		if !ok {
			c.Abort()
			return
		}

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
		userID, ok := getUserID(c)
		if !ok {
			return
		}

		user, err := store.GetUser(userID)
		if err != nil {
			c.JSON(500, gin.H{"error": "user lookup failed"})
			return
		}

		user.IsAdmin = cfg.IsAdmin(user.ID)
		c.JSON(200, user)
	}
}

func LogoutHandler(store *Store) gin.HandlerFunc {
	return func(c *gin.Context) {
		token, ok := getBearerToken(c)
		if !ok {
			return
		}
		if err := store.DeleteSession(token); err != nil {
			c.JSON(http.StatusInternalServerError, gin.H{"error": "logout failed"})
			return
		}
		c.JSON(http.StatusOK, gin.H{"status": "logged out"})
	}
}

func getBearerToken(c *gin.Context) (string, bool) {
	authHeader := c.GetHeader("Authorization")
	if authHeader == "" {
		c.JSON(http.StatusUnauthorized, gin.H{"error": "missing auth header"})
		c.Abort()
		return "", false
	}

	const prefix = "Bearer "
	if len(authHeader) < len(prefix) || authHeader[:len(prefix)] != prefix {
		c.JSON(http.StatusUnauthorized, gin.H{"error": "invalid auth header"})
		c.Abort()
		return "", false
	}

	return authHeader[len(prefix):], true
}

func getUserID(c *gin.Context) (string, bool) {
	value, ok := c.Get("user_id")
	if !ok {
		c.JSON(http.StatusUnauthorized, gin.H{"error": "missing user context"})
		c.Abort()
		return "", false
	}
	userID, ok := value.(string)
	if !ok || userID == "" {
		c.JSON(http.StatusUnauthorized, gin.H{"error": "invalid user context"})
		c.Abort()
		return "", false
	}
	return userID, true
}

func AdminMiddleware(cfg *Config) gin.HandlerFunc {
	return func(c *gin.Context) {
		userID, ok := getUserID(c)
		if !ok {
			return
		}
		if !cfg.IsAdmin(userID) {
			c.JSON(http.StatusForbidden, gin.H{"error": "admin access required"})
			c.Abort()
			return
		}
		c.Next()
	}
}

func ChatEnabledMiddleware(cfg *Config) gin.HandlerFunc {
	return func(c *gin.Context) {
		if !cfg.ChatEnabled() {
			c.JSON(http.StatusNotFound, gin.H{"error": "chat disabled"})
			c.Abort()
			return
		}
		c.Next()
	}
}

func (s *Store) CreateSession(userID string, ttl time.Duration) (string, error) {
	token := uuid.NewString()

	err := s.db.Update(func(txn *badger.Txn) error {
		entry := badger.NewEntry([]byte("session:"+token), []byte(userID))
		if ttl > 0 {
			entry = entry.WithTTL(ttl)
		}
		return txn.SetEntry(entry)
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

func (s *Store) DeleteSession(token string) error {
	return s.db.Update(func(txn *badger.Txn) error {
		return txn.Delete([]byte("session:" + token))
	})
}

type loginChallengeStore struct {
	mu    sync.Mutex
	items map[string]time.Time
}

func newLoginChallengeStore() *loginChallengeStore {
	return &loginChallengeStore{
		items: make(map[string]time.Time),
	}
}

func (s *loginChallengeStore) Create(ttl time.Duration) string {
	nonce := uuid.NewString()
	expiry := time.Now().Add(ttl)
	s.mu.Lock()
	defer s.mu.Unlock()
	s.cleanupLocked()
	s.items[nonce] = expiry
	return nonce
}

func (s *loginChallengeStore) Consume(nonce string) bool {
	if nonce == "" {
		return false
	}
	now := time.Now()
	s.mu.Lock()
	defer s.mu.Unlock()
	expiry, ok := s.items[nonce]
	if !ok {
		s.cleanupLocked()
		return false
	}
	delete(s.items, nonce)
	s.cleanupLocked()
	return expiry.After(now)
}

func (s *loginChallengeStore) cleanupLocked() {
	now := time.Now()
	for key, expiry := range s.items {
		if !expiry.After(now) {
			delete(s.items, key)
		}
	}
}
