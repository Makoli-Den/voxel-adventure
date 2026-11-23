//! Packs: where resources come from. A [`DirPack`] is one directory, a [`PackStack`] layers
//! several of them so that a higher pack overrides a lower one, file by file.

use std::fs;
use std::io;
use std::path::{Component, Path, PathBuf};

use crate::ResourceId;
use crate::error::ResourceError;
use crate::id::PATH_SEPARATOR;

/// A directory holding resources: `ns:a/b.png` is the file `<root>/ns/a/b.png`.
///
/// Packs are trusted local folders: ids cannot leave the root through `..`, but a symlink
/// inside a pack can.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirPack {
    root: PathBuf,
}

impl DirPack {
    /// A pack rooted at `root`. The directory is not touched until a resource is read, so a
    /// missing root simply holds nothing.
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// The directory of this pack, for messages.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Whether the root is an existing directory. Reads never fail for a missing root, they
    /// treat it as an empty pack, so startup code that wants to warn about (or reject) a
    /// mistyped `--pack` path calls this.
    pub fn exists(&self) -> bool {
        self.root.is_dir()
    }

    /// Reads the file for `id`. A missing file is `Ok(None)`; every other failure (including
    /// a directory where a file is expected) is an error.
    ///
    /// # Errors
    /// [`ResourceError::Io`] when the file exists but cannot be read.
    pub fn read(&self, id: &ResourceId) -> Result<Option<Vec<u8>>, ResourceError> {
        let Some(path) = self.file_path(id) else {
            return Ok(None);
        };
        match fs::read(path) {
            Ok(bytes) => Ok(Some(bytes)),
            Err(error) if is_missing(&error) => Ok(None),
            Err(source) => Err(ResourceError::Io {
                id: id.clone(),
                source,
            }),
        }
    }

    /// Whether the pack has an entry for `id`, without reading it. Agrees with
    /// [`DirPack::read`]: an entry that exists but cannot be read counts as present.
    ///
    /// # Errors
    /// [`ResourceError::Io`] when the lookup fails for a reason other than absence.
    pub fn contains(&self, id: &ResourceId) -> Result<bool, ResourceError> {
        let Some(path) = self.file_path(id) else {
            return Ok(false);
        };
        match fs::metadata(path) {
            Ok(_) => Ok(true),
            Err(error) if is_missing(&error) => Ok(false),
            Err(source) => Err(ResourceError::Io {
                id: id.clone(),
                source,
            }),
        }
    }

    /// The file `id` names inside this pack, or `None` if it would not be under the root.
    /// A validated id never gives `None`, so that is an internal bug: loud in debug builds, and
    /// in release the resource is treated as absent, which is the safe answer.
    fn file_path(&self, id: &ResourceId) -> Option<PathBuf> {
        let relative = format!("{}{PATH_SEPARATOR}{}", id.namespace(), id.path());
        let path = confined_path(&self.root, &relative);
        debug_assert!(path.is_some(), "validated id `{id}` escapes the pack root");
        path
    }
}

/// Joins `relative` to `root` only if it is made of plain names, so the result is lexically
/// under `root`. Valid ids always pass; this is the second line of defence if the id rules
/// ever loosen. Absolute paths, drive prefixes and `..` components give `None`.
fn confined_path(root: &Path, relative: &str) -> Option<PathBuf> {
    let relative = Path::new(relative);
    let plain = !relative.as_os_str().is_empty()
        && relative
            .components()
            .all(|part| matches!(part, Component::Normal(_)));
    if !plain {
        return None;
    }
    Some(root.join(relative))
}

/// A missing file, or a file where a directory of the path should be: either way the pack
/// does not have the resource.
fn is_missing(error: &io::Error) -> bool {
    matches!(
        error.kind(),
        io::ErrorKind::NotFound | io::ErrorKind::NotADirectory
    )
}

/// Packs ordered by priority, lowest first. A resource is served by the highest pack that has
/// it; files are never merged, so a pack can replace one texture and leave the rest to the
/// packs below.
///
/// ```
/// use vox_resources::{DirPack, PackStack, ResourceError, ResourceId};
///
/// let mut stack = PackStack::new();
/// stack.push(DirPack::new("assets")); // built-in, lowest
/// stack.push(DirPack::new("my_pack")); // overrides it
/// let id = ResourceId::parse("core:shaders/chunk.frag").unwrap();
/// // Neither directory exists here, and a missing root holds nothing.
/// assert!(matches!(stack.read(&id), Err(ResourceError::NotFound { .. })));
/// ```
#[derive(Debug, Default)]
pub struct PackStack {
    packs: Vec<DirPack>,
}

impl PackStack {
    /// An empty stack; every lookup is `NotFound` until a pack is pushed.
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds `pack` above all packs already in the stack.
    pub fn push(&mut self, pack: DirPack) {
        self.packs.push(pack);
    }

    /// The number of packs.
    pub fn len(&self) -> usize {
        self.packs.len()
    }

    /// Whether the stack has no packs.
    pub fn is_empty(&self) -> bool {
        self.packs.is_empty()
    }

    /// Reads `id` from the highest pack that has it.
    ///
    /// An error in a higher pack is returned as is and is not hidden by a lower pack: a broken
    /// override should be seen, not silently replaced.
    ///
    /// # Errors
    /// [`ResourceError::NotFound`] when no pack has it, [`ResourceError::Io`] when reading fails.
    pub fn read(&self, id: &ResourceId) -> Result<Vec<u8>, ResourceError> {
        for pack in self.packs.iter().rev() {
            if let Some(bytes) = pack.read(id)? {
                return Ok(bytes);
            }
        }
        Err(ResourceError::NotFound { id: id.clone() })
    }

    /// Like [`PackStack::read`], for UTF-8 text.
    ///
    /// # Errors
    /// As [`PackStack::read`]; bytes that are not UTF-8 give [`ResourceError::Io`] with kind
    /// `InvalidData`.
    pub fn read_to_string(&self, id: &ResourceId) -> Result<String, ResourceError> {
        let bytes = self.read(id)?;
        String::from_utf8(bytes).map_err(|error| ResourceError::Io {
            id: id.clone(),
            source: io::Error::new(io::ErrorKind::InvalidData, error),
        })
    }

    /// The pack that serves `id`, for messages (`DirPack::root` names it).
    ///
    /// # Errors
    /// [`ResourceError::NotFound`] when no pack has it, [`ResourceError::Io`] when a lookup fails.
    pub fn source_of(&self, id: &ResourceId) -> Result<&DirPack, ResourceError> {
        for pack in self.packs.iter().rev() {
            if pack.contains(id)? {
                return Ok(pack);
            }
        }
        Err(ResourceError::NotFound { id: id.clone() })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures");

    fn id(text: &str) -> ResourceId {
        ResourceId::parse(text).unwrap()
    }

    fn low() -> DirPack {
        DirPack::new(format!("{FIXTURES}/low"))
    }

    #[test]
    fn dir_pack_reads_a_file_and_reports_a_missing_one_as_none() {
        let pack = low();
        assert_eq!(
            pack.read(&id("base:a.txt")).unwrap(),
            Some(b"low a".to_vec())
        );
        assert_eq!(pack.read(&id("base:nope.txt")).unwrap(), None);
        assert_eq!(pack.read(&id("other:a.txt")).unwrap(), None);
        assert_eq!(pack.read(&id("base:a.txt/inner")).unwrap(), None);
        assert!(pack.contains(&id("base:a.txt")).unwrap());
        assert!(!pack.contains(&id("base:nope.txt")).unwrap());
    }

    #[test]
    fn a_missing_root_holds_nothing() {
        let pack = DirPack::new(format!("{FIXTURES}/does-not-exist"));
        assert_eq!(pack.read(&id("base:a.txt")).unwrap(), None);
    }

    #[test]
    fn a_directory_where_a_file_is_expected_is_io_not_missing() {
        let pack = low();
        assert!(matches!(
            pack.read(&id("base:folder")),
            Err(ResourceError::Io { .. })
        ));
        assert!(pack.contains(&id("base:folder")).unwrap());
    }

    #[test]
    fn confined_path_accepts_plain_names_only() {
        let root = Path::new("root");
        assert_eq!(
            confined_path(root, "ns/a/b.png"),
            Some(root.join("ns").join("a").join("b.png"))
        );
        for bad in [
            "",
            "../x",
            "ns/../../x",
            "/etc/passwd",
            "ns//a/..",
            "C:/windows",
            "C:x",
            "\\\\server\\share\\x",
        ] {
            // On Unix a backslash and a drive-like `C:` are plain file name characters.
            if !cfg!(windows) && bad.contains(['\\', ':']) {
                continue;
            }
            let result = confined_path(root, bad);
            assert_eq!(result, None, "`{bad}` must not be accepted");
        }
    }

    #[test]
    fn exists_tells_a_real_root_from_a_missing_or_file_root() {
        assert!(low().exists());
        assert!(!DirPack::new(format!("{FIXTURES}/does-not-exist")).exists());
        assert!(!DirPack::new(format!("{FIXTURES}/low/base/a.txt")).exists());
    }

    #[test]
    fn a_failing_lookup_is_io_for_both_read_and_contains() {
        // An interior NUL is not a valid path on any platform, so the OS call fails with
        // something other than "missing".
        let pack = DirPack::new("bad root");
        assert!(matches!(
            pack.read(&id("base:a.txt")),
            Err(ResourceError::Io { .. })
        ));
        assert!(matches!(
            pack.contains(&id("base:a.txt")),
            Err(ResourceError::Io { .. })
        ));
    }

    /// Pins today's behaviour: ids cannot escape the root, but a symlink inside a pack is
    /// followed (packs are trusted local folders, see decision 0013 Risks).
    #[cfg(unix)]
    #[test]
    fn a_symlink_inside_a_pack_is_followed() {
        let dir =
            std::env::temp_dir().join(format!("vox-resources-symlink-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("pack/base")).unwrap();
        fs::write(dir.join("outside.txt"), "outside").unwrap();
        std::os::unix::fs::symlink(dir.join("outside.txt"), dir.join("pack/base/link.txt"))
            .unwrap();
        let pack = DirPack::new(dir.join("pack"));
        let read = pack.read(&id("base:link.txt"));
        fs::remove_dir_all(&dir).unwrap();
        assert_eq!(read.unwrap(), Some(b"outside".to_vec()));
    }

    #[test]
    fn empty_stack_finds_nothing() {
        let stack = PackStack::new();
        assert!(stack.is_empty());
        assert!(matches!(
            stack.read(&id("base:a.txt")),
            Err(ResourceError::NotFound { .. })
        ));
    }
}
