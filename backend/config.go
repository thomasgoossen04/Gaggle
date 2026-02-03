package main

import (
	"fmt"
	"log"

	"github.com/BurntSushi/toml"
)

type Config struct {
	Port    int           `toml:"port"`
	Mode    string        `toml:"mode"`
	Discord DiscordConfig `toml:"discord"`
}

type DiscordConfig struct {
	ClientID     string   `toml:"client_id"`
	ClientSecret string   `toml:"client_secret"`
	RedirectURI  string   `toml:"redirect_uri"`
	Scopes       []string `toml:"scopes"`
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
