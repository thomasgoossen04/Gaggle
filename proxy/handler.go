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

	rangeHeader := c.GetHeader("Range")

	// If RANGE request → proxy directly (no cache)
	if rangeHeader != "" {
		url := p.nasBase + "/" + id + ".tar.gz"

		req, _ := http.NewRequest("GET", url, nil)
		req.Header.Set("Range", rangeHeader)

		resp, err := http.DefaultClient.Do(req)
		if err != nil {
			c.Status(502)
			return
		}
		defer resp.Body.Close()

		// copy headers
		for k, v := range resp.Header {
			for _, vv := range v {
				c.Header(k, vv)
			}
		}

		c.Status(resp.StatusCode)
		io.Copy(c.Writer, resp.Body)
		return
	}

	// Normal cache logic (no Range)
	if entry, ok := p.cache.Exists(id); ok {
		file, err := os.Open(entry.Path)
		if err == nil {
			defer file.Close()
			stat, _ := file.Stat()
			http.ServeContent(c.Writer, c.Request, filepath.Base(entry.Path), stat.ModTime(), file)
			return
		}
	}

	// fetch full file → cache → serve
	_, err := p.group.Do(id, func() (interface{}, error) {
		// download full file
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

		stat, _ := os.Stat(cachePath)
		p.cache.Add(id, cachePath, stat.Size())

		return nil, nil
	})
	if err != nil {
		c.Status(500)
		return
	}

	file, _ := os.Open(cachePath)
	defer file.Close()
	stat, _ := file.Stat()
	http.ServeContent(c.Writer, c.Request, filepath.Base(cachePath), stat.ModTime(), file)
}
