# The Cookie Package format (`.cpk`)

**Format version: 1**
**Status: stable for milestone 1. Version 2 will add what milestone 3 and 4 need.**

A `.cpk` — a *cookie* — holds one Android app and everything Cookie needs to
install it, launch it, and show it in a desktop menu like any other program.

---

## Why a zip

A cookie is a **zip archive**. The alternative considered was `tar.zst`.

Zip won for one reason that matters every day: **a zip has a central directory,
so any single entry can be read with one seek.** `cookie jar` lists installed
apps by reading `manifest.toml` and `icon.png` out of each cookie. With a zip
that costs a few kilobytes of I/O whether the app is 4 MB or 400 MB. With
`tar.zst` the whole stream has to be decompressed from the front to reach a
member, so listing twenty apps would mean decompressing gigabytes.

The usual argument for `tar.zst` is a better compression ratio — and here it
barely applies. The bulk of a cookie is APKs, which are themselves deflate
compressed zips. Recompressing them gains very little and costs real time on
every bake. Cookie therefore **stores** APKs and icons without recompression,
and deflates only `manifest.toml`.

Two smaller benefits follow from the same choice:

- Anyone can open a cookie with `unzip`, Ark, or a file manager, and see what
  is inside. No special tooling to debug a broken package.
- A stored APK sits at a known offset as an intact file, so a future backend
  can stream or memory-map it straight out of the cookie instead of extracting
  it first.

---

## Layout

```
teapot.cpk
├── manifest.toml                      # metadata (deflated)
├── icon.png                           # launcher icon (stored)
└── app/
    ├── base.apk                       # the base APK (stored)
    ├── split_config.arm64_v8a.apk     # split APKs, if any (stored)
    └── split_config.xxhdpi.apk
```

### Rules

- `manifest.toml` is **required** and must be at the archive root.
- `app/base.apk` is **required**. Its name inside the archive is always
  `app/base.apk`, whatever the source file was called.
- Split APKs live in `app/` and keep their original filenames, reduced to
  `[A-Za-z0-9._-]` with every other character replaced by `_`.
- The icon is optional in principle, but `cookie bake` always writes one: if
  the APK's launcher icon cannot be found, Cookie's own icon is used. Its
  entry name is `icon.png` or `icon.webp`, and `manifest.toml` says which.
- Entry names are never absolute and never contain `..`. A reader must
  nonetheless reduce every entry name to its final path component before
  writing it to disk, and Cookie does.

---

## `manifest.toml`

```toml
cpk_version = 1

[cookie]
name = "Teapot"
package = "com.example.teapot"
version_name = "2.1.0"
version_code = 4210
abis = ["arm64-v8a"]
icon = "icon.png"
min_sdk = 24
target_sdk = 34
launchable_activity = "com.example.teapot.MainActivity"

[baked]
by = "cookie 0.1.0"
at = "2026-11-18T23:01:10Z"
base_apk = "app/base.apk"
splits = ["app/split_config.arm64_v8a.apk"]
signature_sha256 = "ea185bf3dc83d0e9a71af5914bc81f22712a9b1900af579cd6be44ec3193a192"

[baked.files]
"app/base.apk" = "e1bbdaa7f1d70a899c1faa6c571dce8ac3d227232ccba0a44201b637d8483bed"
"app/split_config.arm64_v8a.apk" = "9f2c..."
```

### Top level

| Key | Type | Required | Meaning |
|---|---|---|---|
| `cpk_version` | integer | yes | Format version. `1` today. A reader **must** refuse a cookie whose version is higher than it understands, and say so. |

### `[cookie]` — what the app is

| Key | Type | Required | Meaning |
|---|---|---|---|
| `name` | string | yes | Display name, used by `cookie jar` and the desktop launcher. |
| `package` | string | yes | Android application id. Non-empty. This is the app's identity everywhere in Cookie. |
| `version_name` | string | no | Human version, e.g. `"2.1.0"`. |
| `version_code` | integer | no | Android's monotonic version counter. Milestone 3 compares this to decide whether a bake is an update. |
| `abis` | array of string | no, defaults `[]` | ABI directories found under `lib/` across the base and all splits. **Empty means the app has no native code and runs on any ABI** — it does not mean "unknown". |
| `icon` | string | no | Archive path of the icon. |
| `min_sdk` | integer | no | `android:minSdkVersion`. |
| `target_sdk` | integer | no | `android:targetSdkVersion`. |
| `launchable_activity` | string | no | Fully-qualified component of the first `MAIN`/`LAUNCHER` activity. Relative names in the APK (`.MainActivity`) are expanded against `package` before being written here. |

Optional keys are **omitted** when absent, never written as empty strings or
zeros.

### `[baked]` — how the cookie was made

| Key | Type | Required | Meaning |
|---|---|---|---|
| `by` | string | yes | Tool and version, e.g. `"cookie 0.1.0"`. |
| `at` | string | yes | Bake time, ISO 8601 UTC, e.g. `"2026-11-18T23:01:10Z"`. |
| `base_apk` | string | yes | Archive path of the base APK. Always `app/base.apk` in version 1. |
| `splits` | array of string | no, defaults `[]` | Archive paths of split APKs, sorted. |
| `signature_sha256` | string | no | SHA-256 over the base APK's v1 signing certificates, concatenated in sorted entry-name order. See below. |
| `files` | table | no, defaults `{}` | SHA-256 of each APK, keyed by archive path. |

#### About `signature_sha256`

This exists so milestone 3 can tell *"a newer build of the app you already
have"* from *"a different app wearing the same package id"*, and refuse to
replace one with the other silently.

It is the SHA-256 of every `META-INF/*.{RSA,DSA,EC}` entry in the base APK,
hashed in sorted entry-name order. That is APK Signature Scheme **v1**.

**An APK signed only with Scheme v2 or v3 has no such entry, and the key is
omitted.** A reader that finds it missing must warn that the signature could
not be checked rather than assume a match. Parsing the v2 signing block is
planned for version 2 of this format.

This value identifies a signer; it is **not** a verification of the signature.
Cookie does not currently verify that an APK's signature is valid — the
Android runtime does that at install time.

---

## Reading a cookie

1. Open the zip. If it is not a zip, it is not a cookie.
2. Read `manifest.toml`. If it is missing, it is not a cookie.
3. Parse it. Reject `cpk_version` of `0`, or greater than you support.
4. Reject an empty `package`.
5. To install: extract each path in `base_apk` then `splits`, **in that
   order**, reducing each entry name to its final component first. The base
   APK must be installed before its splits.

## Writing a cookie

Write to a temporary file beside the destination and rename it into place on
success, so an interrupted bake never leaves a half-written cookie where a
complete one is expected. Cookie uses `<name>.cpk.partial`.

---

## Reserved for future versions

These are **not** part of version 1. They are listed so that version 2 does
not collide with something a tool invented in the meantime.

| Name | Planned for |
|---|---|
| `data/` | Milestone 3: app data captured for backup or migration. |
| `[cookie].permissions` | Declared Android permissions, shown before install. |
| `[cookie].features` | `<uses-feature>` requirements, so `burnt` can flag a missing one. |
| `[baked].signature_v2` | Signature Scheme v2/v3 signer digest. |
| `[desktop]` | Overrides for the generated `.desktop` file. |

A version 1 reader encountering unknown keys **must ignore them**, not fail.

---

## MIME type and desktop integration

| | |
|---|---|
| MIME type | `application/x-cookie-package` |
| Extension | `.cpk` |
| Magic | `PK\x03\x04` (it is a zip) |
| Opened by | `cookie eat %f` |

Milestone 2 installs the XDG MIME definition and the `.desktop` file that
binds them, so double-clicking a cookie in Dolphin runs it.

Note that `.cpk` is not a globally unique extension — several games use it for
unrelated archive formats. The MIME definition therefore matches on both the
extension **and** the presence of `manifest.toml` at the archive root, so
Cookie never claims a file it cannot open.
