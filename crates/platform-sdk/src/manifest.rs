//! The game manifest: one JSON file per game that the launcher's registry
//! reads. Everything game-specific the launcher needs (names, the disc the
//! player must own, how to import it, how to start the game, where its
//! online server is, whether it takes mods) lives here, so the launcher has
//! no game hard-coded.
//!
//! Commands are program + argument lists (never a shell line) with
//! `{placeholders}` expanded by [`expand_args`].

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// A game the launcher can import and start.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GameManifest {
    /// The launcher-game contract this manifest was written for
    /// ([`crate::CONTRACT_VERSION`]).
    pub contract: u32,
    /// Stable id: 2..=32 of `a-z`, `0-9`, `-` (folder names, URLs, tokens).
    pub id: String,
    /// Full name (tooltip, settings).
    pub name: String,
    /// The game build's version (shown in settings, sent to its server).
    #[serde(default)]
    pub version: String,
    /// Big title text shown before the player's own import provides art.
    #[serde(default)]
    pub hero: Option<String>,
    /// The small line under the title.
    #[serde(default)]
    pub subtitle: Option<String>,
    /// A beta/status notice shown as a dismissable banner.
    #[serde(default)]
    pub notice: Option<String>,
    /// Discs the player must own; any one of them is enough.
    pub discs: Vec<DiscRequirement>,
    /// What kind of disc image file the player picks (default: an `.iso`).
    #[serde(default, skip_serializing_if = "DiscInput::is_default")]
    pub disc: DiscInput,
    pub importer: ImporterSpec,
    pub executable: LaunchSpec,
    /// An optional on/off switch the launcher shows next to Play (Roses:
    /// "Online Play and extras"; off starts the game as the original).
    #[serde(default)]
    pub launch_toggle: Option<LaunchToggle>,
    /// Where this game's imports, saves and logs go, relative to the
    /// launcher's data folder (default: the game's id). `"."` is the data
    /// folder itself (Roses, for installs from before the registry).
    #[serde(default)]
    pub data_dir: Option<String>,
    #[serde(default)]
    pub online: Option<OnlineSpec>,
    #[serde(default)]
    pub mods: Option<ModSupport>,
    /// Player-facing texts for the settings page.
    #[serde(default)]
    pub texts: GameTexts,
    #[serde(default)]
    pub updates: Option<UpdateChannel>,
    /// Web pages for this game (none yet: placeholders until its
    /// repository exists). Opened in the system browser.
    #[serde(default)]
    pub links: GameLinks,
    /// How the launcher pictures the game (all optional).
    #[serde(default)]
    pub art: GameArt,
}

/// The game's pictures in the launcher. Nothing of a game ships with the
/// launcher: the backdrop comes from the player's own import.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct GameArt {
    /// Formerly one of the launcher's own drawn marks for the sidebar;
    /// no game is pictured by a drawing of the launcher's any more, so it is
    /// read (old manifests keep loading) and ignored.
    #[serde(skip_serializing)]
    pub mark: Option<String>,
    /// The sidebar tile's text before an import provides the game's logo
    /// (1 to 4 letters or digits, e.g. "DotR"); default: the name's
    /// initials.
    pub initials: Option<String>,
    /// A PNG inside the newest import (relative to its folder), shown as
    /// the page's backdrop once imported.
    pub backdrop: Option<String>,
    /// The part of the backdrop (x, y, width, height in its pixels) cut out
    /// as the sidebar logo; black is keyed out.
    pub logo: Option<[u32; 4]>,
}

/// A game's web pages; each https or absent.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct GameLinks {
    /// Where to report a problem (an issue tracker).
    pub issues: Option<String>,
    /// The known-issues page.
    pub known_issues: Option<String>,
    /// The game's releases (newest build).
    pub releases: Option<String>,
    pub website: Option<String>,
}

impl GameLinks {
    pub fn all(&self) -> [(&'static str, &Option<String>); 4] {
        [("issues", &self.issues), ("known_issues", &self.known_issues), ("releases", &self.releases), ("website", &self.website)]
    }
}

/// A link the launcher may open: `https://` with a host, no spaces or
/// controls (nothing else is handed to the system browser).
pub fn valid_link(url: &str) -> bool {
    url.len() <= 2048
        && url.strip_prefix("https://").is_some_and(|rest| !rest.is_empty() && !rest.starts_with('/'))
        && !url.chars().any(|c| c.is_whitespace() || c.is_control() || c == '"')
}

/// One supported disc. The importer checks the fingerprint; the launcher
/// only shows what is needed and may pre-check size/serial.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DiscRequirement {
    /// "PAL (Europe)".
    pub label: String,
    /// "SLES-51579".
    pub serial: String,
    /// Image size in bytes, when one exact dump is required.
    #[serde(default)]
    pub size: Option<u64>,
    /// SHA-256 of the whole image (lower-case hex), when one exact dump is
    /// required.
    #[serde(default)]
    pub sha256: Option<String>,
}

/// The disc image files the launcher's file picker offers and the importer
/// accepts (`{iso}` is the picked file whatever its kind).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct DiscInput {
    /// File extensions without the dot, lower case, 1 to 8 of them, e.g.
    /// `["cue", "bin"]` for a PS1 game (a CUE sheet with its BIN, or the BIN
    /// alone). Absent: `["iso"]`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub extensions: Option<Vec<String>>,
}

impl DiscInput {
    /// The extensions when none are given.
    pub const DEFAULT_EXTENSIONS: [&'static str; 1] = ["iso"];

    fn is_default(&self) -> bool {
        self.extensions.is_none()
    }

    /// The accepted extensions (lower case, without the dot).
    pub fn extensions(&self) -> Vec<&str> {
        match &self.extensions {
            Some(list) => list.iter().map(String::as_str).collect(),
            None => Self::DEFAULT_EXTENSIONS.to_vec(),
        }
    }

    /// `path` has one of the accepted extensions (any case).
    pub fn accepts(&self, path: &std::path::Path) -> bool {
        path.extension().and_then(|e| e.to_str()).is_some_and(|e| self.extensions().iter().any(|x| x.eq_ignore_ascii_case(e)))
    }

    /// "ISO" for the default, else "disc image" (button and dialog texts).
    pub fn noun(&self) -> &'static str {
        if self.extensions() == Self::DEFAULT_EXTENSIONS {
            "ISO"
        } else {
            "disc image"
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ImporterSpec {
    /// The importer program, relative to the game's install folder.
    pub program: String,
    /// Arguments; placeholders: `{iso}`, `{import_dir}`, `{data_dir}`.
    pub args: Vec<String>,
    /// Each import goes to a new `<prefix><timestamp>` folder in the data
    /// folder ("game-").
    pub import_prefix: String,
    /// The file an import writes last; the import counts as complete only
    /// once it exists ("game.json").
    pub complete_marker: String,
    /// Progress lines on stdout/stderr: "steps" means `[n/total] text`.
    #[serde(default)]
    pub progress: Option<String>,
    /// Superseded by `verify`/`check` and ignored (still read so manifests
    /// that set it keep loading).
    #[serde(default, skip_serializing)]
    pub legacy_roses_library_check: bool,
    /// Checks one finished import before the launcher calls it complete;
    /// placeholders `{import_dir}`, `{marker}`, `{data_dir}`. It prints an
    /// [`crate::contract::ImportReport`] (see [`crate::contract`]).
    #[serde(default)]
    pub verify: Option<CommandSpec>,
    /// Finds the newest usable import when the launcher starts (an import
    /// that no longer checks out is skipped for an older one); placeholder
    /// `{data_dir}`. Prints an [`crate::contract::ImportReport`]. Without it
    /// the newest complete marker counts.
    #[serde(default)]
    pub check: Option<CommandSpec>,
    /// Launcher development options passed on to the importer when given
    /// (e.g. `"--research-root"`); none for players.
    #[serde(default)]
    pub dev_options: Vec<String>,
}

/// A helper command of a game's importer.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommandSpec {
    /// Relative to the game's install folder (default: the importer).
    #[serde(default)]
    pub program: Option<String>,
    pub args: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LaunchSpec {
    /// The game program, relative to the game's install folder.
    pub program: String,
    /// Arguments; placeholders: `{marker}` (the newest import's complete
    /// marker file), `{import_dir}`, `{data_dir}`.
    pub args: Vec<String>,
    /// Launcher development options passed on to the game when given
    /// (e.g. `"--research-root"`); none for players.
    #[serde(default)]
    pub dev_options: Vec<String>,
}

/// A switch shown beside Play, remembered per game by the launcher. Its
/// state only picks extra arguments for the game: `args_on` when on,
/// `args_off` when off (each `--option` or a placeholder of
/// [`LaunchSpec::args`]).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LaunchToggle {
    /// Stable id (as a game id: `a-z`, `0-9`, `-`); the launcher remembers
    /// the state under it.
    pub id: String,
    /// The switch's caption, 1 to 40 characters.
    pub label: String,
    /// The state before the player changes it.
    #[serde(default)]
    pub default: bool,
    #[serde(default)]
    pub args_on: Vec<String>,
    #[serde(default)]
    pub args_off: Vec<String>,
}

impl LaunchToggle {
    /// The extra game arguments for this state.
    pub fn args(&self, on: bool) -> &[String] {
        if on {
            &self.args_on
        } else {
            &self.args_off
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OnlineSpec {
    /// The server file shipped with the game ("online.json").
    pub config_file: String,
    /// The audience a platform token for this game's server names.
    pub audience: String,
    /// What the game's server needs in an account-only hello besides the
    /// platform's own fields (game-defined JSON, e.g. its protocol version),
    /// so the launcher's Profile page signs in without knowing the game.
    #[serde(default)]
    pub account_hello: Option<serde_json::Map<String, serde_json::Value>>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModSupport {
    /// The mod API version the game loads ([`crate::mods::ModManifest::api`]).
    pub api: u32,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct GameTexts {
    /// What to say before anything is imported.
    pub import_needed: Option<String>,
    pub saves: Option<String>,
    pub controls: Option<String>,
    pub running: Option<String>,
    pub ready: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UpdateChannel {
    /// An https URL serving a signed release index (not used yet).
    pub url: String,
    #[serde(default)]
    pub channel: Option<String>,
}

/// Why a manifest is not usable.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ManifestError(pub String);

impl std::fmt::Display for ManifestError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

pub fn valid_id(id: &str) -> bool {
    (2..=32).contains(&id.len())
        && id.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        && id.starts_with(|c: char| c.is_ascii_lowercase())
}

/// A relative path that stays inside its base folder (no `..`, no root, no
/// drive).
pub fn safe_relative(path: &str) -> bool {
    !path.is_empty()
        && !path.contains(':')
        && !path.starts_with(['/', '\\'])
        && path.split(['/', '\\']).all(|part| part != ".." && !part.is_empty())
}

impl GameManifest {
    pub fn parse(json: &str) -> Result<Self, ManifestError> {
        let manifest: Self = serde_json::from_str(json).map_err(|e| ManifestError(format!("not a game manifest: {e}")))?;
        manifest.validate()?;
        Ok(manifest)
    }

    pub fn validate(&self) -> Result<(), ManifestError> {
        let bad = |why: String| Err(ManifestError(why));
        if self.contract != crate::CONTRACT_VERSION {
            return bad(format!("{}: written for launcher contract {}, this launcher speaks {}", self.id, self.contract, crate::CONTRACT_VERSION));
        }
        if !valid_id(&self.id) {
            return bad(format!("bad game id {:?}", self.id));
        }
        if self.name.trim().is_empty() || self.name.len() > 80 {
            return bad(format!("{}: the name must be 1 to 80 characters", self.id));
        }
        if self.discs.is_empty() {
            return bad(format!("{}: at least one supported disc is needed", self.id));
        }
        for d in &self.discs {
            if let Some(h) = &d.sha256 {
                if h.len() != 64 || !h.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)) {
                    return bad(format!("{}: disc {} has a malformed sha256", self.id, d.serial));
                }
            }
        }
        if let Some(list) = &self.disc.extensions {
            if !(1..=8).contains(&list.len()) {
                return bad(format!("{}: disc.extensions must name 1 to 8 extensions", self.id));
            }
            for (i, ext) in list.iter().enumerate() {
                if !(1..=8).contains(&ext.len()) || !ext.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit()) {
                    return bad(format!("{}: disc.extensions: {ext:?} must be 1 to 8 lower-case letters or digits, without the dot", self.id));
                }
                if list[..i].contains(ext) {
                    return bad(format!("{}: disc.extensions names {ext:?} twice", self.id));
                }
            }
        }
        let helpers = [&self.importer.verify, &self.importer.check];
        for program in [&self.importer.program, &self.executable.program]
            .into_iter()
            .chain(helpers.iter().filter_map(|c| c.as_ref()?.program.as_ref()))
        {
            if !safe_relative(program) {
                return bad(format!("{}: program {program:?} must be a relative path inside the game folder", self.id));
            }
        }
        if !safe_relative(&self.importer.complete_marker) || self.importer.import_prefix.is_empty() || !safe_relative(&self.importer.import_prefix) {
            return bad(format!("{}: bad import folder prefix or complete marker", self.id));
        }
        if let Some(dir) = &self.data_dir {
            if dir != "." && !safe_relative(dir) {
                return bad(format!("{}: data_dir must stay inside the launcher's data folder", self.id));
            }
        }
        for option in self.importer.dev_options.iter().chain(&self.executable.dev_options) {
            if !option.starts_with("--") {
                return bad(format!("{}: dev option {option:?} must start with --", self.id));
            }
        }
        if let Some(initials) = &self.art.initials {
            let n = initials.chars().count();
            if !(1..=4).contains(&n) || !initials.chars().all(char::is_alphanumeric) {
                return bad(format!("{}: art.initials must be 1 to 4 letters or digits", self.id));
            }
        }
        if let Some(path) = &self.art.backdrop {
            if !safe_relative(path) {
                return bad(format!("{}: art.backdrop must be a relative path inside the import", self.id));
            }
        }
        for (name, link) in self.links.all() {
            if link.as_deref().is_some_and(|l| !valid_link(l)) {
                return bad(format!("{}: links.{name} must be an https:// address", self.id));
            }
        }
        if let Some(toggle) = &self.launch_toggle {
            if !valid_id(&toggle.id) {
                return bad(format!("{}: launch_toggle has a bad id {:?}", self.id, toggle.id));
            }
            let label = toggle.label.trim();
            if label.is_empty() || toggle.label.chars().count() > 40 || toggle.label.chars().any(char::is_control) {
                return bad(format!("{}: launch_toggle.label must be 1 to 40 characters", self.id));
            }
            for arg in toggle.args_on.iter().chain(&toggle.args_off) {
                if !arg.starts_with("--") && !(arg.starts_with('{') && arg.ends_with('}')) {
                    return bad(format!("{}: launch_toggle argument {arg:?} must start with -- or be a placeholder", self.id));
                }
            }
            check_placeholders(&toggle.args_on, &["marker", "import_dir", "data_dir"])
                .and_then(|_| check_placeholders(&toggle.args_off, &["marker", "import_dir", "data_dir"]))
                .map_err(|p| ManifestError(format!("{}: launch_toggle: unknown placeholder {{{p}}}", self.id)))?;
        }
        check_placeholders(&self.importer.args, &["iso", "import_dir", "data_dir"]).map_err(|p| ManifestError(format!("{}: importer: unknown placeholder {{{p}}}", self.id)))?;
        check_placeholders(&self.executable.args, &["marker", "import_dir", "data_dir"]).map_err(|p| ManifestError(format!("{}: executable: unknown placeholder {{{p}}}", self.id)))?;
        if let Some(verify) = &self.importer.verify {
            check_placeholders(&verify.args, &["import_dir", "marker", "data_dir"]).map_err(|p| ManifestError(format!("{}: importer.verify: unknown placeholder {{{p}}}", self.id)))?;
        }
        if let Some(check) = &self.importer.check {
            check_placeholders(&check.args, &["data_dir"]).map_err(|p| ManifestError(format!("{}: importer.check: unknown placeholder {{{p}}}", self.id)))?;
        }
        Ok(())
    }

    /// The data folder name relative to the launcher's data root.
    pub fn data_dir(&self) -> &str {
        self.data_dir.as_deref().unwrap_or(&self.id)
    }
}

fn placeholders(arg: &str) -> impl Iterator<Item = &str> {
    arg.split('{').skip(1).filter_map(|rest| rest.split_once('}').map(|(name, _)| name))
}

fn check_placeholders(args: &[String], allowed: &[&str]) -> Result<(), String> {
    for arg in args {
        for name in placeholders(arg) {
            if !allowed.contains(&name) {
                return Err(name.to_string());
            }
        }
    }
    Ok(())
}

/// Expand `{name}` placeholders in each argument (values are whole OS
/// strings: an argument that is exactly one placeholder becomes the value
/// unchanged, so paths that are not UTF-8 survive). Unknown names are an
/// error.
pub fn expand_args(args: &[String], values: &BTreeMap<&str, std::ffi::OsString>) -> Result<Vec<std::ffi::OsString>, ManifestError> {
    args.iter()
        .map(|arg| {
            if let Some(name) = arg.strip_prefix('{').and_then(|a| a.strip_suffix('}')).filter(|n| !n.contains(['{', '}'])) {
                return values.get(name).cloned().ok_or_else(|| ManifestError(format!("unknown placeholder {{{name}}}")));
            }
            let mut out = String::new();
            let mut rest = arg.as_str();
            while let Some(start) = rest.find('{') {
                out.push_str(&rest[..start]);
                let (name, after) = rest[start + 1..].split_once('}').ok_or_else(|| ManifestError(format!("unclosed placeholder in {arg:?}")))?;
                let value = values.get(name).ok_or_else(|| ManifestError(format!("unknown placeholder {{{name}}}")))?;
                out.push_str(&value.to_string_lossy());
                rest = after;
            }
            out.push_str(rest);
            Ok(out.into())
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> String {
        r#"{
            "contract": 1, "id": "example", "name": "Example Game",
            "discs": [{"label": "PAL", "serial": "SLES-00000"}],
            "importer": {"program": "example-cli.exe", "args": ["import", "--iso", "{iso}", "--out", "{import_dir}"],
                         "import_prefix": "game-", "complete_marker": "game.json", "progress": "steps"},
            "executable": {"program": "example.exe", "args": ["--game", "{marker}"]},
            "online": {"config_file": "online.json", "audience": "example"},
            "mods": {"api": 1}
        }"#
        .into()
    }

    #[test]
    fn a_manifest_parses_and_validates() {
        let m = GameManifest::parse(&sample()).unwrap();
        assert_eq!(m.id, "example");
        assert_eq!(m.data_dir(), "example");
        assert_eq!(m.mods, Some(ModSupport { api: 1 }));
    }

    #[test]
    fn unsafe_or_unknown_things_are_refused() {
        let bad = |from: &str, to: &str| GameManifest::parse(&sample().replacen(from, to, 1)).unwrap_err().0;
        assert!(bad("\"example\"", "\"Ex ample\"").contains("bad game id"));
        assert!(bad("example-cli.exe", "../evil.exe").contains("relative path"));
        assert!(bad("example.exe", "C:/Windows/evil.exe").contains("relative path"));
        assert!(bad("{marker}", "{password}").contains("unknown placeholder"));
        assert!(bad("\"contract\": 1", "\"contract\": 9").contains("contract"));
        assert!(bad("\"mods\"", "\"modz\"").contains("not a game manifest"));
        assert!(bad("\"mods\": {\"api\": 1}", "\"mods\": {\"api\": 1}, \"links\": {\"issues\": \"http://x.example\"}").contains("https"));
        assert!(valid_link("https://github.com/owner/repo/issues") && !valid_link("javascript:alert(1)") && !valid_link("https://"));
        assert!(!valid_link("https://a b") && !valid_link("file:///C:/x"));
        assert!(bad("\"progress\": \"steps\"", "\"progress\": \"steps\", \"check\": {\"args\": [\"{iso}\"]}").contains("importer.check"));
        assert!(bad("\"progress\": \"steps\"", "\"progress\": \"steps\", \"verify\": {\"program\": \"../x.exe\", \"args\": []}").contains("relative path"));
        assert!(bad("\"progress\": \"steps\"", "\"progress\": \"steps\", \"dev_options\": [\"x\"]").contains("dev option"));
        // Manifests from before verify/check still load.
        let old = GameManifest::parse(&sample().replacen("\"progress\": \"steps\"", "\"progress\": \"steps\", \"legacy_roses_library_check\": true", 1)).unwrap();
        assert!(old.importer.verify.is_none() && old.importer.check.is_none());
    }

    #[test]
    fn a_launch_toggle_is_optional_and_validated() {
        assert_eq!(GameManifest::parse(&sample()).unwrap().launch_toggle, None);
        let with = |toggle: &str| sample().replacen("\"mods\": {\"api\": 1}", &format!("\"mods\": {{\"api\": 1}}, \"launch_toggle\": {toggle}"), 1);
        let m = GameManifest::parse(&with(r#"{"id": "extras", "label": "Online Play and extras", "default": true, "args_off": ["--faithful"]}"#)).unwrap();
        let t = m.launch_toggle.unwrap();
        assert!(t.default);
        assert_eq!(t.args(false), ["--faithful"]);
        assert!(t.args(true).is_empty());
        let bad = |toggle: &str| GameManifest::parse(&with(toggle)).unwrap_err().0;
        assert!(bad(r#"{"id": "Extras", "label": "x"}"#).contains("bad id"));
        assert!(bad(r#"{"id": "extras", "label": " "}"#).contains("label"));
        assert!(bad(&format!(r#"{{"id": "extras", "label": "{}"}}"#, "x".repeat(41))).contains("label"));
        assert!(bad(r#"{"id": "extras", "label": "x", "args_off": ["faithful"]}"#).contains("must start with --"));
        assert!(bad(r#"{"id": "extras", "label": "x", "args_on": ["{iso}"]}"#).contains("unknown placeholder"));
        assert!(bad(r#"{"id": "extras", "label": "x", "colour": 1}"#).contains("not a game manifest"));
        let art = |a: &str| sample().replacen("\"mods\": {\"api\": 1}", &format!("\"mods\": {{\"api\": 1}}, \"art\": {a}"), 1);
        assert_eq!(GameManifest::parse(&art(r#"{"initials": "DotR"}"#)).unwrap().art.initials.as_deref(), Some("DotR"));
        assert!(GameManifest::parse(&art(r#"{"initials": "D o"}"#)).unwrap_err().0.contains("initials"));
        assert!(GameManifest::parse(&art(r#"{"mark": "rose"}"#)).is_ok(), "an old mark still loads");
    }

    #[test]
    fn disc_extensions_default_to_iso_and_are_validated() {
        let m = GameManifest::parse(&sample()).unwrap();
        assert_eq!(m.disc.extensions(), ["iso"]);
        assert_eq!(m.disc.noun(), "ISO");
        assert!(m.disc.accepts(std::path::Path::new("C:/a/Game.ISO")) && !m.disc.accepts(std::path::Path::new("a.cue")));
        assert!(!serde_json::to_string(&m).unwrap().contains("\"disc\":"), "the default is not written");
        let with = |disc: &str| sample().replacen("\"mods\": {\"api\": 1}", &format!("\"mods\": {{\"api\": 1}}, \"disc\": {disc}"), 1);
        let ps1 = GameManifest::parse(&with(r#"{"extensions": ["cue", "bin"]}"#)).unwrap();
        assert_eq!(ps1.disc.extensions(), ["cue", "bin"]);
        assert_eq!(ps1.disc.noun(), "disc image");
        assert!(ps1.disc.accepts(std::path::Path::new("D:/FM (Europe).Bin")) && !ps1.disc.accepts(std::path::Path::new("x.iso")));
        assert!(!ps1.disc.accepts(std::path::Path::new("no-extension")));
        assert_eq!(GameManifest::parse(&with("{}")).unwrap().disc.extensions(), ["iso"]);
        let bad = |disc: &str| GameManifest::parse(&with(disc)).unwrap_err().0;
        assert!(bad(r#"{"extensions": []}"#).contains("1 to 8"));
        assert!(bad(r#"{"extensions": [".cue"]}"#).contains("without the dot"));
        assert!(bad(r#"{"extensions": ["CUE"]}"#).contains("lower-case"));
        assert!(bad(r#"{"extensions": ["toolongext"]}"#).contains("1 to 8"));
        assert!(bad(r#"{"extensions": ["cue", "cue"]}"#).contains("twice"));
        assert!(bad(r#"{"extensions": ["a","b","c","d","e","f","g","h","i"]}"#).contains("1 to 8"));
        assert!(bad(r#"{"extension": ["cue"]}"#).contains("not a game manifest"));
    }

    #[test]
    fn placeholders_expand_without_a_shell() {
        let mut values = BTreeMap::new();
        values.insert("iso", std::ffi::OsString::from("C:\\My Games\\a b.iso"));
        values.insert("import_dir", std::ffi::OsString::from("D:\\data\\game-1"));
        let args = vec!["--iso".to_string(), "{iso}".into(), "--out={import_dir}".into()];
        let out = expand_args(&args, &values).unwrap();
        assert_eq!(out, vec!["--iso".into(), std::ffi::OsString::from("C:\\My Games\\a b.iso"), "--out=D:\\data\\game-1".into()]);
        assert!(expand_args(&["{nope}".into()], &values).is_err());
    }
}
