//! `discover` against real folders: the fixtures under `tests/fixtures`, plus folders written
//! into the cargo target directory for the cases a fixture cannot hold (a huge file).

#![allow(clippy::unwrap_used)]

use std::fs;
use std::path::{Path, PathBuf};

use vox_mods::{
    MANIFEST_FILE, MAX_MANIFEST_BYTES, MOD_API_VERSION, ManifestProblem, ModError, discover,
    resolve,
};

fn fixtures(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join(name)
}

/// A fresh empty folder under the target directory, named after the test.
fn scratch(name: &str) -> PathBuf {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join(name);
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn finds_the_chain_in_folder_order_and_skips_non_mods() {
    let found = discover(&fixtures("chain")).unwrap();
    let names: Vec<&str> = found.iter().map(|m| m.manifest().id().as_str()).collect();
    // `not_a_mod` (no manifest) and `notes.txt` (a file) are ignored.
    assert_eq!(names, ["base", "gems", "ruby"]);
    assert!(found[0].dir().ends_with("base"));
    assert!(
        found[0].assets_dir().ends_with("base/assets")
            || found[0].assets_dir().ends_with("base\\assets")
    );
}

#[test]
fn the_chain_resolves_in_dependency_order() {
    let manifests: Vec<_> = discover(&fixtures("chain"))
        .unwrap()
        .iter()
        .map(|m| m.manifest().clone())
        .collect();
    let order = resolve(&manifests, MOD_API_VERSION).unwrap();
    let names: Vec<&str> = order.iter().map(|id| id.as_str()).collect();
    assert_eq!(names, ["base", "ruby", "gems"]);
}

#[test]
fn a_full_manifest_is_read_from_disk() {
    let found = discover(&fixtures("chain")).unwrap();
    let gems = found[1].manifest();
    assert_eq!(gems.version().to_string(), "2.3.4");
    assert_eq!(gems.entry().unwrap().to_string(), "gems:scripts/main.lua");
    assert_eq!(gems.capabilities().len(), 2);
}

#[test]
fn every_broken_mod_is_reported_and_the_good_one_does_not_hide_them() {
    let errors = discover(&fixtures("broken")).unwrap_err();
    assert_eq!(errors.len(), 2, "{errors:?}");
    // Ordered by folder name: `badjson`, then `wrongname` (`fine` is fine).
    assert!(matches!(
        &errors[0],
        ModError::Manifest { path, problem: ManifestProblem::Json(_) } if path.ends_with("mod.json")
    ));
    assert!(matches!(
        &errors[1],
        ModError::Manifest {
            problem: ManifestProblem::IdDiffersFromFolder { .. },
            ..
        }
    ));
    let message = errors[1].to_string();
    assert!(
        message.contains("other") && message.contains("wrongname"),
        "{message}"
    );
}

#[test]
fn a_missing_folder_is_an_error_naming_it() {
    let errors = discover(&fixtures("does_not_exist")).unwrap_err();
    assert!(matches!(errors[..], [ModError::ReadDir { .. }]));
    assert!(errors[0].to_string().contains("does_not_exist"));
}

#[test]
fn an_empty_folder_has_no_mods() {
    assert!(discover(&scratch("discover_empty")).unwrap().is_empty());
}

#[test]
fn an_oversized_manifest_is_refused_without_being_parsed() {
    let root = scratch("discover_oversized");
    let folder = root.join("huge");
    fs::create_dir_all(&folder).unwrap();
    // A valid manifest padded with whitespace past the limit.
    let mut text =
        String::from(r#"{ "format": 1, "id": "huge", "version": "1.0.0", "api": "1.0" }"#);
    text.push_str(&" ".repeat(MAX_MANIFEST_BYTES));
    fs::write(folder.join(MANIFEST_FILE), text).unwrap();
    let errors = discover(&root).unwrap_err();
    assert!(matches!(
        &errors[..],
        [ModError::Manifest {
            problem: ManifestProblem::TooLarge { .. },
            ..
        }]
    ));
}

#[test]
fn a_manifest_that_is_not_utf8_is_a_manifest_error() {
    let root = scratch("discover_not_utf8");
    let folder = root.join("bin");
    fs::create_dir_all(&folder).unwrap();
    fs::write(folder.join(MANIFEST_FILE), [0xff, 0xfe, 0x00]).unwrap();
    let errors = discover(&root).unwrap_err();
    assert!(matches!(
        &errors[..],
        [ModError::Manifest {
            problem: ManifestProblem::Json(_),
            ..
        }]
    ));
}

const VALID_MANIFEST: &str = r#"{ "format": 1, "id": "NAME", "version": "1.0.0", "api": "1.0" }"#;

/// A manifest for the mod called `name`.
fn manifest_text(name: &str) -> String {
    VALID_MANIFEST.replace("NAME", name)
}

/// Creates a directory link, or returns `false` when the OS refuses (Windows without developer
/// mode or privilege), so the caller can skip the test instead of failing.
fn make_dir_link(target: &Path, link: &Path) -> bool {
    #[cfg(unix)]
    let result = std::os::unix::fs::symlink(target, link);
    #[cfg(windows)]
    let result =
        std::os::windows::fs::symlink_dir(target, link).or_else(|_| make_junction(target, link));
    #[cfg(not(any(unix, windows)))]
    let result: std::io::Result<()> = Err(std::io::ErrorKind::Unsupported.into());
    result.is_ok()
}

/// A Windows junction, which needs no privilege (unlike a symbolic link), through `mklink /J`.
#[cfg(windows)]
fn make_junction(target: &Path, link: &Path) -> std::io::Result<()> {
    let status = std::process::Command::new("cmd")
        .args(["/C", "mklink", "/J"])
        .arg(link)
        .arg(target)
        .stdout(std::process::Stdio::null())
        .status()?;
    if status.success() {
        Ok(())
    } else {
        Err(std::io::ErrorKind::PermissionDenied.into())
    }
}

/// Same for a file link.
fn make_file_link(target: &Path, link: &Path) -> bool {
    #[cfg(unix)]
    let result = std::os::unix::fs::symlink(target, link);
    #[cfg(windows)]
    let result = std::os::windows::fs::symlink_file(target, link);
    #[cfg(not(any(unix, windows)))]
    let result: std::io::Result<()> = Err(std::io::ErrorKind::Unsupported.into());
    result.is_ok()
}

#[test]
fn a_symlinked_mod_folder_is_reported_and_not_followed() {
    let root = scratch("discover_link_folder");
    let outside = scratch("discover_link_folder_outside").join("real");
    fs::create_dir_all(&outside).unwrap();
    fs::write(outside.join(MANIFEST_FILE), manifest_text("linked")).unwrap();
    if !make_dir_link(&outside, &root.join("linked")) {
        eprintln!("skipped: the OS refused to create a directory link");
        return;
    }
    let errors = discover(&root).unwrap_err();
    assert!(
        matches!(&errors[..], [ModError::Symlink { path }] if path.ends_with("linked")),
        "{errors:?}"
    );
    assert!(errors[0].to_string().contains("not followed"));
}

#[test]
fn a_symlinked_manifest_is_reported_even_when_it_dangles() {
    let root = scratch("discover_link_manifest");
    let folder = root.join("m");
    fs::create_dir_all(&folder).unwrap();
    let missing = root.join("nowhere.json");
    if !make_file_link(&missing, &folder.join(MANIFEST_FILE)) {
        eprintln!("skipped: the OS refused to create a file link");
        return;
    }
    let errors = discover(&root).unwrap_err();
    assert!(
        matches!(&errors[..], [ModError::Symlink { path }] if path.ends_with(MANIFEST_FILE)),
        "{errors:?}"
    );
}

#[test]
fn a_manifest_that_is_a_folder_is_reported() {
    let root = scratch("discover_manifest_dir");
    fs::create_dir_all(root.join("m").join(MANIFEST_FILE)).unwrap();
    let errors = discover(&root).unwrap_err();
    assert!(
        matches!(&errors[..], [ModError::NotAFile { path }] if path.ends_with(MANIFEST_FILE)),
        "{errors:?}"
    );
    assert!(errors[0].to_string().contains("not a regular file"));
}

#[test]
fn an_unreadable_manifest_is_an_io_error_with_its_path() {
    // A real read failure (permissions) cannot be made portably in a test, so this pins the
    // message and the "no duplicate cause" rule of the error type; the code path returns
    // `ModError::Io` for every error kind except NotFound.
    let error = ModError::Io {
        path: PathBuf::from("mods/x/mod.json"),
        source: std::io::Error::from(std::io::ErrorKind::PermissionDenied),
    };
    let message = error.to_string();
    assert!(message.contains("mods/x/mod.json"), "{message}");
    assert!(std::error::Error::source(&error).is_none());
}

#[cfg(unix)]
#[test]
fn a_folder_name_that_is_not_utf8_is_an_id_mismatch_not_a_panic() {
    use std::ffi::OsStr;
    use std::os::unix::ffi::OsStrExt;
    let root = scratch("discover_non_utf8_folder");
    let folder = root.join(OsStr::from_bytes(&[b'm', 0xff]));
    fs::create_dir_all(&folder).unwrap();
    fs::write(folder.join(MANIFEST_FILE), manifest_text("m")).unwrap();
    let errors = discover(&root).unwrap_err();
    assert!(matches!(
        &errors[..],
        [ModError::Manifest {
            problem: ManifestProblem::IdDiffersFromFolder { .. },
            ..
        }]
    ));
}

#[test]
fn a_bom_in_a_manifest_file_is_accepted() {
    let root = scratch("discover_bom");
    let folder = root.join("bommed");
    fs::create_dir_all(&folder).unwrap();
    let mut bytes = vec![0xEF, 0xBB, 0xBF];
    bytes.extend_from_slice(manifest_text("bommed").as_bytes());
    fs::write(folder.join(MANIFEST_FILE), bytes).unwrap();
    assert_eq!(discover(&root).unwrap().len(), 1);
}

#[test]
fn a_symlinked_assets_folder_is_reported_and_not_followed() {
    let root = scratch("discover_link_assets");
    let outside = scratch("discover_link_assets_outside").join("real");
    fs::create_dir_all(&outside).unwrap();
    let folder = root.join("m");
    fs::create_dir_all(&folder).unwrap();
    fs::write(folder.join(MANIFEST_FILE), manifest_text("m")).unwrap();
    if !make_dir_link(&outside, &folder.join("assets")) {
        eprintln!("skipped: the OS refused to create a directory link");
        return;
    }
    let errors = discover(&root).unwrap_err();
    assert!(
        matches!(&errors[..], [ModError::Symlink { path }] if path.ends_with("assets")),
        "{errors:?}"
    );
}

#[test]
fn an_assets_entry_that_is_a_file_is_reported() {
    let root = scratch("discover_assets_file");
    let folder = root.join("m");
    fs::create_dir_all(&folder).unwrap();
    fs::write(folder.join(MANIFEST_FILE), manifest_text("m")).unwrap();
    fs::write(folder.join("assets"), "x").unwrap();
    let errors = discover(&root).unwrap_err();
    assert!(
        matches!(&errors[..], [ModError::NotADirectory { path }] if path.ends_with("assets")),
        "{errors:?}"
    );
    assert!(errors[0].to_string().contains("not a directory"));
}

#[test]
fn a_missing_or_real_assets_folder_is_fine() {
    let root = scratch("discover_assets_ok");
    for name in ["without", "with"] {
        let folder = root.join(name);
        fs::create_dir_all(&folder).unwrap();
        fs::write(folder.join(MANIFEST_FILE), manifest_text(name)).unwrap();
    }
    fs::create_dir_all(root.join("with").join("assets")).unwrap();
    assert_eq!(discover(&root).unwrap().len(), 2);
}
