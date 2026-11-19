//! The shared Android system image.
//!
//! One image serves every cookie, so the cost of an Android userspace is paid
//! once however many apps are installed.
//!
//! Cookie does not fetch images. It never has and this does not change that:
//! an Android system image is a large binary from a third party, and deciding
//! which one to trust is the user's call, not ours. What Cookie does is check
//! that what it has been handed is actually usable, and say precisely what is
//! wrong when it is not.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

/// Filesystem formats an Android image may be in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ImageFormat {
    /// The usual format for a LineageOS-derived `system.img`.
    Ext4,
    /// Read-only and compressed; common for vendor images.
    Squashfs,
    /// Android's newer read-only filesystem.
    Erofs,
}

impl ImageFormat {
    pub fn label(self) -> &'static str {
        match self {
            ImageFormat::Ext4 => "ext4",
            ImageFormat::Squashfs => "squashfs",
            ImageFormat::Erofs => "erofs",
        }
    }

    /// What to pass to `mount -t`.
    pub fn mount_type(self) -> &'static str {
        match self {
            ImageFormat::Ext4 => "ext4",
            ImageFormat::Squashfs => "squashfs",
            ImageFormat::Erofs => "erofs",
        }
    }
}

/// What an image turned out to be.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Inspection {
    /// A filesystem image that can be mounted as it is.
    Usable(ImageFormat),
    /// An Android *sparse* image. These are what most downloads contain, and
    /// they have to be expanded before anything can mount them.
    Sparse,
    /// Readable, but not a filesystem image we recognise.
    Unrecognised,
}

/// Where a registered image lives and what it is.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Image {
    /// Absolute path on the host.
    pub path: PathBuf,
    pub format: ImageFormat,
    /// Size in bytes at registration time, so a swapped file can be noticed.
    pub size: u64,
    /// When it was registered, as a Unix timestamp.
    pub registered_at: i64,
}

impl Image {
    /// Is the file still there, and still the one we registered?
    pub fn is_present(&self) -> bool {
        std::fs::metadata(&self.path)
            .map(|m| m.is_file() && m.len() == self.size)
            .unwrap_or(false)
    }
}

/// Smallest thing that could plausibly be an Android system image.
///
/// A real one is well over a gigabyte; this only catches an empty or
/// truncated file early, with a better message than a mount failure.
const MIN_PLAUSIBLE_SIZE: u64 = 16 * 1024 * 1024;

/// Work out what a file is.
pub fn inspect(path: &Path) -> Result<Inspection> {
    let mut file = File::open(path).with_context(|| format!("cannot open {}", path.display()))?;

    // Android sparse: magic at offset 0.
    let mut head = [0u8; 4];
    if file.read_exact(&mut head).is_err() {
        return Ok(Inspection::Unrecognised);
    }
    if u32::from_le_bytes(head) == 0xED26_FF3A {
        return Ok(Inspection::Sparse);
    }
    // squashfs: "hsqs" at offset 0.
    if &head == b"hsqs" {
        return Ok(Inspection::Usable(ImageFormat::Squashfs));
    }

    // erofs: magic at offset 1024.
    if let Some(magic) = read_u32_at(&mut file, 1024) {
        if magic == 0xE0F5_E1E2 {
            return Ok(Inspection::Usable(ImageFormat::Erofs));
        }
    }

    // ext2/3/4: magic in the superblock, 0x38 bytes into it, and the
    // superblock starts 1024 bytes in.
    if let Some(magic) = read_u16_at(&mut file, 1024 + 0x38) {
        if magic == 0xEF53 {
            return Ok(Inspection::Usable(ImageFormat::Ext4));
        }
    }

    Ok(Inspection::Unrecognised)
}

fn read_u32_at(file: &mut File, offset: u64) -> Option<u32> {
    file.seek(SeekFrom::Start(offset)).ok()?;
    let mut buf = [0u8; 4];
    file.read_exact(&mut buf).ok()?;
    Some(u32::from_le_bytes(buf))
}

fn read_u16_at(file: &mut File, offset: u64) -> Option<u16> {
    file.seek(SeekFrom::Start(offset)).ok()?;
    let mut buf = [0u8; 2];
    file.read_exact(&mut buf).ok()?;
    Some(u16::from_le_bytes(buf))
}

/// Check a file and turn it into a registered [`Image`].
pub fn register(path: &Path) -> Result<Image> {
    let path = path
        .canonicalize()
        .with_context(|| format!("cannot resolve {}", path.display()))?;

    let metadata =
        std::fs::metadata(&path).with_context(|| format!("cannot read {}", path.display()))?;
    if metadata.is_dir() {
        bail!(
            "{} is a directory.\n\
             Point Cookie at the image file itself, usually called system.img.",
            path.display()
        );
    }

    let size = metadata.len();
    if size < MIN_PLAUSIBLE_SIZE {
        bail!(
            "{} is only {}, which is far too small to be an Android system image.\n\
             A real one is well over a gigabyte. The download may be incomplete.",
            path.display(),
            crate::ui::bytes(size)
        );
    }

    match inspect(&path)? {
        Inspection::Usable(format) => Ok(Image {
            path,
            format,
            size,
            registered_at: crate::util::now_unix(),
        }),

        Inspection::Sparse => bail!(
            "{} is an Android *sparse* image, which cannot be mounted directly.\n\
             Most system image downloads are in this format. Expand it first:\n  \
             simg2img {} system.img\n\
             then register the result:\n  \
             cookie oven image add system.img\n\
             `simg2img` comes with android-tools on most distributions.",
            path.display(),
            path.display()
        ),

        Inspection::Unrecognised => bail!(
            "{} is not a filesystem image Cookie recognises.\n\
             It looks for ext4, squashfs and erofs, which is what Android system images use.\n\
             If this is an archive, extract it first; if it is a sparse image, run it through \
             simg2img.",
            path.display()
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::paths::ScratchDir;
    use std::io::Write;

    /// Build a file whose magic bytes say what it is.
    fn fake_image(dir: &Path, name: &str, write_magic: impl Fn(&mut [u8])) -> PathBuf {
        let mut data = vec![0u8; MIN_PLAUSIBLE_SIZE as usize + 4096];
        write_magic(&mut data);
        let path = dir.join(name);
        let mut file = File::create(&path).expect("create");
        file.write_all(&data).expect("write");
        path
    }

    fn ext4(data: &mut [u8]) {
        let at = 1024 + 0x38;
        data[at..at + 2].copy_from_slice(&0xEF53u16.to_le_bytes());
    }

    fn squashfs(data: &mut [u8]) {
        data[0..4].copy_from_slice(b"hsqs");
    }

    fn erofs(data: &mut [u8]) {
        data[1024..1028].copy_from_slice(&0xE0F5_E1E2u32.to_le_bytes());
    }

    fn sparse(data: &mut [u8]) {
        data[0..4].copy_from_slice(&0xED26_FF3Au32.to_le_bytes());
    }

    #[test]
    fn recognises_each_filesystem() {
        let scratch = ScratchDir::new("image").expect("scratch");
        for (name, magic, expected) in [
            ("system.img", ext4 as fn(&mut [u8]), ImageFormat::Ext4),
            ("vendor.img", squashfs, ImageFormat::Squashfs),
            ("erofs.img", erofs, ImageFormat::Erofs),
        ] {
            let path = fake_image(scratch.path(), name, magic);
            assert_eq!(
                inspect(&path).expect("inspect"),
                Inspection::Usable(expected),
                "{name}"
            );
            let image = register(&path).expect("register");
            assert_eq!(image.format, expected);
            assert!(image.is_present());
        }
    }

    #[test]
    fn a_sparse_image_is_recognised_and_explained() {
        let scratch = ScratchDir::new("image").expect("scratch");
        let path = fake_image(scratch.path(), "sparse.img", sparse);
        assert_eq!(inspect(&path).expect("inspect"), Inspection::Sparse);

        let err = register(&path).unwrap_err().to_string();
        assert!(err.contains("sparse"), "{err}");
        assert!(err.contains("simg2img"), "{err}");
    }

    #[test]
    fn an_unrecognised_file_is_rejected_with_advice() {
        let scratch = ScratchDir::new("image").expect("scratch");
        let path = fake_image(scratch.path(), "junk.img", |_| {});
        assert_eq!(inspect(&path).expect("inspect"), Inspection::Unrecognised);

        let err = register(&path).unwrap_err().to_string();
        assert!(err.contains("not a filesystem image"), "{err}");
        assert!(err.contains("ext4"), "{err}");
    }

    #[test]
    fn a_tiny_file_is_caught_before_the_magic_is_read() {
        let scratch = ScratchDir::new("image").expect("scratch");
        let path = scratch.path().join("truncated.img");
        std::fs::write(&path, b"not much here").expect("write");

        let err = register(&path).unwrap_err().to_string();
        assert!(err.contains("too small"), "{err}");
        assert!(err.contains("incomplete"), "{err}");
    }

    #[test]
    fn a_directory_is_rejected() {
        let scratch = ScratchDir::new("image").expect("scratch");
        let err = register(scratch.path()).unwrap_err().to_string();
        assert!(err.contains("is a directory"), "{err}");
        assert!(err.contains("system.img"), "{err}");
    }

    #[test]
    fn a_missing_file_is_rejected() {
        let err = register(Path::new("/nope/system.img"))
            .unwrap_err()
            .to_string();
        assert!(err.contains("cannot resolve"), "{err}");
    }

    #[test]
    fn a_swapped_file_is_noticed() {
        let scratch = ScratchDir::new("image").expect("scratch");
        let path = fake_image(scratch.path(), "system.img", ext4);
        let image = register(&path).expect("register");
        assert!(image.is_present());

        // Replace it with something a different size.
        std::fs::write(&path, b"swapped").expect("write");
        assert!(
            !image.is_present(),
            "a changed image must not pass as the registered one"
        );
    }

    #[test]
    fn inspecting_an_empty_file_does_not_panic() {
        let scratch = ScratchDir::new("image").expect("scratch");
        let path = scratch.path().join("empty.img");
        std::fs::write(&path, b"").expect("write");
        assert_eq!(inspect(&path).expect("inspect"), Inspection::Unrecognised);
    }

    #[test]
    fn mount_types_are_what_mount_expects() {
        assert_eq!(ImageFormat::Ext4.mount_type(), "ext4");
        assert_eq!(ImageFormat::Squashfs.mount_type(), "squashfs");
        assert_eq!(ImageFormat::Erofs.mount_type(), "erofs");
    }
}
