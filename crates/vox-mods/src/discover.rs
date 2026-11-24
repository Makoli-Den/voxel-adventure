//! Finding mods on disk: reads `<dir>/<id>/mod.json` for every subfolder that has one.
//!
//! Only manifests are read. No script runs and no asset is opened, so a broken or hostile mod
//! costs one small file read here.

use std::fs::{self, File};
use std::io::{self, ErrorKind, Read};
use std::path::{Path, PathBuf};

use crate::error::ModError;
use crate::manifest::{MAX_MANIFEST_BYTES, ModManifest};

/// The manifest file name inside a mod folder.
pub const MANIFEST_FILE: &str = "mod.json";

/// The folder of a mod that holds its resource pack, laid out like the built-in `assets/`.
pub const ASSETS_DIR: &str = "assets";

/// A mod found on disk: its validated manifest and where it lives.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscoveredMod {
    manifest: ModManifest,
    dir: PathBuf,
}

impl DiscoveredMod {
    /// The validated manifest. Its id equals the folder name.
    pub fn manifest(&self) -> &ModManifest {
        &self.manifest
    }

    /// The mod's folder, `<mods dir>/<id>`.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// The folder of the mod's resource pack, `<mod dir>/assets`. It may not exist.
    pub fn assets_dir(&self) -> PathBuf {
        self.dir.join(ASSETS_DIR)
    }
}

/// Finds the mods in `dir`: every immediate subfolder with a `mod.json`. Subfolders without one
/// and plain files are ignored. The result is ordered by folder name, so it is the same on every
/// machine.
///
/// **Links are never followed.** A mod folder or a `mod.json` that is a symbolic link or a
/// junction is an error ([`ModError::Symlink`]), so a mods folder cannot make the game read
/// outside it. Every I/O error except "not found" is reported with its path, never skipped.
///
/// All problems are collected and returned together, one per broken mod, so the player can fix
/// them in one pass.
///
/// # Errors
/// [`ModError`]s: the folder cannot be listed, a link is found, or a manifest cannot be read or
/// is invalid (including an id that differs from the folder name).
pub fn discover(dir: &Path) -> Result<Vec<DiscoveredMod>, Vec<ModError>> {
    let entries = fs::read_dir(dir).map_err(|source| {
        vec![ModError::ReadDir {
            dir: dir.to_owned(),
            source,
        }]
    })?;
    let mut folders: Vec<(String, PathBuf)> = Vec::new();
    let mut errors = Vec::new();
    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(source) => {
                errors.push(ModError::ReadDir {
                    dir: dir.to_owned(),
                    source,
                });
                continue;
            }
        };
        let path = entry.path();
        // `DirEntry::file_type` does not follow links, unlike `Path::is_dir`.
        match entry.file_type() {
            Ok(kind) if kind.is_symlink() => errors.push(ModError::Symlink { path }),
            Ok(kind) if kind.is_dir() => {
                let name = entry.file_name().to_string_lossy().into_owned();
                folders.push((name, path));
            }
            Ok(_) => {} // A plain file next to the mod folders.
            Err(source) => errors.push(ModError::Io { path, source }),
        }
    }
    folders.sort();

    let mut found = Vec::new();
    for (name, folder) in folders {
        let manifest_path = folder.join(MANIFEST_FILE);
        match check_manifest_file(&manifest_path).and_then(|present| {
            present
                .then(|| read_manifest(&manifest_path, &name))
                .transpose()
        }) {
            Ok(Some(manifest)) => {
                let assets = folder.join(ASSETS_DIR);
                match check_assets_dir(&assets) {
                    Ok(()) => found.push(DiscoveredMod {
                        manifest,
                        dir: folder,
                    }),
                    Err(error) => errors.push(error),
                }
            }
            Ok(None) => {} // No `mod.json`: not a mod.
            Err(error) => errors.push(error),
        }
    }
    if errors.is_empty() {
        Ok(found)
    } else {
        Err(errors)
    }
}

/// Whether `path` is a regular file to read: `Ok(false)` if it does not exist, an error for a
/// link, a folder, or any I/O error other than "not found".
fn check_manifest_file(path: &Path) -> Result<bool, ModError> {
    match fs::symlink_metadata(path) {
        Ok(meta) if meta.file_type().is_symlink() => Err(ModError::Symlink {
            path: path.to_owned(),
        }),
        Ok(meta) if meta.is_file() => Ok(true),
        Ok(_) => Err(ModError::NotAFile {
            path: path.to_owned(),
        }),
        Err(source) if source.kind() == ErrorKind::NotFound => Ok(false),
        Err(source) => Err(ModError::Io {
            path: path.to_owned(),
            source,
        }),
    }
}

/// Checks the mod's `assets` entry: absent is fine, a real directory is fine, a link (which
/// would let a mod's pack read outside its folder) or anything else is an error.
fn check_assets_dir(path: &Path) -> Result<(), ModError> {
    match fs::symlink_metadata(path) {
        Ok(meta) if meta.file_type().is_symlink() => Err(ModError::Symlink {
            path: path.to_owned(),
        }),
        Ok(meta) if meta.is_dir() => Ok(()),
        Ok(_) => Err(ModError::NotADirectory {
            path: path.to_owned(),
        }),
        Err(source) if source.kind() == ErrorKind::NotFound => Ok(()),
        Err(source) => Err(ModError::Io {
            path: path.to_owned(),
            source,
        }),
    }
}

/// Reads at most [`MAX_MANIFEST_BYTES`] + 1 bytes from `reader` (one more is enough to see
/// the manifest is too large), so a huge or endless file costs a bounded read.
fn read_bounded(reader: impl Read) -> io::Result<Vec<u8>> {
    let limit = u64::try_from(MAX_MANIFEST_BYTES).unwrap_or(u64::MAX);
    let mut bytes = Vec::new();
    reader
        .take(limit.saturating_add(1))
        .read_to_end(&mut bytes)?;
    Ok(bytes)
}

/// Reads and validates the manifest at `path` in the folder called `folder_name`.
fn read_manifest(path: &Path, folder_name: &str) -> Result<ModManifest, ModError> {
    let io_error = |source| ModError::Io {
        path: path.to_owned(),
        source,
    };
    let manifest_error = |problem| ModError::Manifest {
        path: path.to_owned(),
        problem,
    };
    let bytes = read_bounded(File::open(path).map_err(io_error)?).map_err(io_error)?;
    let manifest = ModManifest::parse(&bytes).map_err(manifest_error)?;
    manifest.check_folder(folder_name).map_err(manifest_error)?;
    Ok(manifest)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::ManifestProblem;

    /// A reader that counts the bytes handed out and never ends.
    struct Endless {
        served: usize,
    }

    impl Read for Endless {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            buf.fill(b' ');
            self.served += buf.len();
            Ok(buf.len())
        }
    }

    #[test]
    fn reading_is_bounded_even_for_an_endless_file() {
        let mut reader = Endless { served: 0 };
        let bytes = read_bounded(&mut reader).unwrap();
        assert_eq!(bytes.len(), MAX_MANIFEST_BYTES + 1);
        // The reader was not drained: at most one buffer past the limit was requested.
        assert!(
            reader.served <= 2 * (MAX_MANIFEST_BYTES + 1),
            "{}",
            reader.served
        );
        assert_eq!(
            ModManifest::parse(&bytes),
            Err(ManifestProblem::TooLarge {
                limit: MAX_MANIFEST_BYTES
            })
        );
    }

    #[test]
    fn a_file_at_the_limit_is_read_whole() {
        let bytes = read_bounded(&vec![b' '; MAX_MANIFEST_BYTES][..]).unwrap();
        assert_eq!(bytes.len(), MAX_MANIFEST_BYTES);
    }
}
