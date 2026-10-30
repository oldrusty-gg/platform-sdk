//! The shared platform contract (future `platform-sdk` repository).
//!
//! Everything here is used by more than one of: the launcher, the platform
//! account service, and each game (client, importer, online server). It has
//! no dependency on any game crate.
//!
//! - [`manifest`]: the per-game manifest the launcher's registry reads (id,
//!   names, required ISO fingerprints, importer, executable, online server,
//!   mod support).
//! - [`contract`]: how the launcher starts a game's importer and executable
//!   (arguments, environment variables, the session hand-off file).
//! - [`mods`]: the mod manifest format and load-order resolution.
//! - [`token`]: platform access tokens, Ed25519-signed by the account
//!   service and verified offline by every game server.
//! - [`b64`]: base64url without padding (tokens, links).

pub mod b64;
pub mod contract;
pub mod manifest;
pub mod mods;
pub mod token;

/// Bumped when the launcher-game contract changes incompatibly. A game
/// manifest names the contract version it was written for.
pub const CONTRACT_VERSION: u32 = 1;
