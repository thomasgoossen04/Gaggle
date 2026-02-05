package main

import (
	"bytes"
	"net/http"
	"net/http/httptest"
	"testing"
)

func TestPlaytimeEndpointsRequireAuth(t *testing.T) {
	store := newTestStore(t)
	router := newTestRouter(store)

	endpoints := []struct {
		name   string
		method string
		path   string
		body   string
	}{
		{name: "list", method: http.MethodGet, path: "/apps/playtime"},
		{name: "post", method: http.MethodPost, path: "/apps/test-app/playtime", body: `{"seconds":60}`},
	}

	for _, ep := range endpoints {
		t.Run(ep.name, func(t *testing.T) {
			var body *bytes.Buffer
			if ep.body != "" {
				body = bytes.NewBufferString(ep.body)
			} else {
				body = bytes.NewBuffer(nil)
			}
			req := httptest.NewRequest(ep.method, ep.path, body)
			req.Header.Set("Content-Type", "application/json")
			rec := httptest.NewRecorder()
			router.ServeHTTP(rec, req)
			if rec.Code != http.StatusUnauthorized {
				t.Fatalf("expected 401 for %s %s, got %d", ep.method, ep.path, rec.Code)
			}
		})
	}
}

func TestPlaytimeEndpointsHandleAuth(t *testing.T) {
	store := newTestStore(t)
	router := newTestRouter(store)

	if err := store.UpsertUser(User{ID: "u1", Username: "tester"}); err != nil {
		t.Fatalf("failed to upsert user: %v", err)
	}
	token, err := store.CreateSession("u1", 0)
	if err != nil {
		t.Fatalf("failed to create session: %v", err)
	}

	t.Run("list", func(t *testing.T) {
		req := httptest.NewRequest(http.MethodGet, "/apps/playtime", nil)
		req.Header.Set("Authorization", "Bearer "+token)
		rec := httptest.NewRecorder()
		router.ServeHTTP(rec, req)
		if rec.Code != http.StatusOK {
			t.Fatalf("expected 200, got %d", rec.Code)
		}
	})

	t.Run("post-invalid", func(t *testing.T) {
		req := httptest.NewRequest(
			http.MethodPost,
			"/apps/test-app/playtime",
			bytes.NewBufferString(`{"seconds":0}`),
		)
		req.Header.Set("Authorization", "Bearer "+token)
		req.Header.Set("Content-Type", "application/json")
		rec := httptest.NewRecorder()
		router.ServeHTTP(rec, req)
		if rec.Code != http.StatusBadRequest {
			t.Fatalf("expected 400, got %d", rec.Code)
		}
	})
}
