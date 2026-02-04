package main

import (
	"fmt"
	"log"
	"sync"
	"time"

	"github.com/BurntSushi/toml"
)

type Config struct {
	Port    int           `toml:"port"`
	Mode    string        `toml:"mode"`
	Discord DiscordConfig `toml:"discord"`
	Features Features     `toml:"features"`
	Admins  []string      `toml:"admins"`
	Session SessionConfig `toml:"session"`
	Theme   *ThemeConfig  `toml:"theme"`
	mu      sync.RWMutex
}

type DiscordConfig struct {
	ClientID     string   `toml:"client_id"`
	ClientSecret string   `toml:"client_secret"`
	RedirectURI  string   `toml:"redirect_uri"`
	Scopes       []string `toml:"scopes"`
}

type Features struct {
	ChatEnabled bool `toml:"chat_enabled"`
}

type SessionConfig struct {
	TTLHours int `toml:"ttl_hours"`
}

type ThemeConfig struct {
	Primary   string `toml:"primary" json:"primary"`
	Secondary string `toml:"secondary" json:"secondary"`
	Accent    string `toml:"accent" json:"accent"`
	Ink       string `toml:"ink" json:"ink"`
	InkLight  string `toml:"ink_light" json:"ink_light"`
	Font      string `toml:"font" json:"font"`
}

func LoadConfig() (*Config, error) {
	configPath := "./config.toml"
	var cfg Config
	if _, err := toml.DecodeFile(configPath, &cfg); err != nil {
		return nil, fmt.Errorf("Failed to read config file %w", err)
	}

	return &cfg, nil
}

func MustLoadConfig() *Config {
	cfg, err := LoadConfig()
	if err != nil {
		log.Fatalf("config load failed: %v", err)
	}
	return cfg
}

func (c *Config) Reload() error {
	next, err := LoadConfig()
	if err != nil {
		return err
	}

	c.mu.Lock()
	defer c.mu.Unlock()

	c.Port = next.Port
	c.Mode = next.Mode
	c.Discord = next.Discord
	c.Features = next.Features
	c.Admins = next.Admins
	c.Session = next.Session
	c.Theme = next.Theme
	return nil
}

func (c *Config) IsAdmin(userID string) bool {
	c.mu.RLock()
	defer c.mu.RUnlock()
	for _, id := range c.Admins {
		if id == userID {
			return true
		}
	}
	return false
}

func (c *Config) GetTheme() *ThemeConfig {
	c.mu.RLock()
	defer c.mu.RUnlock()
	return c.Theme
}

func (c *Config) ChatEnabled() bool {
	c.mu.RLock()
	defer c.mu.RUnlock()
	return c.Features.ChatEnabled
}

func (c *Config) SessionTTL() time.Duration {
	c.mu.RLock()
	defer c.mu.RUnlock()
	if c.Session.TTLHours <= 0 {
		return 0
	}
	return time.Duration(c.Session.TTLHours) * time.Hour
}
