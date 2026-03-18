package main

import (
	"os"
	"path/filepath"
	"strings"
	"sync"
	"time"
)

type CacheEntry struct {
	Path       string
	Size       int64
	LastAccess time.Time
}

type Cache struct {
	dir         string
	maxSize     int64
	currentSize int64
	mu          sync.Mutex
	entries     map[string]*CacheEntry
}

func NewCache(dir string, maxSize int64) *Cache {
	os.MkdirAll(dir, 0755)
	c := &Cache{
		dir:     dir,
		maxSize: maxSize,
		entries: make(map[string]*CacheEntry),
	}

	c.loadExisting()
	return c
}

func (c *Cache) Exists(key string) (*CacheEntry, bool) {
	c.mu.Lock()
	defer c.mu.Unlock()

	entry, ok := c.entries[key]
	if ok {
		entry.LastAccess = time.Now()
	}

	return entry, ok
}

func (c *Cache) GetPath(key string) string {
	return filepath.Join(c.dir, key+".tar.gz")
}

func (c *Cache) Add(key string, path string, size int64) {
	c.mu.Lock()
	defer c.mu.Unlock()

	c.entries[key] = &CacheEntry{
		Path:       path,
		Size:       size,
		LastAccess: time.Now(),
	}

	c.currentSize += size
	c.evictIfNeededLocked()
}

func (c *Cache) evictIfNeededLocked() {
	for c.currentSize > c.maxSize {
		var oldestKey string
		var oldestTime time.Time

		for k, v := range c.entries {
			if oldestKey == "" || v.LastAccess.Before(oldestTime) {
				oldestKey = k
				oldestTime = v.LastAccess
			}
		}

		if oldestKey == "" {
			return
		}

		entry := c.entries[oldestKey]

		_ = os.Remove(entry.Path)

		c.currentSize -= entry.Size
		delete(c.entries, oldestKey)
	}
}

func (c *Cache) loadExisting() {
	files, err := os.ReadDir(c.dir)
	if err != nil {
		return
	}

	for _, f := range files {
		if f.IsDir() {
			continue
		}

		path := filepath.Join(c.dir, f.Name())

		info, err := os.Stat(path)
		if err != nil {
			continue
		}

		// Extract key from filename
		key := strings.TrimSuffix(f.Name(), ".tar.gz")

		c.entries[key] = &CacheEntry{
			Path:       path,
			Size:       info.Size(),
			LastAccess: info.ModTime(), // reasonable default
		}

		c.currentSize += info.Size()
	}

	// Enforce size limit on startup
	c.evictIfNeededLocked()
}
