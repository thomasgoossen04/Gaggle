package main

import (
	"bytes"
	"net/http"
	"net/http/httptest"
	"os"
	"path/filepath"
	"sync"
	"testing"

	"github.com/gin-gonic/gin"
)

func setupTestProxy(t *testing.T) (*Proxy, func()) {
	tmpDir := t.TempDir()

	cache := NewCache(tmpDir, 1024*1024*1024) // 1GB for tests

	// Fake NAS server
	var requestCount int
	var mu sync.Mutex

	nas := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		mu.Lock()
		requestCount++
		mu.Unlock()

		data := []byte("THIS_IS_TEST_FILE_CONTENT_1234567890")

		// Handle Range requests
		rangeHeader := r.Header.Get("Range")
		if rangeHeader != "" {
			w.WriteHeader(http.StatusPartialContent)
			w.Write(data[5:15]) // simple slice
			return
		}

		w.WriteHeader(http.StatusOK)
		w.Write(data)
	}))

	proxy := NewProxy(cache, nas.URL)

	cleanup := func() {
		nas.Close()
	}

	return proxy, cleanup
}

func TestCacheMissFetchesFromNAS(t *testing.T) {
	gin.SetMode(gin.TestMode)

	proxy, cleanup := setupTestProxy(t)
	defer cleanup()

	r := gin.Default()
	r.GET("/apps/:id/archive", proxy.HandleDownload)

	req := httptest.NewRequest("GET", "/apps/test/archive", nil)
	w := httptest.NewRecorder()

	r.ServeHTTP(w, req)

	if w.Code != 200 {
		t.Fatalf("expected 200, got %d", w.Code)
	}

	body := w.Body.Bytes()
	expected := []byte("THIS_IS_TEST_FILE_CONTENT_1234567890")

	if !bytes.Equal(body, expected) {
		t.Fatalf("unexpected body: %s", string(body))
	}

	// Check file exists in cache
	cachePath := proxy.cache.GetPath("test")
	if _, err := os.Stat(cachePath); err != nil {
		t.Fatalf("file not cached")
	}
}

func TestCacheHitServesFromDisk(t *testing.T) {
	gin.SetMode(gin.TestMode)

	proxy, cleanup := setupTestProxy(t)
	defer cleanup()

	r := gin.Default()
	r.GET("/apps/:id/archive", proxy.HandleDownload)

	// First request (fills cache)
	req1 := httptest.NewRequest("GET", "/apps/test/archive", nil)
	w1 := httptest.NewRecorder()
	r.ServeHTTP(w1, req1)

	// Second request (should hit cache)
	req2 := httptest.NewRequest("GET", "/apps/test/archive", nil)
	w2 := httptest.NewRecorder()
	r.ServeHTTP(w2, req2)

	if w2.Code != 200 {
		t.Fatalf("expected 200, got %d", w2.Code)
	}

	if w2.Body.Len() == 0 {
		t.Fatalf("empty response on cache hit")
	}
}

func TestSingleflightPreventsDuplicateFetches(t *testing.T) {
	gin.SetMode(gin.TestMode)

	tmpDir := t.TempDir()
	cache := NewCache(tmpDir, 1024*1024*1024)

	var requestCount int
	var mu sync.Mutex

	nas := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		mu.Lock()
		requestCount++
		mu.Unlock()

		data := []byte("DATA")
		w.Write(data)
	}))
	defer nas.Close()

	proxy := NewProxy(cache, nas.URL)

	r := gin.Default()
	r.GET("/apps/:id/archive", proxy.HandleDownload)

	var wg sync.WaitGroup

	for i := 0; i < 10; i++ {
		wg.Add(1)
		go func() {
			defer wg.Done()
			req := httptest.NewRequest("GET", "/apps/test/archive", nil)
			w := httptest.NewRecorder()
			r.ServeHTTP(w, req)
		}()
	}

	wg.Wait()

	if requestCount != 1 {
		t.Fatalf("expected 1 upstream request, got %d", requestCount)
	}
}

func TestRangeRequestFromCache(t *testing.T) {
	gin.SetMode(gin.TestMode)

	proxy, cleanup := setupTestProxy(t)
	defer cleanup()

	r := gin.Default()
	r.GET("/apps/:id/archive", proxy.HandleDownload)

	// First request to populate cache
	req1 := httptest.NewRequest("GET", "/apps/test/archive", nil)
	w1 := httptest.NewRecorder()
	r.ServeHTTP(w1, req1)

	// Range request
	req2 := httptest.NewRequest("GET", "/apps/test/archive", nil)
	req2.Header.Set("Range", "bytes=0-9")

	w2 := httptest.NewRecorder()
	r.ServeHTTP(w2, req2)

	if w2.Code != http.StatusPartialContent {
		t.Fatalf("expected 206, got %d", w2.Code)
	}

	if w2.Body.Len() == 0 {
		t.Fatalf("empty range response")
	}
}

func TestCacheEviction(t *testing.T) {
	tmpDir := t.TempDir()

	cache := NewCache(tmpDir, 50)

	// Create fake files
	pathA := filepath.Join(tmpDir, "a")
	os.WriteFile(pathA, []byte("aaaaaaaaaa"), 0644)

	pathB := filepath.Join(tmpDir, "b")
	os.WriteFile(pathB, []byte("bbbbbbbbbb"), 0644)

	cache.Add("a", pathA, 30)
	cache.Add("b", pathB, 30)

	if cache.currentSize > 50 {
		t.Fatalf("cache exceeded max size")
	}

	if len(cache.entries) != 1 {
		t.Fatalf("expected 1 entry after eviction, got %d", len(cache.entries))
	}
}
