// SPDX-License-Identifier: Apache-2.0
// Go fsnotify, used the usual way: every directory watched, new ones
// added on Create.
package main

import (
	"fmt"
	"io/fs"
	"os"
	"path/filepath"
	"time"

	"github.com/fsnotify/fsnotify"
)

func main() {
	w, err := fsnotify.NewWatcher()
	if err != nil {
		panic(err)
	}
	filepath.WalkDir(os.Args[1], func(p string, d fs.DirEntry, err error) error {
		if err == nil && d.IsDir() {
			w.Add(p)
		}
		return nil
	})
	for {
		select {
		case e := <-w.Events:
			fmt.Printf("%d fsnotify %s %s\n", time.Now().UnixMilli(), e.Op, e.Name)
			if e.Op&fsnotify.Create != 0 {
				if fi, err := os.Stat(e.Name); err == nil && fi.IsDir() {
					w.Add(e.Name)
				}
			}
		case err := <-w.Errors:
			fmt.Fprintln(os.Stderr, err)
		}
	}
}
