# The oven

Cookie's own Android runtime, replacing Waydroid.

**Status: the three known blockers are fixed. Still never booted.**

An earlier version of this file claimed the host side was complete. Comparing
what Cookie generates against Waydroid's `images.py`, `mount.py` and
`config_base` found three things that each stopped the container coming up.
All three are now fixed, and the fix is verified by a test that does real
loop and overlay mounts rather than by inspection.

What that does **not** mean is that it boots. It has never run against a
kernel with binder, two Android images, LXC and a Wayland session at the same
time. What is below is what is known, and what is still unknown.

Waydroid remains the **default** backend. `--backend oven` opts in.

---

## What was wrong, and what fixed it

### 1. One image, where two are needed — fixed

Waydroid mounts **two**: `system.img` as the container root, and
`vendor.img` at `rootfs/vendor`. Android's HALs live in `/vendor`, the
graphics ones included, so a system image alone boots into something with
nothing to draw with.

`cookie oven image add` now takes both, or a directory holding both:

```sh
cookie oven image add ~/waydroid-images            # finds system.img and vendor.img
cookie oven image add system.img vendor.img        # or name them
```

Starting without a vendor image is refused, with both ways to fix it.

### 2. A read-only root, where a writable overlay is needed — fixed

The container creates device nodes and mount points inside its own root, and
a read-only image cannot have those created in it:

```
$ mount -o loop,ro system.img rootfs/
$ touch rootfs/dev/binder
touch: cannot touch 'rootfs/dev/binder': Read-only file system
```

Every binder device, `/dev/sw_sync` and the `/run/xdg` tmpfs carrying the
Wayland socket are `create=file` or `create=dir` entries, so the display path
failed for the same reason the IPC path did.

The root is now layered, as Waydroid does it:

```
rootfs          overlay: lowerdir=overlay:rootfs, upper=overlay_rw/system
  system.img    read-only
rootfs/vendor   overlay: lowerdir=overlay/vendor:rootfs/vendor, upper=overlay_rw/vendor
  vendor.img    read-only
```

`tests/oven.rs` mounts this for real — two loop devices, two overlays —
creates each of the paths that used to fail, checks the writes landed in the
upper layer rather than the image, and takes it all apart again.

One deliberate divergence: Waydroid's `mount_overlay` passes `ro`, which
would defeat the purpose. Cookie mounts the overlay read-write, which is what
makes the layer usable.

### 3. Properties written where nothing reads them — fixed

Cookie wrote `/data/cookie.prop`. Waydroid bind-mounts its file to
`/vendor/waydroid.prop`, which is what the image reads. Every
`ro.hardware.*` value Cookie worked out was computed correctly and then
discarded.

The file is now bound to `/vendor/waydroid.prop`. That bind is only possible
*because* of the overlay above — on a bare read-only vendor image there would
be nowhere to create the file.

### Also fixed

- **Capabilities.** `lxc.cap.keep` now names the set Android's init needs —
  `sys_admin`, `mknod`, `sys_time`, `net_admin` and the rest. It is an
  allowlist, so LXC's default was dropping them.
- **Kernel filesystems.** `lxc.mount.auto = cgroup:ro sys:ro proc`.
- **`/dev`.** A tmpfs, with the basic nodes bound in from the host, since
  `lxc.autodev` is off and Android's ueventd expects somewhere to work.
- **cgroup version.** The device allowlist key differs between hierarchies,
  and the wrong one means no allowlist rather than an error. Cookie now
  detects which the host uses.
- **Host EGL libraries**, bound in when the host has them.
- **`stop()` unmounts.** It used to leave the loop device busy and the image
  pinned.

---

## What is still unknown

Honestly: whether it boots.

- **Android's init has never run under this configuration.** Everything
  above is necessary; whether it is sufficient is not something inspection
  can answer.
- **Waydroid's `config_nodes` and `config_session` could not be read** (both
  404 at the paths tried), so its `/dev` node setup and cgroup handling are
  unverified. Cookie's approach to both is reasoned rather than copied.
- **Waydroid ships a seccomp profile** (`waydroid.seccomp`); Cookie disables
  seccomp entirely. That is more permissive, so it should not block boot, but
  it is a difference.
- **Whether a given image reads `/vendor/waydroid.prop`** is still
  image-specific, as below.

If it does not boot, the configuration is at
`~/.local/share/cookie/oven/container.conf` and this runs it in the
foreground where the failure is visible:

```sh
lxc-start -n cookie -f ~/.local/share/cookie/oven/container.conf -F
```

---

## The display path

Four things, all of them the host's responsibility:

**1. The socket, where the image expects it.** `/run/xdg/wayland-0` inside
the container. Not an arbitrary path — it is the one a Waydroid-derived image
looks in, so such an image works unmodified. A tmpfs is mounted at
`/run/xdg` first, because there has to be something to mount the socket onto.
`XDG_RUNTIME_DIR` and `WAYLAND_DISPLAY` are set in the container so the HAL
finds it the way any Wayland client would.

**2. The GPU.** Each DRI render node is bind-mounted in, along with
`/dev/sw_sync`, which the composer needs to fence its buffers.

**3. Which implementation to load.** Android decides this from properties, and
only the host knows what hardware is present:

| What the host has | `ro.hardware.gralloc` | `ro.hardware.egl` |
|---|---|---|
| Its own Android HALs (a Halium device) | that HAL's name | the same |
| A DRI render node (an ordinary desktop) | `gbm`, plus `gralloc.gbm.device` | `mesa` |
| No GPU | `default` | `swiftshader` |

`cookie oven prop` shows what Cookie worked out and lets you override any of
it.

**4. One window per app.** `persist.waydroid.multi_windows=true`.

The property is read by the *image*, and the images anyone actually has are
Waydroid's — so the `waydroid`-namespaced name is the one that does the work.
Cookie sets `persist.cookie.multi_windows` alongside it for an image built
against Cookie later. An image that does not know a property ignores it, so
setting both costs nothing.

### What the host cannot determine

Whether the image carries a Wayland-capable hardware composer at all.

A LineageOS build made for Waydroid has one. A stock Android system image
does not, and with such an image the container boots and apps install, but
nothing is drawn. No amount of host-side inspection distinguishes them —
only booting does.

So Cookie states it rather than guessing:

```
$ cookie burnt --backend oven
  ✔ display  mesa on /dev/dri/renderD128, one window per app
  · image graphics  cannot be checked without booting the image
```

---

## Why LXC

**The oven replaces Waydroid, not LXC.**

Waydroid is a session manager: image handling, properties, window plumbing,
its own daemon. That is the part worth owning, because it decides how an app
feels on the desktop.

Running a container is not that part. LXC already does it, it is what
Waydroid itself uses, it is a plain binary with no opinion about init so it
behaves the same under OpenRC and systemd, and writing our own namespace and
`pivot_root` plumbing would mean a lot of `unsafe` to arrive back where LXC
already is.

Cookie owns the image, binderfs, the configuration, the properties, the
lifecycle and the app commands. `cookie oven recipe` prints the entirety of
what Cookie asks of LXC.

---

## The system image

Cookie does **not** download one. An Android system image is a large binary
from a third party, and deciding which one to trust is the user's call.

```sh
cookie oven image add /path/to/system.img   # register one
cookie oven image                           # show what is registered
cookie oven image remove                    # forget it (the file stays)
```

Cookie checks what it is handed and says precisely what is wrong when it is
not usable:

| What you gave it | What it says |
|---|---|
| ext4, squashfs or erofs | registered |
| An Android **sparse** image | tells you to run `simg2img` first, with the command |
| Something far too small | says the download is probably incomplete |
| Anything else | names the three formats it looks for |

Most system image downloads are sparse, so that second row is the one most
people meet.

The size is recorded at registration, so a file swapped underneath is noticed
rather than silently mounted.

---

## binderfs

Android IPC goes through the binder driver, which has been in mainline Linux
for years.

**Cookie never builds or loads a kernel module.** If the kernel was not built
with binder support, the only honest answer is which options to turn on:

```
CONFIG_ANDROID_BINDER_IPC=y   (or =m)
CONFIG_ANDROID_BINDERFS=y     (or =m)
```

On Gentoo those are under *Device Drivers → Android → Android Drivers*.

Cookie distinguishes four situations, because they need four different things
from you:

| State | What it means |
|---|---|
| Ready | binderfs is mounted, or legacy `/dev/binder` devices exist |
| Needs mounting | the kernel knows binderfs; Cookie can mount it |
| Needs the module | a binder module exists but is not loaded — `modprobe` it |
| Unsupported | the kernel cannot do this; rebuild it with the options above |

Mounting binderfs needs root, because mounting a filesystem does. Cookie
prints the exact command before asking for it, and runs that one command — it
keeps no privileged helper around.

### Creating the devices

A freshly mounted binderfs contains only `binder-control`. The devices an
Android container needs — `binder`, `hwbinder`, `vndbinder` — have to be
asked for, one at a time, with a `BINDER_CTL_ADD` ioctl on that node.

That is the only `unsafe` block in Cookie. It is confined to one function in
`backend/oven/binder.rs`, the struct layout and ioctl number are asserted
against the kernel ABI in tests, and the name is validated before the call:

```rust
const BINDER_CTL_ADD: u32 = ioc(3, b'b' as u32, 1, size_of::<BinderfsDevice>() as u32);
// == 0xC108_6201
```

---

## The container

`cookie oven recipe` prints the configuration without needing anything
installed, so you can check the setup before committing to it. It is built
from what your machine actually has.

Notable choices:

- **`lxc.init.cmd = /init`** — Android's own init is the container's first
  process.
- **Deny-all devices, then an explicit allowlist.** Nothing is exposed that is
  not named.
- **No user namespace.** Android's init expects real uids and binder ties
  handles to them. The container is confined by the device allowlist instead.
- **`lxc.namespace.keep = net`** — apps stay on the host's network, which is
  what someone running a single app expects.
- **AppArmor and seccomp off.** Android ships its own SELinux policy inside the
  image; LXC's would only fight it.

---

## Properties

```sh
cookie oven prop                              # what the oven will set, and where each came from
cookie oven prop set ro.sf.lcd_density 240    # override one
cookie oven prop unset ro.sf.lcd_density      # back to Cookie's answer
```

Properties are applied two ways, because Android treats them differently:

- **`ro.*`** are fixed while Android's init runs and cannot be changed
  afterwards. These go in a file Cookie writes into the shared data
  directory, which the container sees as `/data/cookie.prop`, before the
  container starts.
- **`persist.*`** can be set on a running system, and Cookie applies those
  with `setprop` once the container is up.

Which file an image reads at boot is image-specific. `ro.sf.lcd_density` is
the one most worth setting by hand: it decides how large everything in an app
is, and the right answer depends on your monitor rather than on anything
Cookie can work out.

---

## What is left

1. **Boot it on real hardware.** Every part of this is written and tested, and
   none of it has run against a kernel with binder, an Android image and a
   Wayland session at the same time. That is the next thing to do and it
   needs a machine, not more code.
2. **Window geometry.** Density is settable, but Cookie does not yet tell
   Android about window resizes, output scale or rotation.
3. **A Cookie-built image**, so `persist.cookie.multi_windows` is the name
   that does the work rather than the one kept for later.
4. **Dropping LXC**, if owning the namespace plumbing ever turns out to be
   worth the `unsafe` it costs. It may well not be.
