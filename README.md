<div align="center">

<img src="assets/cookie.png" alt="Cookie" width="160">

# Cookie 🍪

**Run Android apps on Linux like they belong there.**

One app, one ordinary desktop window. No Android home screen in the way.

</div>

---

> ## ⚠️ Work in progress — nothing here is finished
>
> Cookie is at **milestone 1 of 4**. Today it is a friendlier front end over
> [Waydroid](https://waydro.id/), which does the actual Android running. The
> commands work, the `.cpk` format is real, and `cookie burnt` will tell you
> honestly what your machine is missing.
>
> **What does not exist yet:** double-clicking a `.cpk` (milestone 2),
> in-place updates (milestone 3), and Cookie's own runtime — the oven —
> replacing Waydroid (milestone 4).
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
```

`waydroid init` is the one step that genuinely needs root, which is why Cookie
asks you to run it yourself rather than calling `sudo` behind your back.

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
cookie eat teapot.cpk              # install (if new) and launch
cookie eat teapot.cpk --no-launch  # install only
cookie eat teapot.cpk --reinstall  # install again even if it is already there
```

Warms the oven for you if it is cold, and turns on one-window-per-app mode
before launching.

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

### `cookie toss` — uninstall

```sh
cookie toss Teapot        # asks first
cookie toss Teapot --yes  # does not
```

---

## Where things live

Everything is under the XDG base directories in your home. Nothing Cookie
writes needs root.

| Path | What |
|---|---|
| `~/.local/share/cookie/jar/<package>/` | One directory per installed app: its manifest, icon and install record |
| `~/.cache/cookie/scratch/` | Scratch space while baking and installing; cleaned up automatically |
| `~/.cache/cookie/logs/oven.log` | Output from the last `cookie oven start` |

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

The full specification is in **[docs/cpk-spec.md](docs/cpk-spec.md)**.

---

## Roadmap

| | Milestone | State |
|---|---|---|
| **1** | `bake`, `eat`, `jar`, `crumbs`, `burnt`, `oven`, `toss` on a Waydroid backend | ✅ done |
| **2** | `application/x-cookie-package` MIME type, `.desktop` launchers with icons, double-click to run | ⬜ next |
| **3** | In-place updates that keep app data when signatures match, and warn loudly when they do not | ⬜ |
| **4** | **The oven**: Cookie's own runtime — container setup, binderfs mounting, one shared Android image — replacing Waydroid | ⬜ |

The backend already sits behind a trait (`AndroidBackend`), so milestone 4 is
a new implementation rather than a rewrite.

---

## Development

```sh
cargo test                              # 108 tests
cargo clippy --all-targets              # clean
python3 tests/fixtures/make_fixtures.py # regenerate the test APKs
```

The fixtures in `tests/fixtures/` are structurally real APKs — genuine zip
layout, genuine Android binary XML — built by a script that implements the
AXML format independently of Cookie's own reader, so the parser is checked
against a second implementation rather than against itself. They contain no
executable code.

### House rules

- No `unwrap()` or `expect()` on any path a user can reach. Tests may.
- Every error says what went wrong **and** what to do about it.
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
