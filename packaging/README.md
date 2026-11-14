# FrameForge Packaging Notes

## .deb Package

Built for Pop!_OS 22.04 LTS and Ubuntu 22.04/24.04 LTS.

### Dependencies (exact versions from Ubuntu 22.04 repositories)
- libmpv2 (>= 0.34) — libmpv shared library
- libavcodec58, libavformat58, libavutil56, libswscale5, libswresample3, libavfilter7 (>= 7:4.4) — FFmpeg libav*
- libwebkit2gtk-4.0-0 (>= 2.30) — WebKitGTK for the WebView UI
- libgtk-3-0 (>= 3.24) — GTK3 for native widgets
- libssl3 — TLS/cryptography
- libgl1 — OpenGL for libmpv render API
- libglib2.0-0 — GLib core
- libva-drm2, mesa-va-drivers — VAAPI hardware acceleration (recommended)
- nvidia-driver-535+, nvidia-utils-535+ — NVDEC/NVENC (NVIDIA only)

### License considerations
- libmpv: LGPL
- FFmpeg libav*: LGPL (depending on build configuration)
- x264/x265: GPL (if linked)
- Codec patents (H.264/H.265): User's responsibility in their jurisdiction

### Building the .deb
```bash
cd packaging/debian
dpkg-buildpackage -b -us -uc
```

### Generated package structure
```
/usr/bin/frameforge                    # Main binary
/usr/lib/frameforge/                   # Libraries (if any bundled)
/usr/share/applications/frameforge.desktop
/usr/share/icons/hicolor/*/apps/frameforge.png
/usr/share/doc/frameforge/             # Copyright, changelog
```

## AppImage

Built as a portable single-file distribution.

### Important limitations
- AppImage does NOT bundle GTK/WebKitGTK/libmpv/FFmpeg by default.
- It relies on host system libraries being present.
- Hardware acceleration (VAAPI/NVENC) requires host GPU drivers.
- For a fully self-contained AppImage, use linuxdeploy or linuxdeployqt to bundle all GTK/WebKitGTK dependencies (increases size to ~100-200 MB).

### Building the AppImage
```bash
cd packaging/appimage
./build-appimage.sh
```

### AppImage runtime requirements
- FUSE (libfuse2) for AppImage execution
- Host libraries: libwebkit2gtk-4.0-0, libmpv2, libav*, libgtk-3-0
- GPU drivers for hardware acceleration

### Flatpak (evaluation only)
Flatpak is evaluated but NOT recommended for v1 due to:
- Sandbox restrictions complicate VAAPI/NVENC device access
- WebKitGTK bundling inside Flatpak is non-trivial
- Adds another build/maintenance pipeline

If pursued later:
- Use `org.freedesktop.Platform` + `org.gtk.Gtk3` + `org.freedesktop.Platform.ffmpeg-full`
- `--device=dri` for GPU access
- `--socket=pulseaudio` for audio

## Platform support
- Pop!_OS 22.04 LTS (primary, GNOME/COSMIC, X11/Wayland)
- Ubuntu 22.04/24.04 LTS
- NVIDIA, AMD, Intel GPUs
- Windows/macOS: explicitly out of scope for v1

## Versioning
- Version from `Cargo.toml` workspace.package.version (currently 0.1.0)
- .deb version matches Cargo version
- AppImage version matches Cargo version

## Signing
- .deb: Not signed in CI (add `debsign` step for release)
- AppImage: Not signed (add gpg sign for release)

## CI Integration (GitHub Actions)
```yaml
- name: Build .deb
  run: |
    cd packaging/debian
    dpkg-buildpackage -b -us -uc
- name: Build AppImage
  run: |
    cd packaging/appimage
    ./build-appimage.sh
- name: Upload artifacts
  uses: actions/upload-artifact@v4
  with:
    path: |
      packaging/debian/*.deb
      packaging/appimage/*.AppImage
```