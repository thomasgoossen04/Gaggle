package main

import (
	"github.com/gin-gonic/gin"
)

func main() {
	cfg := MustLoadConfig()
	router := gin.Default()
	gin.SetMode(cfg.Mode)

	store, err := InitDb("./db")
	if err != nil {
		panic(err)
	}
	defer store.db.Close()
	store.UpsertUser(User{ID: "TEST", Username: "TEST!"})

	StartServer(router, store, cfg)
}
