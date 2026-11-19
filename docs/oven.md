# The oven

Cookie's own Android runtime, replacing Waydroid.

**Status: started, not finished.** The oven can prepare and boot an Android
container. It cannot show an app's window yet. Waydroid remains the default
backend, and `--backend oven` opts in.

---

## What works

| Piece | State |
|---|---|
| Shared system image — register, check, mount | ✅ works |
| binderfs — detect, mount, expose devices | ✅ works |
| Container configuration | ✅ works |
| Container lifecycle — start, stop, status | ✅ works |
| App commands — install, remove, list | ✅ works, through `lxc-attach` |
| **Display bridge — app windows on Wayland** | ❌ **not built** |

## Why the display bridge is the hard part

An Android app does not draw to a window. It hands buffers to SurfaceFlinger,
which composites them through a hardware composer — a HAL that expects to end
at a display controller.

Putting those surfaces into Wayland windows means writing a hardware-composer
implementation that is a Wayland client: it takes Android's graphics buffers,
wraps them as `wl_buffer`s via `dmabuf`, and drives one Wayland surface per
Android task. It also has to carry input back the other way, and keep Android's
idea of display size, density and rotation in step with a window the user can
resize.

That is a substantial piece of native code, and it is the single largest thing
Waydroid provides. Until Cookie has its own, an app booted in the oven has
nowhere to put its window.

Cookie says so rather than appearing to work:

```
$ cookie burnt --backend oven
  ✔ system image  ~/android/system.img (ext4, 2.1 GiB)
  ✔ container engine  lxc is installed
  ✘ display bridge  not built yet, so the oven cannot show an app's window
```

---

## Why LXC

**The oven replaces Waydroid, not LXC.**

Waydroid is a whole session manager: image handling, Android properties,
window plumbing, its own daemon. That is the part Cookie wants to own, because
it is the part that decides how an app feels on the desktop.

Running a container is not that part. LXC already does it, it is what Waydroid
itself uses, it is a plain binary with no opinion about init so it behaves the
same under OpenRC and systemd, and writing our own namespace and `pivot_root`
plumbing would mean a lot of `unsafe` to arrive back where LXC already is.

So Cookie owns the image, binderfs, the configuration, the lifecycle and the
app commands. LXC is the engine underneath, and
`cookie oven recipe` prints the entirety of what Cookie asks of it.

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

Mounting binderfs genuinely needs root, because mounting a filesystem does.
Cookie prints the exact command before asking for it, and runs that one
command — it keeps no privileged helper around:

```
:: mounting binderfs at /dev/binderfs — this needs root, because mounting a filesystem does
     sudo mount -t binder binder /dev/binderfs
```

Cookie distinguishes four situations, because they need four different things
from you:

| State | What it means |
|---|---|
| Ready | binderfs is mounted, or legacy `/dev/binder` devices exist |
| Needs mounting | the kernel knows binderfs; Cookie can mount it |
| Needs the module | a binder module exists but is not loaded — `modprobe` it |
| Unsupported | the kernel cannot do this; rebuild it with the options above |

---

## The container

`cookie oven recipe` prints the configuration without needing anything
installed, so you can check the setup before committing to it. It is built
from what your machine actually has: your binder devices, your GPU render
nodes, your Wayland socket.

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

## What comes next

1. **The display bridge.** A hardware-composer implementation that is a
   Wayland client. Everything else waits on this.
2. **Binder device allocation.** Cookie currently mounts binderfs and expects
   the devices to be there. Allocating them from the host needs an `ioctl` on
   `binder-control`, which means a small amount of `unsafe`.
3. **Android properties.** The oven has no equivalent of
   `waydroid prop set` yet.
4. **Dropping LXC**, if owning the namespace plumbing ever turns out to be
   worth the `unsafe` it costs. It may well not be.
