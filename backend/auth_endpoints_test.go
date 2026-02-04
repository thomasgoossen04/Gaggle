package main

import (
	"net/http"
	"net/http/httptest"
	"os"
	"testing"
)

func TestAppEndpointsRequireAuth(t *testing.T) {
	store := newTestStore(t)
	router := newTestRouter(store)

	endpoints := []struct {
		name   string
		method string
		path   string
	}{
		{name: "list", method: http.MethodGet, path: "/apps"},
		{name: "config", method: http.MethodGet, path: "/apps/test-app/config"},
		{name: "archive", method: http.MethodGet, path: "/apps/test-app/archive"},
		{name: "refresh", method: http.MethodPost, path: "/apps/refresh"},
	}

	for _, ep := range endpoints {
		t.Run(ep.name, func(t *testing.T) {
			req := httptest.NewRequest(ep.method, ep.path, nil)
			rec := httptest.NewRecorder()
			router.ServeHTTP(rec, req)
			if rec.Code != http.StatusUnauthorized {
				t.Fatalf("expected 401 for %s %s, got %d", ep.method, ep.path, rec.Code)
			}
		})
	}
}

func TestAppEndpointsAllowAuth(t *testing.T) {
	store := newTestStore(t)
	router := newTestRouter(store)

	err := store.UpsertUser(User{ID: "u1", Username: "tester"})
	if err != nil {
		t.Fatalf("failed to upsert user: %v", err)
	}
	token, err := store.CreateSession("u1", 0)
	if err != nil {
		t.Fatalf("failed to create session: %v", err)
	}

	endpoints := []struct {
		name       string
		method     string
		path       string
		expectCode int
	}{
		{name: "list", method: http.MethodGet, path: "/apps", expectCode: http.StatusOK},
		{name: "config", method: http.MethodGet, path: "/apps/test-app/config", expectCode: http.StatusNotFound},
		{name: "archive", method: http.MethodGet, path: "/apps/test-app/archive", expectCode: http.StatusNotFound},
		{name: "refresh", method: http.MethodPost, path: "/apps/refresh", expectCode: http.StatusOK},
	}

	for _, ep := range endpoints {
		t.Run(ep.name, func(t *testing.T) {
			req := httptest.NewRequest(ep.method, ep.path, nil)
			req.Header.Set("Authorization", "Bearer "+token)
			rec := httptest.NewRecorder()
			router.ServeHTTP(rec, req)
			if rec.Code != ep.expectCode {
				t.Fatalf("expected %d for %s %s, got %d", ep.expectCode, ep.method, ep.path, rec.Code)
			}
		})
	}
}

func TestMain(m *testing.M) {
	// Ensure tests don't accidentally read real config.toml from another working dir.
	_ = os.Chdir("..")
	os.Exit(m.Run())
}
