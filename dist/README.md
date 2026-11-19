# Packaging Cookie

Already packaged for [GURT](https://github.com/clrealy/GURT): the recipe is in
**[gurt/](gurt/)**.

The rest of this file is for packaging Cookie for any other distribution.

These files are for installing Cookie **system-wide**, from a distribution
package. A user installing Cookie by hand does not need them — they run
`cookie kitchen setup`, which writes the per-user equivalents under
`~/.local/share`.

Both routes produce the same behaviour. The files here are generated from the
same code `cookie kitchen setup` uses, and a test (`tests/dist_files.rs`)
fails if they ever drift apart.

## What goes where

| File | Install to |
|---|---|
| `cookie.desktop` | `/usr/share/applications/cookie.desktop` |
| `cookie.xml` | `/usr/share/mime/packages/cookie.xml` |
| `../assets/cookie-icon.png` | `/usr/share/icons/hicolor/256x256/apps/cookie.png` |
| `../assets/cookie-128.png` | `/usr/share/icons/hicolor/128x128/apps/cookie.png` |
| `../assets/cookie-icon.png` | `/usr/share/icons/hicolor/256x256/mimetypes/application-x-cookie-package.png` |
| `../assets/cookie-128.png` | `/usr/share/icons/hicolor/128x128/mimetypes/application-x-cookie-package.png` |

## Example

```sh
install -Dm755 target/release/cookie    "$DESTDIR/usr/bin/cookie"
install -Dm644 dist/cookie.desktop      "$DESTDIR/usr/share/applications/cookie.desktop"
install -Dm644 dist/cookie.xml          "$DESTDIR/usr/share/mime/packages/cookie.xml"
install -Dm644 assets/cookie-icon.png   "$DESTDIR/usr/share/icons/hicolor/256x256/apps/cookie.png"
install -Dm644 assets/cookie-128.png    "$DESTDIR/usr/share/icons/hicolor/128x128/apps/cookie.png"
install -Dm644 assets/cookie-icon.png   "$DESTDIR/usr/share/icons/hicolor/256x256/mimetypes/application-x-cookie-package.png"
install -Dm644 assets/cookie-128.png    "$DESTDIR/usr/share/icons/hicolor/128x128/mimetypes/application-x-cookie-package.png"
install -Dm644 LICENSE                  "$DESTDIR/usr/share/licenses/cookie/LICENSE"
```

## Post-install

Refresh the two databases, if the package manager does not do it already:

```sh
update-mime-database /usr/share/mime
update-desktop-database /usr/share/applications
```

## What the package does not need to do

- **Do not** set a default application in a system `mimeapps.list`. Cookie's
  desktop entry declares the MIME type, which is enough for it to appear in
  "Open With". Which application *opens* a file is the user's choice, and
  `cookie kitchen setup` sets it per user.
- **Do not** install per-app launchers. Those are created by `cookie eat`
  when a user installs an app, and live in that user's home.
- **Do not** ship a kernel module. Cookie uses the binder support already in
  the kernel and never builds or loads anything. If it is missing, `cookie
  burnt` says which config options to turn on.

## Runtime dependencies

| Package | Why | Required? |
|---|---|---|
| `waydroid` | the Android runtime, for now | yes, until milestone 4 |
| `shared-mime-info` | `update-mime-database` | recommended |
| `desktop-file-utils` | `update-desktop-database` | recommended |
| `kdialog` or `libnotify` | shows errors when an app is launched from the menu, where there is no terminal | optional |

Cookie never calls `systemctl` and works the same on OpenRC, systemd, runit
and s6.
