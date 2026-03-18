package main

import (
	"log"
	"os"
	"strconv"
	"strings"

	"github.com/gin-gonic/gin"
)

const (
	defaultCacheSize  = 10 * 1024 * 1024 * 1024 // 10GB
	defaultNASBaseURL = "http://127.0.0.1:2122/apps"
)

func parseCacheSize() int64 {
	val := os.Getenv("CACHE_SIZE")
	if val == "" {
		return defaultCacheSize
	}

	val = strings.ToUpper(strings.TrimSpace(val))

	multiplier := int64(1)

	switch {
	case strings.HasSuffix(val, "GB"):
		multiplier = 1024 * 1024 * 1024
		val = strings.TrimSuffix(val, "GB")
	case strings.HasSuffix(val, "MB"):
		multiplier = 1024 * 1024
		val = strings.TrimSuffix(val, "MB")
	case strings.HasSuffix(val, "KB"):
		multiplier = 1024
		val = strings.TrimSuffix(val, "KB")
	}

	num, err := strconv.ParseInt(strings.TrimSpace(val), 10, 64)
	if err != nil {
		log.Printf("Invalid CACHE_SIZE, using default (10GB)")
		return defaultCacheSize
	}

	return num * multiplier
}

func parseNASBaseURL() string {
	val := strings.TrimSpace(os.Getenv("NAS_BASE_URL"))
	if val == "" {
		log.Printf("NAS_BASE_URL not set, using default: %s", defaultNASBaseURL)
		return defaultNASBaseURL
	}

	// Ensure no trailing slash (avoids double //)
	val = strings.TrimRight(val, "/")

	return val
}

func main() {
	cacheSize := parseCacheSize()
	nasBase := parseNASBaseURL()

	log.Printf("Cache size: %.2f GB", float64(cacheSize)/(1024*1024*1024))
	log.Printf("NAS base URL: %s", nasBase)

	cache := NewCache("./cache", cacheSize)
	proxy := NewProxy(cache, nasBase)

	r := gin.Default()
	r.GET("/apps/:id/archive", proxy.HandleDownload)

	r.Run(":8081")
}
