package main

import (
	"github.com/gin-contrib/cors"
	"github.com/gin-gonic/gin"
)

func main() {
	cfg := MustLoadConfig()
	router := gin.Default()
	router.Use(cors.New(cors.Config{
		AllowOrigins:     []string{"*"},
		AllowMethods:     []string{"*"},
		AllowHeaders:     []string{"*"},
		AllowCredentials: false,
	}))
	gin.SetMode(cfg.Mode)

	store, err := InitDb("./db")
	if err != nil {
		panic(err)
	}

	StartServer(router, store, cfg)
}
