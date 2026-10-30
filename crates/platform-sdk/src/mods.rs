//! Mod manifests and load order, shared by every game.
//!
//! A mod is a folder `mods/<id>/` in a game's data folder with `mod.json`.
//! Mods are data only in API 1 (replacement assets, text, tables a game
//! declares moddable); no mod code is executed. Which files a game accepts
//! and how it applies them is the game's business; this module only says
//! which mods are enabled and in what order.

use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModManifest {
    /// 2..=48 of `a-z0-9-` (folder name).
    pub id: String,
    pub name: String,
    pub version: String,
    /// The game this mod is for (its manifest id), or several for a
    /// cross-game mod (for example a shared UI theme).
    pub games: Vec<String>,
    /// The mod API version ([`crate::manifest::ModSupport::api`]).
    pub api: u32,
    #[serde(default)]
    pub authors: Vec<String>,
    /// Mods that must load before this one.
    #[serde(default)]
    pub requires: Vec<String>,
    /// Mods this one should load after when both are enabled.
    #[serde(default)]
    pub after: Vec<String>,
    /// Mods that cannot be enabled together with this one.
    #[serde(default)]
    pub conflicts: Vec<String>,
    /// Changes competitive play (cards, rules): such mods are refused by
    /// ranked/public online servers and must match on both sides in
    /// private rooms.
    #[serde(default)]
    pub affects_online: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LoadOrderError {
    Missing { mod_id: String, requires: String },
    Conflict(String, String),
    Cycle(Vec<String>),
    WrongGame { mod_id: String },
    WrongApi { mod_id: String, api: u32 },
}

/// The order to apply `enabled` mods for `game` (API `api`): requirements
/// and `after` first, ties broken by the player's order in `enabled`.
pub fn load_order(game: &str, api: u32, mods: &[ModManifest], enabled: &[String]) -> Result<Vec<String>, LoadOrderError> {
    let by_id: BTreeMap<&str, &ModManifest> = mods.iter().map(|m| (m.id.as_str(), m)).collect();
    let on: BTreeSet<&str> = enabled.iter().map(String::as_str).filter(|id| by_id.contains_key(id)).collect();
    for id in &on {
        let m = by_id[id];
        if !m.games.iter().any(|g| g == game) {
            return Err(LoadOrderError::WrongGame { mod_id: m.id.clone() });
        }
        if m.api != api {
            return Err(LoadOrderError::WrongApi { mod_id: m.id.clone(), api: m.api });
        }
        if let Some(r) = m.requires.iter().find(|r| !on.contains(r.as_str())) {
            return Err(LoadOrderError::Missing { mod_id: m.id.clone(), requires: r.clone() });
        }
        if let Some(c) = m.conflicts.iter().find(|c| on.contains(c.as_str())) {
            return Err(LoadOrderError::Conflict(m.id.clone(), c.clone()));
        }
    }
    let rank: BTreeMap<&str, usize> = enabled.iter().enumerate().map(|(i, id)| (id.as_str(), i)).collect();
    let mut placed: Vec<String> = Vec::new();
    let mut left: Vec<&str> = on.iter().copied().collect();
    left.sort_by_key(|id| rank[id]);
    while !left.is_empty() {
        let ready = left.iter().position(|id| {
            let m = by_id[id];
            m.requires.iter().chain(&m.after).all(|d| !on.contains(d.as_str()) || placed.iter().any(|p| p == d))
        });
        match ready {
            Some(i) => placed.push(left.remove(i).to_string()),
            None => return Err(LoadOrderError::Cycle(left.iter().map(|s| s.to_string()).collect())),
        }
    }
    Ok(placed)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn m(id: &str, requires: &[&str], after: &[&str], conflicts: &[&str]) -> ModManifest {
        let v = |x: &[&str]| x.iter().map(|s| s.to_string()).collect();
        ModManifest {
            id: id.into(),
            name: id.into(),
            version: "1".into(),
            games: vec!["roses".into()],
            api: 1,
            authors: vec![],
            requires: v(requires),
            after: v(after),
            conflicts: v(conflicts),
            affects_online: false,
        }
    }

    #[test]
    fn requirements_load_first_and_player_order_breaks_ties() {
        let mods = [m("hud", &["fonts"], &[], &[]), m("fonts", &[], &[], &[]), m("music", &[], &["hud"], &[])];
        let on = |ids: &[&str]| ids.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(load_order("roses", 1, &mods, &on(&["music", "hud", "fonts"])).unwrap(), on(&["fonts", "hud", "music"]));
        assert_eq!(load_order("roses", 1, &mods, &on(&["fonts", "music"])).unwrap(), on(&["fonts", "music"]));
        assert!(matches!(load_order("roses", 1, &mods, &on(&["hud"])), Err(LoadOrderError::Missing { .. })));
        assert!(matches!(load_order("other", 1, &mods, &on(&["fonts"])), Err(LoadOrderError::WrongGame { .. })));
        assert!(matches!(load_order("roses", 2, &mods, &on(&["fonts"])), Err(LoadOrderError::WrongApi { .. })));
    }

    #[test]
    fn conflicts_and_cycles_are_reported() {
        let mods = [m("a", &[], &["b"], &[]), m("b", &[], &["a"], &[]), m("c", &[], &[], &["a"])];
        let on = |ids: &[&str]| ids.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert!(matches!(load_order("roses", 1, &mods, &on(&["a", "b"])), Err(LoadOrderError::Cycle(_))));
        assert!(matches!(load_order("roses", 1, &mods, &on(&["a", "c"])), Err(LoadOrderError::Conflict(..))));
    }
}
