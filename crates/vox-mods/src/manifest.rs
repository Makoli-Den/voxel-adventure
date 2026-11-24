//! The manifest `mod.json`: what a mod says about itself, validated once when it is parsed.
//!
//! The manifest sits outside the mod's resource pack, so no pack can override it and it can be
//! read before any pack stack exists (decision 0014, section a). Parsing gives a
//! [`ModManifest`] whose fields cannot be wrong: every id, version, requirement and entry has
//! already been checked, so later code never re-validates them.

use std::collections::BTreeMap;
use std::fmt;

use semver::{Version, VersionReq};
use serde::Deserialize;
use vox_resources::{ResourceError, ResourceId, validate_namespace};

use crate::error::ManifestProblem;

/// The manifest layout this game reads. A different `format` is refused with its own message
/// instead of a confusing unknown-key error.
pub const MANIFEST_FORMAT: u32 = 1;

/// Largest manifest that is parsed, 64 KiB. A real manifest is well under 1 KiB; the cap only
/// keeps a hostile or broken file from being read into memory whole.
pub const MAX_MANIFEST_BYTES: usize = 64 * 1024;

/// The mod id reserved for the engine's own resources (the `core` namespace).
pub const RESERVED_MOD_ID: &str = "core";

/// The mod API this game provides, `MAJOR.MINOR`. A new function, event or field is a minor
/// bump; removing or changing anything is a major bump (decision 0014, section c).
pub const MOD_API_VERSION: ApiVersion = ApiVersion::new(1, 0);

/// The UTF-8 byte order mark. Some Windows editors put it at the start of a file, and JSON
/// parsers reject it, so it is skipped before parsing.
const UTF8_BOM: &[u8] = b"\xEF\xBB\xBF";

/// Separates major and minor in the text form of an [`ApiVersion`].
const API_SEPARATOR: char = '.';

/// The name of a mod: its namespace, validated once.
///
/// Same character rule as a [`ResourceId`] namespace (it is that rule), minus the name
/// [`RESERVED_MOD_ID`]. Ordered by text, which gives the deterministic tie-break of the load
/// order.
///
/// ```
/// use vox_mods::ModId;
///
/// assert!(ModId::parse("my_mod").is_ok());
/// assert!(ModId::parse("core").is_err());
/// assert!(ModId::parse("My-Mod").is_err());
/// ```
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ModId(String);

impl ModId {
    /// Parses and validates `text`.
    ///
    /// # Errors
    /// [`ManifestProblem::BadId`] or [`ManifestProblem::ReservedId`].
    pub fn parse(text: &str) -> Result<Self, ManifestProblem> {
        validate_namespace(text).map_err(|problem| ManifestProblem::BadId {
            text: text.to_owned(),
            problem,
        })?;
        if text == RESERVED_MOD_ID {
            return Err(ManifestProblem::ReservedId {
                text: text.to_owned(),
            });
        }
        Ok(Self(text.to_owned()))
    }

    /// The id as text, which is also the namespace and the folder name.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ModId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// A mod API version, `MAJOR.MINOR`. A mod that declares `1.2` means "`^1.2`": it loads on API
/// 1.2 and later 1.x, and not on 1.1 or on 2.x. Major 0 gets no special treatment (unlike
/// semver, where `0.x` minors are breaking): the rule is the same for every major.
///
/// ```
/// use vox_mods::ApiVersion;
///
/// let game = ApiVersion::new(1, 2);
/// assert!(ApiVersion::new(1, 0).is_supported_by(game));
/// assert!(!ApiVersion::new(1, 3).is_supported_by(game));
/// assert!(!ApiVersion::new(2, 0).is_supported_by(game));
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ApiVersion {
    major: u32,
    minor: u32,
}

impl ApiVersion {
    /// An API version.
    pub const fn new(major: u32, minor: u32) -> Self {
        Self { major, minor }
    }

    /// The major version.
    pub const fn major(self) -> u32 {
        self.major
    }

    /// The minor version.
    pub const fn minor(self) -> u32 {
        self.minor
    }

    /// Parses `MAJOR.MINOR`: two plain decimal numbers without leading zeros (so `1.0` has one
    /// spelling), nothing else.
    ///
    /// # Errors
    /// [`ManifestProblem::BadApi`].
    pub fn parse(text: &str) -> Result<Self, ManifestProblem> {
        let bad = || ManifestProblem::BadApi {
            text: text.to_owned(),
        };
        let (major, minor) = text.split_once(API_SEPARATOR).ok_or_else(bad)?;
        Ok(Self::new(
            parse_decimal(major).ok_or_else(bad)?,
            parse_decimal(minor).ok_or_else(bad)?,
        ))
    }

    /// Whether a mod written for `self` runs on a game that provides `game`: same major,
    /// and the mod does not use a minor version the game does not have yet.
    pub const fn is_supported_by(self, game: Self) -> bool {
        self.major == game.major && self.minor <= game.minor
    }
}

impl fmt::Display for ApiVersion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}{API_SEPARATOR}{}", self.major, self.minor)
    }
}

/// A number made only of ASCII digits, with no leading zero (`0` itself is fine).
/// `u32::from_str` would also accept a leading `+`.
fn parse_decimal(text: &str) -> Option<u32> {
    if text.is_empty() || !text.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    if text.len() > 1 && text.starts_with('0') {
        return None;
    }
    text.parse().ok()
}

impl Capability {
    /// The name used in `mod.json`.
    pub const fn name(self) -> &'static str {
        match self {
            Self::WorldRead => "world.read",
            Self::WorldWrite => "world.write",
        }
    }
}

/// A permission a mod asks for. Both v1 capabilities concern the game world only, so declaring
/// them is enough (decision 0014, section c).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Deserialize)]
pub enum Capability {
    /// Read blocks of the world.
    #[serde(rename = "world.read")]
    WorldRead,
    /// Change blocks of the world.
    #[serde(rename = "world.write")]
    WorldWrite,
}

/// The manifest as written, before validation. Strict: an unknown key is an error.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawManifest {
    /// Required, but already checked by `FormatHeader` before this struct is read.
    #[serde(rename = "format")]
    _format: u32,
    id: String,
    version: String,
    api: String,
    #[serde(default)]
    dependencies: DependencyPairs,
    #[serde(default)]
    entry: Option<String>,
    #[serde(default)]
    capabilities: Vec<Capability>,
}

/// The `dependencies` object as its key-value pairs in file order. A `BTreeMap` would keep only
/// the last of two equal keys without a word; reading pairs lets validation reject them.
#[derive(Default)]
struct DependencyPairs(Vec<(String, String)>);

impl<'de> Deserialize<'de> for DependencyPairs {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct PairsVisitor;

        impl<'de> serde::de::Visitor<'de> for PairsVisitor {
            type Value = DependencyPairs;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("an object of mod ids and version requirements")
            }

            fn visit_map<A: serde::de::MapAccess<'de>>(
                self,
                mut map: A,
            ) -> Result<Self::Value, A::Error> {
                let mut pairs = Vec::new();
                while let Some(pair) = map.next_entry::<String, String>()? {
                    pairs.push(pair);
                }
                Ok(DependencyPairs(pairs))
            }
        }

        deserializer.deserialize_map(PairsVisitor)
    }
}

/// Only the `format` number, read first and leniently so that a file of a future format reports
/// "unsupported format" and not an unknown-key error from the strict parse.
#[derive(Deserialize)]
struct FormatHeader {
    format: Option<u64>,
}

/// A validated `mod.json`.
///
/// ```
/// use vox_mods::ModManifest;
///
/// let json = br#"{ "format": 1, "id": "ruby", "version": "0.3.1", "api": "1.0",
///                  "dependencies": { "base": "^0.1" } }"#;
/// let manifest = ModManifest::parse(json).unwrap();
/// assert_eq!(manifest.id().as_str(), "ruby");
/// assert_eq!(manifest.dependencies().len(), 1);
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModManifest {
    id: ModId,
    version: Version,
    api: ApiVersion,
    dependencies: BTreeMap<ModId, VersionReq>,
    entry: Option<ResourceId>,
    capabilities: Vec<Capability>,
}

impl ModManifest {
    /// Parses and validates the bytes of a `mod.json`.
    ///
    /// # Errors
    /// The first [`ManifestProblem`] found.
    pub fn parse(json: &[u8]) -> Result<Self, ManifestProblem> {
        if json.len() > MAX_MANIFEST_BYTES {
            return Err(ManifestProblem::TooLarge {
                limit: MAX_MANIFEST_BYTES,
            });
        }
        let json = json.strip_prefix(UTF8_BOM).unwrap_or(json);
        // The text is parsed twice on purpose: the first, lenient pass only reads `format`, so a
        // file of another format gets "unsupported format" instead of an unknown-key error from
        // the strict pass. A manifest is under 1 KiB, so the second pass costs nothing worth
        // avoiding.
        let header: Result<FormatHeader, _> = serde_json::from_slice(json);
        if let Ok(FormatHeader {
            format: Some(found),
        }) = header
        {
            if found != u64::from(MANIFEST_FORMAT) {
                return Err(ManifestProblem::UnsupportedFormat { found });
            }
        }
        let raw: RawManifest =
            serde_json::from_slice(json).map_err(|e| ManifestProblem::Json(e.to_string()))?;
        Self::from_raw(raw)
    }

    fn from_raw(raw: RawManifest) -> Result<Self, ManifestProblem> {
        let id = ModId::parse(&raw.id)?;
        let version = Version::parse(&raw.version).map_err(|e| ManifestProblem::BadVersion {
            text: raw.version.clone(),
            message: e.to_string(),
        })?;
        let api = ApiVersion::parse(&raw.api)?;
        let mut dependencies = BTreeMap::new();
        for (name, requirement) in &raw.dependencies.0 {
            let dependency =
                ModId::parse(name).map_err(|problem| ManifestProblem::BadDependencyId {
                    text: name.clone(),
                    problem: Box::new(problem),
                })?;
            let requirement =
                VersionReq::parse(requirement).map_err(|e| ManifestProblem::BadRequirement {
                    dependency: name.clone(),
                    text: requirement.clone(),
                    message: e.to_string(),
                })?;
            if dependencies.insert(dependency, requirement).is_some() {
                return Err(ManifestProblem::DuplicateDependency { text: name.clone() });
            }
        }
        let mut capabilities: Vec<Capability> = Vec::with_capacity(raw.capabilities.len());
        for capability in raw.capabilities {
            if capabilities.contains(&capability) {
                return Err(ManifestProblem::DuplicateCapability {
                    name: capability.name(),
                });
            }
            capabilities.push(capability);
        }
        let entry = raw
            .entry
            .as_deref()
            .map(|text| parse_entry(text, &id))
            .transpose()?;
        Ok(Self {
            id,
            version,
            api,
            dependencies,
            entry,
            capabilities,
        })
    }

    /// Checks that the id equals the name of the folder the manifest was read from, so a mods
    /// folder cannot hold one id twice by accident.
    ///
    /// # Errors
    /// [`ManifestProblem::IdDiffersFromFolder`].
    pub fn check_folder(&self, folder: &str) -> Result<(), ManifestProblem> {
        if self.id.as_str() == folder {
            Ok(())
        } else {
            Err(ManifestProblem::IdDiffersFromFolder {
                id: self.id.clone(),
                folder: folder.to_owned(),
            })
        }
    }

    /// The mod's id and namespace.
    pub fn id(&self) -> &ModId {
        &self.id
    }

    /// The mod's own version.
    pub fn version(&self) -> &Version {
        &self.version
    }

    /// The mod API the mod was written against.
    pub fn api(&self) -> ApiVersion {
        self.api
    }

    /// The required mods and the versions they must have, ordered by id.
    pub fn dependencies(&self) -> &BTreeMap<ModId, VersionReq> {
        &self.dependencies
    }

    /// The entry script, if the mod has code. Always in the mod's own namespace.
    pub fn entry(&self) -> Option<&ResourceId> {
        self.entry.as_ref()
    }

    /// The permissions the mod asks for.
    pub fn capabilities(&self) -> &[Capability] {
        &self.capabilities
    }
}

/// Parses the entry and checks its namespace is the mod's own.
fn parse_entry(text: &str, id: &ModId) -> Result<ResourceId, ManifestProblem> {
    let entry = ResourceId::parse(text).map_err(|e| ManifestProblem::BadEntry {
        text: text.to_owned(),
        message: match e {
            ResourceError::InvalidId { problem, .. } => problem.to_string(),
            other => other.to_string(),
        },
    })?;
    if entry.namespace() != id.as_str() {
        return Err(ManifestProblem::EntryOutsideMod {
            text: text.to_owned(),
            id: id.clone(),
        });
    }
    Ok(entry)
}

#[cfg(test)]
mod tests {
    use super::*;
    use vox_resources::IdProblem;

    /// A manifest with the required keys and `rest` (more `, "key": value` members) appended.
    fn json(id: &str, version: &str, api: &str, rest: &str) -> String {
        format!(r#"{{ "format": 1, "id": "{id}", "version": "{version}", "api": "{api}"{rest} }}"#)
    }

    fn parse(text: &str) -> Result<ModManifest, ManifestProblem> {
        ModManifest::parse(text.as_bytes())
    }

    fn problem(text: &str) -> ManifestProblem {
        parse(text).expect_err("manifest should be invalid")
    }

    #[test]
    fn a_full_manifest_parses() {
        let manifest = parse(
            r#"{ "format": 1, "id": "example", "version": "0.3.1", "api": "1.0",
                 "dependencies": { "base": "^0.1", "gems": "=1.2.3" },
                 "entry": "example:scripts/main.lua",
                 "capabilities": ["world.read", "world.write"] }"#,
        )
        .unwrap();
        assert_eq!(manifest.id().as_str(), "example");
        assert_eq!(manifest.version(), &Version::new(0, 3, 1));
        assert_eq!(manifest.api(), ApiVersion::new(1, 0));
        let names: Vec<&str> = manifest.dependencies().keys().map(ModId::as_str).collect();
        assert_eq!(names, ["base", "gems"]);
        assert_eq!(
            manifest.entry().map(ToString::to_string).as_deref(),
            Some("example:scripts/main.lua")
        );
        assert_eq!(
            manifest.capabilities(),
            [Capability::WorldRead, Capability::WorldWrite]
        );
    }

    #[test]
    fn optional_parts_default_to_empty() {
        let manifest = parse(&json("data_only", "1.0.0", "1.0", "")).unwrap();
        assert!(manifest.dependencies().is_empty());
        assert!(manifest.entry().is_none());
        assert!(manifest.capabilities().is_empty());
    }

    #[test]
    fn unknown_key_is_rejected() {
        let text = json("a", "1.0.0", "1.0", r#", "colour": "red""#);
        match problem(&text) {
            ManifestProblem::Json(message) => assert!(message.contains("colour"), "{message}"),
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn missing_key_and_bad_json_are_json_problems() {
        assert!(matches!(
            problem(r#"{ "format": 1, "id": "a" }"#),
            ManifestProblem::Json(_)
        ));
        assert!(matches!(problem("{ not json"), ManifestProblem::Json(_)));
        assert!(matches!(problem(""), ManifestProblem::Json(_)));
        assert!(matches!(
            ModManifest::parse(&[0xff, 0xfe]),
            Err(ManifestProblem::Json(_))
        ));
    }

    #[test]
    fn other_format_is_reported_as_such_even_with_other_keys() {
        let text = r#"{ "format": 2, "id": "a", "future_key": true }"#;
        assert_eq!(
            problem(text),
            ManifestProblem::UnsupportedFormat { found: 2 }
        );
        assert_eq!(
            problem(r#"{ "format": 1000000000000, "id": "a" }"#),
            ManifestProblem::UnsupportedFormat {
                found: 1_000_000_000_000
            }
        );
    }

    #[test]
    fn bad_ids_are_rejected() {
        for (id, expected) in [
            ("", IdProblem::EmptyNamespace),
            ("My", IdProblem::BadNamespaceChar('M')),
            ("my-mod", IdProblem::BadNamespaceChar('-')),
            ("a:b", IdProblem::BadNamespaceChar(':')),
            ("nul", IdProblem::ReservedName),
        ] {
            match problem(&json(id, "1.0.0", "1.0", "")) {
                ManifestProblem::BadId { problem, .. } => assert_eq!(problem, expected, "{id}"),
                other => panic!("`{id}`: unexpected {other:?}"),
            }
        }
    }

    #[test]
    fn the_core_id_is_reserved() {
        assert_eq!(
            problem(&json("core", "1.0.0", "1.0", "")),
            ManifestProblem::ReservedId {
                text: "core".to_owned()
            }
        );
        // Longer names that merely start with it are ordinary.
        assert!(ModId::parse("core2").is_ok());
        assert!(ModId::parse("corex").is_ok());
    }

    #[test]
    fn id_must_equal_the_folder_name() {
        let manifest = parse(&json("alpha", "1.0.0", "1.0", "")).unwrap();
        assert_eq!(manifest.check_folder("alpha"), Ok(()));
        assert_eq!(
            manifest.check_folder("beta"),
            Err(ManifestProblem::IdDiffersFromFolder {
                id: ModId::parse("alpha").unwrap(),
                folder: "beta".to_owned()
            })
        );
        // Case matters even on a case-insensitive file system.
        assert!(manifest.check_folder("Alpha").is_err());
    }

    #[test]
    fn bad_versions_are_rejected() {
        for version in ["", "1", "1.0", "v1.0.0", "1.0.0.0", "latest"] {
            assert!(
                matches!(
                    problem(&json("a", version, "1.0", "")),
                    ManifestProblem::BadVersion { .. }
                ),
                "`{version}`"
            );
        }
        assert!(parse(&json("a", "1.0.0-beta.1+build5", "1.0", "")).is_ok());
    }

    #[test]
    fn bad_api_is_rejected() {
        for api in [
            "", "1", "1.", ".1", "1.0.0", "a.b", "+1.0", "1.-1", " 1.0", "1.0 ",
        ] {
            assert!(
                matches!(
                    problem(&json("a", "1.0.0", api, "")),
                    ManifestProblem::BadApi { .. }
                ),
                "`{api}`"
            );
        }
        // Too big for u32.
        assert!(ApiVersion::parse("99999999999.0").is_err());
    }

    #[test]
    fn api_version_parses_displays_and_orders_support() {
        let api = ApiVersion::parse("12.34").unwrap();
        assert_eq!((api.major(), api.minor()), (12, 34));
        assert_eq!(api.to_string(), "12.34");
        assert_eq!(MOD_API_VERSION.to_string(), "1.0");
    }

    #[test]
    fn api_acceptance_rule() {
        let game = ApiVersion::new(1, 2);
        assert!(ApiVersion::new(1, 2).is_supported_by(game), "same");
        assert!(ApiVersion::new(1, 0).is_supported_by(game), "older minor");
        assert!(!ApiVersion::new(1, 3).is_supported_by(game), "newer minor");
        assert!(!ApiVersion::new(0, 2).is_supported_by(game), "older major");
        assert!(!ApiVersion::new(2, 0).is_supported_by(game), "newer major");
        assert!(
            !ApiVersion::new(2, 2).is_supported_by(game),
            "newer major, same minor"
        );
    }

    #[test]
    fn bad_dependency_id_and_requirement_are_rejected() {
        let text = json("a", "1.0.0", "1.0", r#", "dependencies": { "Bad": "^1" }"#);
        assert!(matches!(
            problem(&text),
            ManifestProblem::BadDependencyId { .. }
        ));
        let text = json("a", "1.0.0", "1.0", r#", "dependencies": { "core": "^1" }"#);
        assert!(matches!(
            problem(&text),
            ManifestProblem::BadDependencyId { .. }
        ));
        for requirement in ["", "banana", "^", ">=1.x.y"] {
            let text = json(
                "a",
                "1.0.0",
                "1.0",
                &format!(r#", "dependencies": {{ "base": "{requirement}" }}"#),
            );
            assert!(
                matches!(problem(&text), ManifestProblem::BadRequirement { .. }),
                "`{requirement}`"
            );
        }
    }

    #[test]
    fn entry_must_be_a_valid_id_in_the_mods_own_namespace() {
        let with_entry =
            |entry: &str| json("a", "1.0.0", "1.0", &format!(r#", "entry": "{entry}""#));
        assert!(parse(&with_entry("a:scripts/main.lua")).is_ok());
        assert!(matches!(
            problem(&with_entry("scripts/main.lua")),
            ManifestProblem::BadEntry { .. }
        ));
        assert!(matches!(
            problem(&with_entry("a:../main.lua")),
            ManifestProblem::BadEntry { .. }
        ));
        assert_eq!(
            problem(&with_entry("base:scripts/main.lua")),
            ManifestProblem::EntryOutsideMod {
                text: "base:scripts/main.lua".to_owned(),
                id: ModId::parse("a").unwrap()
            }
        );
    }

    #[test]
    fn duplicate_dependency_keys_are_rejected() {
        let text = json(
            "a",
            "1.0.0",
            "1.0",
            r#", "dependencies": { "base": "^0.1", "base": "^0.2" }"#,
        );
        assert_eq!(
            problem(&text),
            ManifestProblem::DuplicateDependency {
                text: "base".to_owned()
            }
        );
    }

    #[test]
    fn duplicate_capabilities_are_rejected() {
        let text = json(
            "a",
            "1.0.0",
            "1.0",
            r#", "capabilities": ["world.read", "world.write", "world.read"]"#,
        );
        assert_eq!(
            problem(&text),
            ManifestProblem::DuplicateCapability { name: "world.read" }
        );
    }

    #[test]
    fn a_leading_utf8_bom_is_skipped() {
        let mut bytes = UTF8_BOM.to_vec();
        bytes.extend_from_slice(json("a", "1.0.0", "1.0", "").as_bytes());
        assert!(ModManifest::parse(&bytes).is_ok());
        // Only at the start, and only once.
        let mut twice = UTF8_BOM.to_vec();
        twice.extend_from_slice(&bytes);
        assert!(ModManifest::parse(&twice).is_err());
    }

    #[test]
    fn api_with_leading_zeros_is_rejected() {
        for api in ["01.0", "1.00", "1.01", "00.0"] {
            assert!(ApiVersion::parse(api).is_err(), "`{api}`");
        }
        assert_eq!(ApiVersion::parse("0.0"), Ok(ApiVersion::new(0, 0)));
        assert_eq!(ApiVersion::parse("10.10"), Ok(ApiVersion::new(10, 10)));
    }

    #[test]
    fn major_zero_follows_the_same_rule_as_any_major() {
        let game = ApiVersion::new(0, 5);
        assert!(ApiVersion::new(0, 5).is_supported_by(game));
        assert!(ApiVersion::new(0, 4).is_supported_by(game));
        assert!(!ApiVersion::new(0, 6).is_supported_by(game));
    }

    #[test]
    fn bad_api_message_shows_the_current_version() {
        let message = problem(&json("a", "1.0.0", "x", "")).to_string();
        assert!(message.contains(&MOD_API_VERSION.to_string()), "{message}");
    }

    #[test]
    fn unknown_capability_is_rejected() {
        let text = json("a", "1.0.0", "1.0", r#", "capabilities": ["fs.read"]"#);
        match problem(&text) {
            ManifestProblem::Json(message) => assert!(message.contains("fs.read"), "{message}"),
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn oversized_manifest_is_refused_before_parsing() {
        let mut text = json("a", "1.0.0", "1.0", "");
        text.push_str(&" ".repeat(MAX_MANIFEST_BYTES));
        assert_eq!(
            problem(&text),
            ManifestProblem::TooLarge {
                limit: MAX_MANIFEST_BYTES
            }
        );
        // Exactly at the limit is still read.
        let mut text = json("a", "1.0.0", "1.0", "");
        text.push_str(&" ".repeat(MAX_MANIFEST_BYTES - text.len()));
        assert_eq!(text.len(), MAX_MANIFEST_BYTES);
        assert!(parse(&text).is_ok());
    }

    #[test]
    fn messages_name_the_problem() {
        let message = problem(&json("My", "1.0.0", "1.0", "")).to_string();
        assert!(
            message.contains("`My`") && message.contains("'M'"),
            "{message}"
        );
    }
}
