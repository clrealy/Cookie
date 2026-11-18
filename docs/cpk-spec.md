# The Cookie Package format (`.cpk`)

**Format version: 2**
**Status: stable. Version 1 is still readable; see *Version history* at the end.**

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

- `manifest.toml` is **required**, must be at the archive root, and **must be
  the first entry in the archive**. See below for why.
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

### Why `manifest.toml` comes first

Reading a cookie never depends on entry order — a zip's central directory
gives every entry's offset, so any member is one seek away.

Desktop file-type detection does depend on it. The shared-mime-info rule that
identifies a cookie looks for the literal string `manifest.toml` at byte
offset 30, which is where a zip's **first** local file header puts its
filename. Written anywhere else, a cookie would only ever be recognised by its
extension — and that is not good enough, for the reasons under *MIME type*
below.

A writer must therefore emit `manifest.toml` first. In practice that means
hashing the APKs before opening the archive, since `[baked.files]` has to be
complete before the metadata is written. This costs no extra I/O: the digest
was always a separate read.

---

## `manifest.toml`

```toml
cpk_version = 2

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

[baked.signature]
schemes = ["v1", "v2", "v3"]
signers = ["5a63565ddd461e87b4364e5441978877a7e84c36063016fa674b12a22a34097d"]

[baked.files]
"app/base.apk" = "e1bbdaa7f1d70a899c1faa6c571dce8ac3d227232ccba0a44201b637d8483bed"
"app/split_config.arm64_v8a.apk" = "9f2c..."
```

### Top level

| Key | Type | Required | Meaning |
|---|---|---|---|
| `cpk_version` | integer | yes | Format version. `2` today. A reader **must** refuse a cookie whose version is higher than it understands, and say so. |

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
| `files` | table | no, defaults `{}` | SHA-256 of each APK, keyed by archive path. |

### `[baked.signature]` — who signed it

| Key | Type | Required | Meaning |
|---|---|---|---|
| `signers` | array of string | no, defaults `[]` | SHA-256 of each signing certificate's DER encoding, lowercase hex, sorted and unique. |
| `schemes` | array of string | no, defaults `[]` | Which schemes the certificates were found under: `"v1"`, `"v2"`, `"v3"`, `"v3.1"`. |

The whole table is **omitted** when nothing could be read. A reader that finds
it missing must treat the signer as *unknown* — never as *different*.

#### What the digest is, and why

This exists to tell *"a newer build of the app you already have"* from *"a
different app wearing the same package id"*. Android ties an app's data to the
key that signed it, so this is what decides whether an update may keep that
data.

Each entry is the SHA-256 of a signing certificate in its DER form — the same
value `apksigner verify --print-certs` prints as the *Signer #1 certificate
SHA-256 digest*, and the same one Android compares on install.

Certificates are collected from every scheme present:

| Scheme | Where the certificate lives |
|---|---|
| v1 (jar signing) | inside the PKCS#7 `SignedData` in `META-INF/*.{RSA,DSA,EC}` |
| v2, v3, v3.1 | the APK Signing Block, between the file entries and the central directory, as raw DER |

Comparison is an **intersection, not equality**: two APKs share a signer if
they have any certificate in common. An app may be signed by several keys, or
add or drop a scheme between releases, and any of those is still the same app.

**This is an identity, not a verification.** Cookie does not check that a
signature is cryptographically valid. Android does that at install time, and
duplicating it here would mean shipping a crypto stack to re-answer a question
the runtime already answers properly.

#### Known limitation: key rotation

Signature Scheme v3 lets an app rotate to a new signing key, carrying a
*proof-of-rotation* lineage that links the old key to the new one. Cookie does
not yet read that lineage, so an app that has rotated its key reads as a
signer mismatch. The install is refused with an explanation rather than going
ahead, which is the safe direction to be wrong in, and `--replace` is
available for a user who knows the rotation is genuine.

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
| `data/` | App data captured for backup or migration. |
| `[cookie].permissions` | Declared Android permissions, shown before install. |
| `[cookie].features` | `<uses-feature>` requirements, so `burnt` can flag a missing one. |
| `[baked.signature].lineage` | v3 proof-of-rotation, so a rotated key is recognised. |
| `[desktop]` | Overrides for the generated `.desktop` file. |

A reader encountering unknown keys **must ignore them**, not fail.

---

## Version history

### Version 2

Replaced `[baked].signature_sha256` with the `[baked.signature]` table.

The old field was the SHA-256 of the concatenated `META-INF/*.RSA` **file
bytes**. Those bytes are not a stable identity: the same key signing the same
app again can produce a different blob, because the signed attributes and the
signature value itself vary. It therefore could not answer the one question it
existed for. It also had nothing to say about APKs signed only under v2 or v3,
which is most modern ones.

Version 2 hashes the signing *certificate* instead, from whichever schemes are
present.

### Version 1

A version 1 cookie still opens, and everything else in it is read normally.
Its `signature_sha256` is read but never compared and never written back, so
such a cookie has **no usable signer identity** and is reported as unknown.
Re-bake it from the original APK to get one.

---

## MIME type and desktop integration

| | |
|---|---|
| MIME type | `application/x-cookie-package` |
| Extension | `.cpk` |
| Detected by | content: zip magic, then `manifest.toml` at offset 30 |
| Sub-class of | `application/zip` |
| Opened by | `cookie eat %f` |

`cookie kitchen setup` installs the shared-mime-info definition and the
`.desktop` file that binds them, so double-clicking a cookie runs it. A
packaged Cookie installs the same files system-wide; see `dist/README.md`.

### Detection is by content, with no filename glob

`.cpk` is not a globally unique extension — several games use it for unrelated
archive formats. The MIME definition therefore carries **no**
`<glob pattern="*.cpk"/>`, only magic.

This is not a theoretical concern, and the reason is not the obvious one.
shared-mime-info consults a glob *before* magic: where a glob matches, the
content is never examined. With a glob present, an unrelated zip named
`something.cpk` is reported as `application/x-cookie-package`, given Cookie's
icon, and handed to `cookie eat` on a double-click. Raising the magic
`priority` does not change this — the glob still wins. Both behaviours were
measured against `update-mime-database` and `gio info`, not assumed.

Content-based detection gives, measured the same way:

| File | Detected as |
|---|---|
| a cookie named `teapot.cpk` | `application/x-cookie-package` |
| a cookie with **no extension** | `application/x-cookie-package` |
| an unrelated zip named `impostor.cpk` | `application/zip` |
| an empty file named `empty.cpk` | `application/x-zerosize` |
| a cookie renamed `teapot.zip` | `application/zip` |

The second row is a bonus a glob could never provide. The last row is the
cost, and it is the right trade: a file named `.zip` being treated as a zip is
reasonable, whereas claiming someone else's file is a broken double-click on
their data.

The magic registers at priority 60, above `application/zip`'s own 50, so a
cookie is reported as a cookie rather than as the zip it also is.
