package main

import (
	"context"
	"encoding/json"
	"net/http"

	"github.com/gin-gonic/gin"
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
	url := discordOAuthConfig.AuthCodeURL("state", oauth2.AccessTypeOffline)
	c.Redirect(http.StatusTemporaryRedirect, url)
}

func DiscordCallbackHandler(store *Store) gin.HandlerFunc {
	return func(c *gin.Context) {
		code := c.Query("code")
		if code == "" {
			c.JSON(http.StatusBadRequest, gin.H{"error": "code not provided"})
			return
		}

		token, err := discordOAuthConfig.Exchange(context.Background(), code)
		if err != nil {
			c.JSON(http.StatusInternalServerError, gin.H{"error": "failed to exchange token"})
			return
		}

		// Fetch user info from Discord
		client := discordOAuthConfig.Client(context.Background(), token)
		resp, err := client.Get("https://discord.com/api/users/@me")
		if err != nil {
			c.JSON(http.StatusInternalServerError, gin.H{"error": "failed to get user info"})
			return
		}
		defer resp.Body.Close()

		var discordUser struct {
			ID       string `json:"id"`
			Username string `json:"username"`
		}

		if err := json.NewDecoder(resp.Body).Decode(&discordUser); err != nil {
			c.JSON(http.StatusInternalServerError, gin.H{"error": "failed to decode user info"})
			return
		}

		// Upsert user into DB
		store.UpsertUser(User{
			ID:       discordUser.ID,
			Username: discordUser.Username,
		})

		c.JSON(http.StatusOK, gin.H{
			"message": "logged in successfully",
			"user":    discordUser,
		})
	}
}
