package main

import (
	"io"
	"net/http"
	"os"
)

func fetchAndCache(url, path string, clientWriter io.Writer) error {
	resp, err := http.Get(url)
	if err != nil {
		return err
	}
	defer resp.Body.Close()

	out, err := os.Create(path)
	if err != nil {
		return err
	}
	defer out.Close()

	writer := io.MultiWriter(clientWriter, out)
	_, err = io.Copy(writer, resp.Body)
	return err
}
