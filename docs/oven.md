# The oven

Cookie's own Android runtime, replacing Waydroid.

**Status: written, and known not to boot yet.**

An earlier version of this file said the host side was complete. Comparing
what Cookie generates against Waydroid's own `images.py`, `mount.py` and
`config_base` found three things it is missing, each of which stops the
container coming up. They are listed under *Known blockers* below, with what
each one needs.

What is right is the content: the socket path, the graphics property values,
binderfs handling, image validation. What is wrong is the container's
filesystem layout, and until that is fixed none of the rest gets a chance to
run.

Waydroid remains the **default** backend. `--backend oven` opts in.

---

## Known blockers

Each of these was found by comparing against Waydroid's working
implementation, not guessed at.

### 1. One image, where two are needed

Waydroid's `mount_rootfs()` mounts **two**: `system.img` as the container
root, and `vendor.img` at `rootfs/vendor`.

Cookie registers exactly one. Android's HALs live in `/vendor` — including
the graphics ones that are the whole point — so without the vendor image
there is nothing for SurfaceFlinger to load.

**Needs:** `cookie oven image add` to take a vendor image too, and the mount
to place it at `rootfs/vendor`.

### 2. A read-only root, where a writable overlay is needed

Cookie mounts the system image `-o loop,ro` and stops there. Waydroid mounts
an overlayfs on top of it:

```
lowerdir = <overlay>:<rootfs>     upperdir = <overlay_rw>/system
```

…and the same again for `vendor`.

This is not cosmetic. On a read-only root, binding onto a directory that
already exists in the image works, but **creating** a path does not — and
every binder device, `/dev/sw_sync` and the `/run/xdg` tmpfs that carries the
Wayland socket are `create=file` or `create=dir` entries:

```
$ mount -o loop,ro system.img rootfs/
$ touch rootfs/dev/binder
touch: cannot touch 'rootfs/dev/binder': Read-only file system
```

So the display path fails for the same reason the IPC path does.

**Needs:** an overlay, or at minimum a tmpfs on `/dev` — Cookie sets
`lxc.autodev = 0` and then never mounts one.

### 3. Properties written where nothing reads them

Cookie writes `/data/cookie.prop`. Waydroid bind-mounts its file to
**`/vendor/waydroid.prop`**:

```python
bind_file(args, args.work + "/waydroid.prop",
          defaults["rootfs"] + "/vendor/waydroid.prop")
```

`/data/cookie.prop` is read by nothing, so every `ro.hardware.*` value Cookie
works out is computed correctly and then discarded.

**Needs:** the same bind, to the path the image actually reads.

### Also missing, less certain

- **Capabilities.** Waydroid's `config_base` keeps an explicit set of ~24
  (`sys_admin`, `mknod`, `sys_time`, `net_admin`, `sys_ptrace` and more).
  Cookie sets none, so the container gets LXC's default, which drops things
  Android's init wants.
- **`lxc.mount.auto = cgroup:ro sys:ro proc`.** Waydroid sets this; Cookie
  leaves `/proc`, `/sys` and cgroups to LXC's defaults.
- **Host EGL libraries.** Waydroid bind-mounts the host's `/vendor/lib/egl`
  and `/vendor/lib64/egl` in when they exist.
- **The device allowlist.** Cookie writes `lxc.cgroup2.devices.*`, which
  assumes cgroup v2 and which Waydroid does not appear to do at all — its
  `/dev` nodes come from a `config_nodes` file this comparison could not
  read.
- **`stop()` leaves the image mounted.** Nothing unmounts the loop device or
  binderfs afterwards.

### What this comparison could not check

`config_nodes`, `config_session` and `drivers.py` were not readable, so the
`/dev` node setup, the cgroup handling and Waydroid's own binder setup are
unverified. There may be more.

One worry turned out to be unfounded: nothing in Waydroid sets
`androidboot.*` parameters, so the container's `/proc/cmdline` belonging to
the host kernel is apparently not a problem.

---

## A correction

The previous version of this document said the remaining work was for Cookie
to write "a hardware-composer implementation that is itself a Wayland
client", and called that the largest piece Waydroid provides.

That was wrong about where the code goes, and the mistake is worth recording
because it changed what milestone 4 even *was*.

Reading Waydroid's `tools/helpers/lxc.py` settles it. The host side
bind-mounts the desktop's Wayland socket into the container:

```python
wayland_host_socket = os.path.realpath(os.path.join(
    session["xdg_runtime_dir"], session["wayland_display"]))
wayland_container_socket = os.path.join(
    tools.config.defaults["container_xdg_runtime_dir"],      # /run/xdg
    tools.config.defaults["container_wayland_display"])      # wayland-0
make_entry(wayland_host_socket, wayland_container_socket[1:])
```

…and that is the whole of it. There is no host-side compositor, no proxy, no
socket forwarding. The host mounts a socket in and starts the container.

Which means **the Wayland client is inside the Android image** — it is the
hardware composer, compiled into the image, connecting *out* through that
socket. The host's job is not to write one. The host's job is to put the
socket where that HAL looks for it, pass the GPU through, and tell it which
implementation to load.

All of that is host-side, finite, and now done.

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
