package main

import (
	"flag"
	"fmt"
	"os"
	"path/filepath"
	"strings"

	"github.com/leaanthony/dmg/dmg"
)

func fail(format string, args ...any) {
	fmt.Fprintf(os.Stderr, "error: "+format+"\n", args...)
	os.Exit(1)
}

func parseBackend(name string) (dmg.Backend, error) {
	switch strings.ToLower(name) {
	case "auto":
		return dmg.BackendAuto, nil
	case "native":
		return dmg.BackendNative, nil
	case "hdiutil":
		return dmg.BackendHdiutil, nil
	default:
		return dmg.BackendAuto, fmt.Errorf("unsupported backend %q (use auto, native, or hdiutil)", name)
	}
}

func main() {
	app := flag.String("app", "", "path to the .app bundle")
	out := flag.String("out", "", "output .dmg path")
	background := flag.String("background", "", "Finder background image")
	volumeIcon := flag.String("volume-icon", "", "volume icon (.icns/.png/.jpg)")
	volumeName := flag.String("volume-name", "Typst Editor", "volume name")
	backendName := flag.String("backend", "auto", "auto, native, or hdiutil")

	winWidth := flag.Int("window-width", 660, "Finder window width")
	winHeight := flag.Int("window-height", 420, "Finder window height")
	appX := flag.Int("app-x", 150, "app icon centre X")
	appY := flag.Int("app-y", 190, "app icon centre Y")
	appsX := flag.Int("apps-x", 510, "Applications icon centre X")
	appsY := flag.Int("apps-y", 190, "Applications icon centre Y")

	flag.Parse()

	if *app == "" || *out == "" {
		fail("-app and -out are required")
	}
	info, err := os.Stat(*app)
	if err != nil || !info.IsDir() {
		fail("app bundle is not a directory: %s", *app)
	}
	if *background == "" {
		fail("-background is required")
	}
	if _, err := os.Stat(*background); err != nil {
		fail("background not found: %s", *background)
	}
	if *volumeIcon != "" {
		if _, err := os.Stat(*volumeIcon); err != nil {
			fail("volume icon not found: %s", *volumeIcon)
		}
	}
	backend, err := parseBackend(*backendName)
	if err != nil {
		fail("%v", err)
	}

	appName := filepath.Base(filepath.Clean(*app))
	opts := dmg.Options{
		VolumeName:             *volumeName,
		OutputPath:             *out,
		Files:                  map[string]string{appName: *app},
		AddApplicationsSymlink: true,
		Window: dmg.WindowConfig{
			X: 200, Y: 120, Width: *winWidth, Height: *winHeight,
		},
		Icon: dmg.IconConfig{Size: 128, TextSize: 13, GridSpace: 100},
		IconPositions: map[string]dmg.IconPosition{
			appName:        {X: *appX, Y: *appY},
			"Applications": {X: *appsX, Y: *appsY},
		},
		Background: &dmg.BackgroundConfig{File: *background},
		Format:     dmg.FormatUDZO,
		Filesystem: dmg.FSHFSPlus,
		Backend:    backend,
	}
	if *volumeIcon != "" {
		opts.VolumeIcon = *volumeIcon
	}
	if err := os.MkdirAll(filepath.Dir(*out), 0o755); err != nil {
		fail("create output directory: %v", err)
	}
	if err := dmg.Build(opts); err != nil {
		fail("build dmg: %v", err)
	}
	st, err := os.Stat(*out)
	if err != nil {
		fail("stat output: %v", err)
	}
	fmt.Printf("dmg: %s (%d bytes)\n", *out, st.Size())
}
