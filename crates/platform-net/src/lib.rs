//! The platform's network layer (future `platform-sdk` repository, beside
//! `platform-sdk`): what the launcher and every game share to talk to an
//! online server. No dependency on any game crate.
//!
//! - [`transport`]: a message connection behind a small trait (framed JSON
//!   over TCP, or in memory for tests).
//! - [`tls`]: TLS for every connection that leaves the machine (web PKI or
//!   a pinned self-signed certificate).
//! - [`config`]: the server file (`online.json`), the saved session
//!   (`online-session.json`) and connecting to the server.
//! - [`account`]: the account protocol (sign-up, log-in, sessions, password
//!   change, email) every game lobby carries, and an account-only client.
//! - [`profile`]: the launcher Profile page's account jobs over it.
//!
//! A game builds its own lobby (challenges, rooms, matches) on these: its
//! lobby messages are a superset of [`account`]'s, and anything
//! game-specific in an account-only hello is game-defined JSON the launcher
//! reads from the game's manifest.

pub mod account;
pub mod config;
pub mod profile;
pub mod tls;
pub mod transport;
