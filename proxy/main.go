package main

import (
	"log"
	"os"
	"strconv"
	"strings"

	"github.com/gin-gonic/gin"
)

const defaultCacheSize = 10 * 1024 * 1024 * 1024 // 10GB

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

func main() {
	cacheSize := parseCacheSize()

	log.Printf("Cache size: %d bytes (%.2f GB)", cacheSize, float64(cacheSize)/(1024*1024*1024))

	cache := NewCache("./cache", cacheSize)
	proxy := NewProxy(cache, "http://nas:8080/apps")

	r := gin.Default()
	r.GET("/apps/:id/archive", proxy.HandleDownload)

	r.Run(":8081")
}
