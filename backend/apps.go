package main

import (
	"net/http"
	"os"
	"path/filepath"
	"strings"

	"github.com/BurntSushi/toml"
	"github.com/gin-gonic/gin"
)

const appsDir = "./apps"

type AppConfig struct {
	Name        string `toml:"name" json:"name"`
	Description string `toml:"description" json:"description"`
	Version     string `toml:"version" json:"version"`
}

type AppInfo struct {
	ID          string `json:"id"`
	Name        string `json:"name"`
	Description string `json:"description"`
	Version     string `json:"version"`
	ArchiveSize int64  `json:"archive_size"`
	HasArchive  bool   `json:"has_archive"`
}

func listAppsHandler(c *gin.Context) {
	apps, err := listApps()
	if err != nil {
		c.JSON(http.StatusInternalServerError, gin.H{"error": "failed to list apps"})
		return
	}
	c.JSON(http.StatusOK, apps)
}

func getAppConfigHandler(c *gin.Context) {
	id := c.Param("id")
	if !isSafeAppID(id) {
		c.JSON(http.StatusBadRequest, gin.H{"error": "invalid app id"})
		return
	}
	path := filepath.Join(appsDir, id+".toml")
	content, err := os.ReadFile(path)
	if err != nil {
		c.JSON(http.StatusNotFound, gin.H{"error": "config not found"})
		return
	}
	c.Data(http.StatusOK, "text/plain; charset=utf-8", content)
}

func getAppArchiveHandler(c *gin.Context) {
	id := c.Param("id")
	if !isSafeAppID(id) {
		c.JSON(http.StatusBadRequest, gin.H{"error": "invalid app id"})
		return
	}
	path := filepath.Join(appsDir, id+".tar.gz")
	file, err := os.Open(path)
	if err != nil {
		c.JSON(http.StatusNotFound, gin.H{"error": "archive not found"})
		return
	}
	defer file.Close()

	stat, err := file.Stat()
	if err != nil {
		c.JSON(http.StatusInternalServerError, gin.H{"error": "archive not readable"})
		return
	}

	c.Header("Content-Disposition", "attachment; filename=\""+id+".tar.gz\"")
	c.Header("Content-Type", "application/gzip")
	http.ServeContent(c.Writer, c.Request, id+".tar.gz", stat.ModTime(), file)
}

func listApps() ([]AppInfo, error) {
	entries, err := os.ReadDir(appsDir)
	if err != nil {
		if os.IsNotExist(err) {
			return []AppInfo{}, nil
		}
		return nil, err
	}

	var apps []AppInfo
	for _, entry := range entries {
		if entry.IsDir() || !strings.HasSuffix(entry.Name(), ".toml") {
			continue
		}
		id := strings.TrimSuffix(entry.Name(), ".toml")
		if !isSafeAppID(id) {
			continue
		}

		cfgPath := filepath.Join(appsDir, entry.Name())
		var cfg AppConfig
		if _, err := toml.DecodeFile(cfgPath, &cfg); err != nil {
			cfg = AppConfig{}
		}

		archivePath := filepath.Join(appsDir, id+".tar.gz")
		var size int64
		hasArchive := false
		if stat, err := os.Stat(archivePath); err == nil && !stat.IsDir() {
			size = stat.Size()
			hasArchive = true
		}

		name := cfg.Name
		if name == "" {
			name = id
		}

		apps = append(apps, AppInfo{
			ID:          id,
			Name:        name,
			Description: cfg.Description,
			Version:     cfg.Version,
			ArchiveSize: size,
			HasArchive:  hasArchive,
		})
	}

	return apps, nil
}

func isSafeAppID(id string) bool {
	if id == "" {
		return false
	}
	if strings.Contains(id, "/") || strings.Contains(id, "\\") {
		return false
	}
	if strings.Contains(id, "..") {
		return false
	}
	return true
}
