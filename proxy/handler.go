package main

import (
	"io"
	"net/http"
	"os"
	"path/filepath"

	"github.com/gin-gonic/gin"
	"github.com/golang/groupcache/singleflight"
)

type Proxy struct {
	cache   *Cache
	group   singleflight.Group
	nasBase string
}

func NewProxy(cache *Cache, nasBase string) *Proxy {
	return &Proxy{
		cache:   cache,
		nasBase: nasBase,
	}
}

func (p *Proxy) HandleDownload(c *gin.Context) {
	id := c.Param("id")
	cachePath := p.cache.GetPath(id)

	// Cache hit
	if entry, ok := p.cache.Exists(id); ok {
		file, err := os.Open(entry.Path)
		if err == nil {
			defer file.Close()
			stat, _ := file.Stat()
			http.ServeContent(c.Writer, c.Request, filepath.Base(entry.Path), stat.ModTime(), file)
			return
		}
	}

	// Cache miss → download
	_, err := p.group.Do(id, func() (interface{}, error) {
		// Double-check after acquiring lock
		if _, err := os.Stat(cachePath); err == nil {
			return nil, nil
		}

		url := p.nasBase + "/" + id + ".tar.gz"

		resp, err := http.Get(url)
		if err != nil {
			return nil, err
		}
		defer resp.Body.Close()

		out, err := os.Create(cachePath)
		if err != nil {
			return nil, err
		}
		defer out.Close()

		_, err = io.Copy(out, resp.Body)
		if err != nil {
			return nil, err
		}

		stat, err := os.Stat(cachePath)
		if err == nil {
			p.cache.Add(id, cachePath, stat.Size())
		}

		return nil, nil
	})
	if err != nil {
		c.Status(500)
		return
	}

	// Now serve (ALL clients get it)
	file, err := os.Open(cachePath)
	if err != nil {
		c.Status(500)
		return
	}
	defer file.Close()

	stat, _ := file.Stat()
	http.ServeContent(c.Writer, c.Request, filepath.Base(cachePath), stat.ModTime(), file)
}
