<div align="center">

<img src="assets/cookie.png" alt="Cookie" width="160">

# Cookie 🍪

**Run Android apps on Linux like they belong there.**

One app, one ordinary desktop window. No Android home screen in the way.

</div>

---

> ## ⚠️ Work in progress — nothing here is finished
>
> Cookie is at **milestone 4 of 4**, and milestone 4 is only *started*.
>
> Running apps today still goes through [Waydroid](https://waydro.id/). The
> commands work, the `.cpk` format is real, double-clicking a cookie runs it,
> installed apps appear in your application menu, updates keep your data, and
> `cookie burnt` will tell you honestly what your machine is missing.
>
> **Cookie's own runtime — the oven — can prepare and boot an Android
> container, but it cannot show an app's window yet.** That needs a display
> bridge, which is the largest single piece Waydroid provides and the next
> thing to build. See **[docs/oven.md](docs/oven.md)**.
>
> Expect rough edges. Expect the `.cpk` format to gain fields. Do not put
> anything you care about in here yet.

---

## Why "Cookie"

[Wine](https://winehq.org) runs Windows programs on Linux.
[Darling](https://darlinghq.org) runs macOS programs on Linux.

Both of those sound like something a grandma would offer you when you visit.
And what else do grandmas give you?

Cookies. 🍪

So the commands are hers too. You **bake** an app into a cookie, you **eat**
it to run it, your cookies live in the **jar**, you check the **crumbs** when
something misbehaves, you ask whether anything is **burnt**, and the Android
runtime they all share is the **oven**. When you are done with an app, you
**toss** it.

---

## What it actually does

An Android APK goes in. A `.cpk` file — a *cookie* — comes out. Running that
cookie starts the app in its own window, sized and decorated like any other
program on your desktop, with no Android launcher around it.

Underneath, Android apps need the kernel's binder IPC driver, the same
mechanism Waydroid uses. **Cookie never builds or loads a kernel module.** It
only detects what your kernel already has, and if binder is missing it tells
you exactly which options to turn on and stops:

```
CONFIG_ANDROID_BINDER_IPC=y   (or =m)
CONFIG_ANDROID_BINDERFS=y     (or =m)
```

Cookie also **only ever reads files you hand it.** It does not download apps,
search for them, or fetch anything from the internet.

---

## Installing

### Requirements

- Linux with binder support in the kernel (`cookie burnt` checks)
- A **Wayland** session — KDE Plasma, GNOME, Sway, anything
- [Waydroid](https://waydro.id/), for now
- Rust 1.74+ to build

Cookie works the same under **OpenRC, systemd, runit and s6**. It never calls
`systemctl`; when a service needs starting it prints the command for the init
system you actually run.

### Build it

```sh
git clone https://github.com/clrealy/cookie
cd cookie
cargo build --release
sudo install -Dm755 target/release/cookie /usr/local/bin/cookie
```

### Set up the Android side, once

```sh
sudo waydroid init          # downloads the Android image; add -s GAPPS if you want Google apps
cookie burnt                # check everything is in place
cookie oven start           # warm up the runtime
cookie kitchen setup        # teach your desktop about .cpk files
```

`waydroid init` is the one step that genuinely needs root, which is why Cookie
asks you to run it yourself rather than calling `sudo` behind your back.
`cookie kitchen setup` needs no root at all — everything it writes goes under
your own home directory.

Packaging Cookie for a distribution instead? See **[dist/README.md](dist/README.md)**.

---

## The commands

### `cookie bake` — APK in, cookie out

```sh
cookie bake teapot.apk                              # -> teapot.cpk
cookie bake base.apk split_config.arm64_v8a.apk     # a split set
cookie bake teapot.xapk                             # .xapk / .apks / .apkm bundles
cookie bake teapot.apk -o ~/cookies/teapot.cpk      # choose where it lands
cookie bake teapot.apk --name "My Teapot"           # override the display name
cookie bake teapot.apk --force                      # overwrite an existing cookie
```

```
:: reading the APK
:: adding teapot.apk

✔ Baked Teapot  -> teapot.cpk
      package   com.example.teapot
      version   2.1.0 (4210)
      abis      arm64-v8a
      size      12.4 MiB

  Eat it with:  cookie eat teapot.cpk
```

### `cookie eat` — install if needed, then run

```sh
cookie eat teapot.cpk                   # install (if new) and launch
cookie eat Teapot                       # an app already in the jar, by name
cookie eat com.example.teapot           # or by package id
cookie eat teapot.cpk --no-launch       # install only
cookie eat teapot.cpk --reinstall       # install again even if it is already there
cookie eat teapot.cpk --no-launcher     # skip the application-menu entry
cookie eat teapot.cpk --allow-downgrade # install an older build over a newer one
cookie eat teapot.cpk --replace         # replace an app signed by a different key
```

Warms the oven for you if it is cold, turns on one-window-per-app mode, and
adds the app to your application menu with its own icon.

If the app is already installed, this is also where updating happens — see
below.

This is also what runs when you double-click a `.cpk` in your file manager.

---

## Updating an app, without losing its data

Bake a newer APK of something you already have, and Cookie says so:

```
✔ Baked Teapot -> teapot.cpk
      package   com.example.teapot
      version   2.2.0 (4300)

  ✔ updates the installed Teapot  2.1.0 (4210) -> 2.2.0 (4300), keeping its data

  Eat it with:  cookie eat teapot.cpk
```

Eating it installs the new build over the old one. Your logins, settings and
files stay exactly where they were.

### Why it checks the signing key first

Android ties an app's data to the key that signed it. Two APKs can claim the
same package id — `com.example.teapot` — while being built by completely
different people. Installing one over the other would hand the first app's
data, including anything it is signed in to, to a build that did not create
it.

So Cookie compares signing certificates before it does anything. Not version
numbers, not names: the key.

```
✘ signed by a different key  Teapot is already installed and was signed by someone else

      installed  5a63565ddd461e87  (v1, v2, v3)
      this one   b7f84552d3c7692d  (v1, v2, v3)

      Both claim the package id com.example.teapot, so Android treats them as the
      same app — but they were built by different people, or with different keys.
      Replacing one with the other would hand the installed app's data, including
      any accounts it is signed in to, to a build that did not create it.

      Cookie will not do that quietly. If you are sure this is the app you want:
        cookie toss Teapot            # remove the installed one and its data
        cookie eat <file.cpk>         # then install this one fresh

      Or, in one step, which also deletes the app's data:
        cookie eat <file.cpk> --replace
```

That check happens **before** the oven is started and before anything is
deleted, so a cookie with the wrong key costs you nothing but the message.

### What Cookie decides

| Situation | What happens |
|---|---|
| Not installed | Normal install |
| Same key, newer version | Updates in place, **data kept** |
| Same key, same version | Reinstalls, data kept |
| Same key, older version | Refused — Android does not downgrade by default. `--allow-downgrade` to insist |
| **Different key** | **Refused.** `--replace` to wipe and install fresh |
| Signature unreadable | Warns, then lets Android decide and report |

### How the key is read

Cookie reads the signing certificate from every scheme an APK carries — v1
jar signing (a PKCS#7 blob under `META-INF`), and Signature Scheme v2, v3 and
v3.1 (the APK Signing Block). It hashes the certificate itself, which is the
same value `apksigner verify --print-certs` reports.

Two APKs count as the same app if they share *any* certificate, so adding or
dropping a scheme between releases does not break an update.

Cookie does **not** verify that a signature is cryptographically valid. That
is Android's job at install time, and duplicating it would mean shipping a
crypto stack to re-answer a question the runtime already answers properly.

**One known gap:** Signature Scheme v3 allows an app to rotate to a new
signing key. Cookie does not yet read the rotation lineage, so a rotated app
reads as a key mismatch and is refused with an explanation. That is the safe
direction to be wrong in, and `--replace` is there if you know the rotation is
genuine.

### `cookie jar` — what is installed

```sh
cookie jar          # names and versions
cookie jar --long   # plus ABIs, install date and backend
```

```
3 cookies in the jar
  F-Droid   1.19.2 (1019052)   org.fdroid.fdroid
  Signal    7.1.2 (1412)       org.thoughtcrime.securesms
  Teapot    2.1.0 (4210)       com.example.teapot   needs arm64-v8a
```

Anything whose native code this machine cannot run natively is flagged, so you
know before it fails.

### `cookie crumbs` — logs for one app

```sh
cookie crumbs Signal              # the last 200 lines
cookie crumbs Signal -f           # keep watching as new lines arrive
cookie crumbs Signal -n 1000      # more history
cookie crumbs org.thoughtcrime.securesms   # the package id works too
```

While the app is running, crumbs are filtered by **process id**, which is
exact. When it is not running, Cookie falls back to matching the package name
in the log text and says so. Either way the note goes to stderr, so
`cookie crumbs Signal > log.txt` stays clean.

You do not have to type the whole name — `cookie crumbs signal` and
`cookie crumbs thoughtcrime` both work, as long as the fragment matches only
one cookie.

### `cookie burnt` — is anything wrong?

```sh
cookie burnt         # check everything
cookie burnt --all   # also show detail for checks that passed
```

Checks the kernel's binder support, the binderfs mount, your display server,
the GPU render node, the backend, the oven, and ARM translation. Every failure
comes with the command that fixes it, written for *your* init system. Exits
non-zero when something is genuinely broken, so it is safe in a script.

```
Checking the oven
  ✔ kernel binder support  binderfs mounted at /dev/binderfs
  ✔ binderfs mount  /dev/binderfs
  ✔ display server  Wayland at /run/user/1000/wayland-0 on KDE
  ✔ gpu  render node renderD128
  ✔ backend  waydroid is installed
  ! oven  stopped
      fix: Start it with:
             cookie oven start
  ✔ arm translation  libhoudini on x86_64

! 1 thing worth a look, but nothing is broken.

If an app will not launch
      Read its logs — the Android side almost always says why:
        cookie crumbs Signal
        cookie crumbs Signal --follow     # watch it live while you try again
```

### `cookie oven` — the shared Android runtime

```sh
cookie oven start    # warm it up
cookie oven status   # is it running?
cookie oven stop     # shut it down
cookie oven status --verbose   # plus the raw backend output
```

One oven is shared by every app, so the cost of the Android runtime is paid
once no matter how many cookies you have.

These also drive **Cookie's own runtime**, which is being built behind
`--backend oven`:

```sh
cookie oven image add system.img       # register a system image
cookie oven image                      # show what is registered
cookie oven recipe                     # print the container configuration
cookie oven status --backend oven      # how the oven is doing
cookie burnt  --backend oven           # the full check, oven included
```

It cannot run apps yet — see below.

### `cookie toss` — uninstall

```sh
cookie toss Teapot        # asks first
cookie toss Teapot --yes  # does not
```

Removes the app from Android, its entry from the jar, and its launcher and
icon from your application menu. If the oven is not running, or Waydroid is
not even installed, the jar and menu entries still go — you are never stuck
with something you cannot get rid of.

### `cookie kitchen` — desktop integration

```sh
cookie kitchen setup              # register everything (this is the one you want)
cookie kitchen status             # what is currently registered
cookie kitchen remove             # undo it
cookie kitchen remove --launchers # undo it, app menu entries included
```

`setup` does four things, all under your home directory and none needing root:

- registers the `application/x-cookie-package` MIME type
- installs Cookie as the handler, so a double-clicked `.cpk` runs `cookie eat`
- installs Cookie's icons, so cookies get a cookie in your file manager
- builds an application-menu entry for everything already in the jar

Run it again any time — it is idempotent, and it re-points menu entries if you
have moved the `cookie` binary.

---

## How a double-click knows it is a cookie

Not by the file extension. `.cpk` is not ours alone — several games use it for
unrelated archive formats — so Cookie's MIME definition carries **no filename
glob**. It identifies a cookie by looking inside: zip magic, then the literal
`manifest.toml` as the first entry's name. That is why `cookie bake` always
writes `manifest.toml` first.

Measured against `update-mime-database` and `gio info`, the same stack Dolphin
uses:

| File | Detected as |
|---|---|
| a cookie named `teapot.cpk` | 🍪 `application/x-cookie-package` |
| a cookie with **no extension at all** | 🍪 `application/x-cookie-package` |
| an unrelated zip named `impostor.cpk` | `application/zip` — correctly left alone |
| an empty file named `empty.cpk` | `application/x-zerosize` |
| a cookie renamed `teapot.zip` | `application/zip` |

That last row is the price. A file named `.zip` being treated as a zip is
reasonable; claiming someone else's `.cpk` and breaking their double-click is
not.

---

## Cookie's own runtime, so far

Milestone 4 is **started, not finished**. What the oven does today:

| Piece | State |
|---|---|
| Shared system image — register, check, mount | ✅ |
| binderfs — detect, mount, expose devices | ✅ |
| Container configuration and lifecycle | ✅ |
| Install, remove and list apps | ✅ |
| **App windows on your desktop** | ❌ **not built** |

An Android app does not draw to a window — it hands buffers to SurfaceFlinger,
which composites them through a hardware composer that expects to end at a
display. Getting those surfaces into Wayland windows means writing a
hardware-composer implementation that is itself a Wayland client. That is the
largest single thing Waydroid provides, and Cookie does not have its own yet.

So the oven says so rather than appearing to work:

```
$ cookie burnt --backend oven
  ✔ system image  ~/android/system.img (ext4, 2.1 GiB)
  ✔ container engine  lxc is installed
  ✘ display bridge  not built yet, so the oven cannot show an app's window
```

**The oven replaces Waydroid, not LXC.** Waydroid is a whole session manager —
image handling, properties, window plumbing, its own daemon — and that is the
part worth owning, because it decides how an app feels on the desktop. Running
a container is not that part, LXC already does it well, and it has no opinion
about your init system. Cookie owns the image, binderfs, the configuration,
the lifecycle and the app commands; `cookie oven recipe` prints the entirety of
what Cookie asks of LXC.

**Cookie does not download a system image.** Which Android build to trust is
your call. Hand it one and Cookie checks it:

```sh
cookie oven image add /path/to/system.img
```

Most downloads are Android *sparse* images, which cannot be mounted directly —
Cookie recognises them and tells you the `simg2img` command rather than
failing obscurely.

The full picture, including why the display bridge is hard and what comes
next, is in **[docs/oven.md](docs/oven.md)**.

---

## Where things live

Everything is under the XDG base directories in your home. Nothing Cookie
writes needs root.

| Path | What |
|---|---|
| `~/.local/share/cookie/jar/<package>/` | One directory per installed app: its manifest, icon and install record |
| `~/.cache/cookie/scratch/` | Scratch space while baking and installing; cleaned up automatically |
| `~/.cache/cookie/logs/oven.log` | Output from the last `cookie oven start` |
| `~/.local/share/applications/cookie.desktop` | The handler that opens `.cpk` files |
| `~/.local/share/applications/cookie-<package>.desktop` | One menu entry per installed app |
| `~/.local/share/mime/packages/cookie.xml` | The `.cpk` file type definition |
| `~/.local/share/icons/hicolor/` | Cookie's icon, and each app's |
| `~/.config/mimeapps.list` | Which application opens a `.cpk` |
| `~/.local/share/cookie/oven/` | The oven: its registered image, container config and Android state |

Every one of those is written by `cookie kitchen setup` and removed by
`cookie kitchen remove`. Cookie only ever deletes `.desktop` files carrying
its own `X-Cookie-Managed=true` marker, so a file of yours that happens to
share the naming is left alone.

---

## The `.cpk` format

A cookie is a **zip** holding one app:

```
teapot.cpk
├── manifest.toml     # name, package, version, ABIs, icon, format version
├── icon.png
└── app/
    ├── base.apk
    └── split_config.arm64_v8a.apk
```

It is a zip rather than a `tar.zst` because a zip's central directory lets
`cookie jar` read one app's metadata with a single seek, instead of
decompressing a 400 MB stream to reach a 500-byte file. APKs are stored
without recompression, since they are already compressed zips themselves.

`manifest.toml` is always the **first** entry, which is what lets a file
manager recognise a cookie by its content rather than its name. It records the
app's signing certificate, which is what makes safe updates possible.

The full specification is in **[docs/cpk-spec.md](docs/cpk-spec.md)**.

---

## Roadmap

| | Milestone | State |
|---|---|---|
| **1** | `bake`, `eat`, `jar`, `crumbs`, `burnt`, `oven`, `toss` on a Waydroid backend | ✅ done |
| **2** | `application/x-cookie-package` MIME type, `.desktop` launchers with icons, double-click to run | ✅ done |
| **3** | In-place updates that keep app data when signatures match, and warn loudly when they do not | ✅ done |
| **4** | **The oven**: Cookie's own runtime — container setup, binderfs mounting, one shared Android image — replacing Waydroid | 🟡 started |

Milestone 4's remaining work, in order: the display bridge, binder device
allocation, and Android properties. See [docs/oven.md](docs/oven.md).

The backend sits behind a trait (`AndroidBackend`), so the oven is a second
implementation rather than a rewrite — both are selectable today with
`--backend`.

---

## Development

```sh
cargo test                              # 277 tests
cargo clippy --all-targets              # clean
cargo fmt --check                       # clean
python3 tests/fixtures/make_fixtures.py # regenerate the test APKs

# after changing what kitchen setup writes:
COOKIE_REGENERATE_DIST=1 cargo test --test dist_files
```

The files in `dist/` are what a distribution package installs system-wide.
They are generated from the same code `cookie kitchen setup` uses, and a test
fails if the two ever drift apart.

The fixtures in `tests/fixtures/` are structurally real APKs — genuine zip
layout, genuine Android binary XML, genuine X.509 certificates, a genuine
PKCS#7 blob and a genuine APK Signing Block. They are built by a script that
implements those formats independently of Cookie's own readers, so the parsers
are checked against a second implementation rather than against themselves.
`signatures.json` records the certificate digests as computed by Python's
`hashlib`, and the tests assert Cookie arrives at the same values.

They contain no executable code, and their signatures are not
cryptographically meaningful — which is fine, because Cookie reads the signer
rather than verifying the signature.

### House rules

- No `unwrap()` or `expect()` on any path a user can reach. Tests may.
- Every error says what went wrong **and** what to do about it.
- Metadata from an APK is untrusted input. It gets escaped before it reaches a
  `.desktop` file, and sanitised before it becomes part of a filename.
- Binary parsers run over files the user did not write. Every read is
  bounds-checked, and a malformed file produces an error, never a panic.
- "Cannot tell" is never silently treated as "fine". An unreadable signature
  warns; it does not pass.
- Unfinished work says it is unfinished. The oven cannot show a window, so it
  reports that in `burnt` and refuses `launch` with an explanation, rather
  than failing in a way that looks like a bug.
- Never assume systemd.
- `sudo` only when it is genuinely required, and always with the reason.
- Cookie reads files the user gives it. It downloads nothing.

---

## Licence

**GPL-3.0-or-later.** Cookie borrows ideas from — and may yet borrow code
from — Waydroid, which is GPL-3.0, so Cookie is too.

Every dependency is permissively licensed and compatible:

| Licence | Crates |
|---|---|
| MIT OR Apache-2.0 | `anyhow`, `thiserror`, `clap`, `serde`, `toml`, `sha2`, `dirs`, and most of the tree |
| MIT | `zip` and four others |
| Zlib | `zlib-rs` |
| MPL-2.0 | `option-ext` (via `dirs`) |
| Unicode-3.0 | `unicode-ident` |

`option-ext` is the only copyleft dependency. It is plain MPL-2.0 **without**
the Exhibit B "Incompatible With Secondary Licenses" notice, so MPL-2.0 §3.3
permits distributing it as part of a GPL-3.0 work. **No conflicts.**

The cookie logo is a photograph supplied by the project owner.
