package main

import (
	"encoding/json"
	"errors"
	"flag"
	"fmt"
	"os"
	"os/exec"
	"path/filepath"
	"strings"
)

const defaultConfigPath = "frontend/src-tauri/tauri.conf.json"

func main() {
	var title string
	var iconsRaw string
	var configPath string
	var skipBuild bool

	flag.StringVar(&title, "title", "", "Window title to set in tauri.conf.json")
	flag.StringVar(&iconsRaw, "icons", "", "Comma-separated list of icon paths (relative to frontend/src-tauri)")
	flag.StringVar(&configPath, "config", defaultConfigPath, "Path to tauri.conf.json")
	flag.BoolVar(&skipBuild, "skip-build", false, "Update config only; skip cargo tauri build")
	flag.Parse()

	if strings.TrimSpace(title) == "" && strings.TrimSpace(iconsRaw) == "" {
		exitf("nothing to update: provide --title and/or --icons")
	}

	absConfig, err := filepath.Abs(configPath)
	if err != nil {
		exitf("failed to resolve config path: %v", err)
	}

	data, err := os.ReadFile(absConfig)
	if err != nil {
		exitf("failed to read config: %v", err)
	}

	var root map[string]any
	if err := json.Unmarshal(data, &root); err != nil {
		exitf("failed to parse config: %v", err)
	}

	configDir := filepath.Dir(absConfig)

	if strings.TrimSpace(title) != "" {
		if err := setWindowTitle(root, title); err != nil {
			exitf("failed to set title: %v", err)
		}
	}

	if strings.TrimSpace(iconsRaw) != "" {
		icons := splitIcons(iconsRaw)
		if err := validateIcons(configDir, icons); err != nil {
			exitf("icon validation failed: %v", err)
		}
		if err := setIcons(root, icons); err != nil {
			exitf("failed to set icons: %v", err)
		}
	}

	out, err := json.MarshalIndent(root, "", "    ")
	if err != nil {
		exitf("failed to write config: %v", err)
	}
	out = append(out, '\n')
	if err := os.WriteFile(absConfig, out, 0644); err != nil {
		exitf("failed to save config: %v", err)
	}

	fmt.Printf("Updated %s\n", absConfig)

	if skipBuild {
		return
	}

	frontendDir := filepath.Clean(filepath.Join(configDir, ".."))
	cmd := exec.Command("cargo", "tauri", "build")
	cmd.Dir = frontendDir
	cmd.Stdout = os.Stdout
	cmd.Stderr = os.Stderr
	cmd.Stdin = os.Stdin

	if err := cmd.Run(); err != nil {
		exitf("build failed: %v", err)
	}
}

func splitIcons(raw string) []string {
	parts := strings.Split(raw, ",")
	icons := make([]string, 0, len(parts))
	for _, part := range parts {
		value := strings.TrimSpace(part)
		if value == "" {
			continue
		}
		icons = append(icons, value)
	}
	return icons
}

func validateIcons(configDir string, icons []string) error {
	if len(icons) == 0 {
		return errors.New("no icons provided")
	}
	for _, icon := range icons {
		path := icon
		if !filepath.IsAbs(path) {
			path = filepath.Join(configDir, icon)
		}
		if _, err := os.Stat(path); err != nil {
			return fmt.Errorf("icon not found: %s", icon)
		}
	}
	return nil
}

func setWindowTitle(root map[string]any, title string) error {
	app, err := ensureMap(root, "app")
	if err != nil {
		return err
	}
	windows, err := ensureSlice(app, "windows")
	if err != nil {
		return err
	}
	if len(windows) == 0 {
		windows = append(windows, map[string]any{"title": title})
		app["windows"] = windows
		return nil
	}
	window, ok := windows[0].(map[string]any)
	if !ok {
		return errors.New("app.windows[0] is not an object")
	}
	window["title"] = title
	return nil
}

func setIcons(root map[string]any, icons []string) error {
	bundle, err := ensureMap(root, "bundle")
	if err != nil {
		return err
	}
	values := make([]any, 0, len(icons))
	for _, icon := range icons {
		values = append(values, icon)
	}
	bundle["icon"] = values
	return nil
}

func ensureMap(parent map[string]any, key string) (map[string]any, error) {
	value, ok := parent[key]
	if !ok {
		next := map[string]any{}
		parent[key] = next
		return next, nil
	}
	child, ok := value.(map[string]any)
	if !ok {
		return nil, fmt.Errorf("%s is not an object", key)
	}
	return child, nil
}

func ensureSlice(parent map[string]any, key string) ([]any, error) {
	value, ok := parent[key]
	if !ok {
		next := []any{}
		parent[key] = next
		return next, nil
	}
	child, ok := value.([]any)
	if !ok {
		return nil, fmt.Errorf("%s is not an array", key)
	}
	return child, nil
}

func exitf(format string, args ...any) {
	fmt.Fprintf(os.Stderr, "error: "+format+"\n", args...)
	os.Exit(1)
}
