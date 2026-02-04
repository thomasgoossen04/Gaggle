package main

import (
	"testing"

	"github.com/gin-gonic/gin"
)

func newTestStore(t *testing.T) *Store {
	t.Helper()

	dir := t.TempDir()
	store, err := InitDb(dir)
	if err != nil {
		t.Fatalf("failed to init db: %v", err)
	}
	t.Cleanup(func() {
		_ = store.Close()
	})
	return store
}

func newTestRouter(store *Store) *gin.Engine {
	gin.SetMode(gin.TestMode)
	router := gin.New()
	cfg := &Config{}
	RegisterRoutes(router, store, cfg)
	return router
}
