package main

import (
	"fmt"
	"log"
	"os"
	"sync"
	"time"

	"github.com/BurntSushi/toml"
)

type Config struct {
	Port    int           `toml:"port" json:"port"`
	Mode    string        `toml:"mode" json:"mode"`
	Discord DiscordConfig `toml:"discord" json:"discord"`
	Features Features     `toml:"features" json:"features"`
	Access  AccessConfig  `toml:"access" json:"access"`
	Admins  []string      `toml:"admins" json:"admins"`
	Session SessionConfig `toml:"session" json:"session"`
	Theme   *ThemeConfig  `toml:"theme" json:"theme"`
	mu      sync.RWMutex
}

type DiscordConfig struct {
	ClientID     string   `toml:"client_id" json:"client_id"`
	ClientSecret string   `toml:"client_secret" json:"client_secret"`
	RedirectURI  string   `toml:"redirect_uri" json:"redirect_uri"`
	Scopes       []string `toml:"scopes" json:"scopes"`
}

type Features struct {
	ChatEnabled bool `toml:"chat_enabled" json:"chat_enabled"`
}

type AccessConfig struct {
	Password string `toml:"password" json:"password"`
}

type SessionConfig struct {
	TTLHours int `toml:"ttl_hours" json:"ttl_hours"`
}

type ThemeConfig struct {
	Primary   string `toml:"primary" json:"primary"`
	Secondary string `toml:"secondary" json:"secondary"`
	Accent    string `toml:"accent" json:"accent"`
	Ink       string `toml:"ink" json:"ink"`
	InkLight  string `toml:"ink_light" json:"ink_light"`
	Font      string `toml:"font" json:"font"`
	Radius    string `toml:"radius" json:"radius"`
}

const configPath = "./config.toml"

func LoadConfig() (*Config, error) {
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
	c.Access = next.Access
	c.Admins = next.Admins
	c.Session = next.Session
	c.Theme = next.Theme
	return nil
}

func (c *Config) Snapshot() Config {
	c.mu.RLock()
	defer c.mu.RUnlock()

	discord := c.Discord
	discord.Scopes = append([]string{}, c.Discord.Scopes...)
	admins := append([]string{}, c.Admins...)
	var themeCopy *ThemeConfig
	if c.Theme != nil {
		tmp := *c.Theme
		themeCopy = &tmp
	}

	return Config{
		Port:     c.Port,
		Mode:     c.Mode,
		Discord:  discord,
		Features: c.Features,
		Access:   AccessConfig{},
		Admins:   admins,
		Session:  c.Session,
		Theme:    themeCopy,
	}
}

func SaveConfig(cfg *Config) error {
	tmpPath := configPath + ".tmp"
	file, err := os.Create(tmpPath)
	if err != nil {
		return fmt.Errorf("Failed to write config file %w", err)
	}
	encoder := toml.NewEncoder(file)
	if err := encoder.Encode(cfg); err != nil {
		_ = file.Close()
		_ = os.Remove(tmpPath)
		return fmt.Errorf("Failed to encode config file %w", err)
	}
	if err := file.Sync(); err != nil {
		_ = file.Close()
		_ = os.Remove(tmpPath)
		return fmt.Errorf("Failed to sync config file %w", err)
	}
	if err := file.Close(); err != nil {
		_ = os.Remove(tmpPath)
		return fmt.Errorf("Failed to close config file %w", err)
	}
	if err := os.Rename(tmpPath, configPath); err != nil {
		_ = os.Remove(tmpPath)
		return fmt.Errorf("Failed to replace config file %w", err)
	}
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

func (c *Config) LoginPasswordRequired() bool {
	c.mu.RLock()
	defer c.mu.RUnlock()
	return c.Access.Password != ""
}

func (c *Config) LoginPassword() string {
	c.mu.RLock()
	defer c.mu.RUnlock()
	return c.Access.Password
}

func (c *Config) SessionTTL() time.Duration {
	c.mu.RLock()
	defer c.mu.RUnlock()
	if c.Session.TTLHours <= 0 {
		return 0
	}
	return time.Duration(c.Session.TTLHours) * time.Hour
}
