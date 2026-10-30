//! The launcher-game interface (contract version [`crate::CONTRACT_VERSION`]).
//!
//! The launcher starts two programs per game, both named in the game's
//! [`crate::manifest::GameManifest`], with argument lists (no shell) and
//! these environment variables:
//!
//! | variable | meaning |
//! |---|---|
//! | [`ENV_CONTRACT`] | contract version the launcher speaks |
//! | [`ENV_GAME_ID`] | the manifest id |
//! | [`ENV_DATA_DIR`] | the game's data folder (imports, saves, logs) |
//! | [`ENV_LAUNCHER_VERSION`] | the launcher's version label |
//! | [`ENV_SESSION_FILE`] | the [`SessionHandoff`] file (game only; may be missing) |
//!
//! **Importer**: exits 0 after writing its complete marker last; progress
//! lines `[n/total] text` on stdout or stderr when the manifest says
//! `"progress": "steps"`; the ISO is opened read-only.
//!
//! **Import check** (optional, `importer.verify` / `importer.check` in the
//! manifest): run without a console, stdin closed; prints one line
//! [`IMPORT_REPORT_PREFIX`] + an [`ImportReport`] as JSON on stdout (other
//! output is ignored; the last such line counts) and exits 0 whether or not
//! the import checks out. `verify` checks the import just made; `check`
//! finds the newest usable import when the launcher starts.
//!
//! **Launch toggle** (optional, `launch_toggle` in the manifest): a switch
//! beside Play, remembered per game in the launcher's state; its state adds
//! `args_on` or `args_off` after the executable's arguments (Roses:
//! "Online Play and extras", off adds `--faithful`).
//!
//! **Game**: writes nothing outside its data folder; reads the session
//! hand-off to sign in to online play without asking for the password;
//! exits 0 on a normal quit. Its stdout/stderr go to `logs/game.log` in the
//! data folder.

use serde::{Deserialize, Serialize};
use std::path::Path;

pub const ENV_CONTRACT: &str = "PLATFORM_CONTRACT";
pub const ENV_GAME_ID: &str = "PLATFORM_GAME_ID";
pub const ENV_DATA_DIR: &str = "PLATFORM_DATA_DIR";
pub const ENV_LAUNCHER_VERSION: &str = "PLATFORM_LAUNCHER_VERSION";
pub const ENV_SESSION_FILE: &str = "PLATFORM_SESSION_FILE";

/// What a signed-in launcher hands a game: the account service's session
/// (to resume) and, once the platform service exists, a short-lived
/// platform token for the game's own server (`crate::token`).
///
/// Today the Roses game reads its own `online-session.json` (same fields
/// minus `platform_token`); this is the shape every game reads later.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionHandoff {
    /// The account server ("host:port").
    pub server: String,
    pub username: String,
    /// Secret: resumes the session on `server`.
    pub session: String,
    /// A signed platform token for this game's audience (optional).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub platform_token: Option<String>,
}

impl SessionHandoff {
    pub fn load(path: &Path) -> Option<Self> {
        let text = std::fs::read_to_string(path).ok()?;
        if text.len() > 64 * 1024 {
            return None;
        }
        serde_json::from_str(&text).ok()
    }
}

/// The line prefix of an [`ImportReport`] on an import check's stdout.
pub const IMPORT_REPORT_PREFIX: &str = "platform-import-report: ";

/// What an import check found.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ImportReport {
    /// The import checks out (`check`: one usable import was found, or there
    /// is none yet and `import_dir` is absent).
    pub ok: bool,
    /// The usable import's folder (`check`; `verify` may repeat its own).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub import_dir: Option<String>,
    /// What the import holds, one line for the player ("854 cards, ...").
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    /// Why it does not check out (shown to the player and logged).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl ImportReport {
    /// The report line to print.
    pub fn line(&self) -> String {
        format!("{IMPORT_REPORT_PREFIX}{}", serde_json::to_string(self).expect("a report serialises"))
    }

    /// The last report line in a check's stdout.
    pub fn find(stdout: &str) -> Option<Self> {
        stdout.lines().rev().find_map(|line| serde_json::from_str(line.trim().strip_prefix(IMPORT_REPORT_PREFIX)?).ok())
    }
}

/// The environment for a game's importer or executable.
pub fn environment(game_id: &str, data_dir: &Path, launcher_version: &str, session_file: Option<&Path>) -> Vec<(&'static str, std::ffi::OsString)> {
    let mut env = vec![
        (ENV_CONTRACT, crate::CONTRACT_VERSION.to_string().into()),
        (ENV_GAME_ID, game_id.into()),
        (ENV_DATA_DIR, data_dir.as_os_str().to_owned()),
        (ENV_LAUNCHER_VERSION, launcher_version.into()),
    ];
    if let Some(file) = session_file {
        env.push((ENV_SESSION_FILE, file.as_os_str().to_owned()));
    }
    env
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_environment_names_the_game_and_its_folder() {
        let env = environment("roses", Path::new("D:/data"), "v1", None);
        assert!(env.contains(&(ENV_GAME_ID, "roses".into())));
        assert!(env.contains(&(ENV_CONTRACT, "1".into())));
        assert!(!env.iter().any(|(k, _)| *k == ENV_SESSION_FILE));
        // Today's Roses session file reads as a hand-off.
        let h: SessionHandoff = serde_json::from_str(r#"{"server":"h:1","username":"Yugi","session":"s"}"#).unwrap();
        assert_eq!(h.platform_token, None);
    }

    #[test]
    fn the_last_import_report_line_counts() {
        let good = ImportReport { ok: true, import_dir: Some("D:/data/game-1".into()), summary: Some("854 cards".into()), error: None };
        let bad = ImportReport { ok: false, error: Some("no".into()), ..ImportReport::default() };
        let out = format!("progress
{}
more
{}
", bad.line(), good.line());
        assert_eq!(ImportReport::find(&out), Some(good));
        assert_eq!(ImportReport::find("nothing here"), None);
        assert_eq!(ImportReport::find(&format!("{IMPORT_REPORT_PREFIX}{{\"ok\":true}}")), Some(ImportReport { ok: true, ..ImportReport::default() }));
    }
}
