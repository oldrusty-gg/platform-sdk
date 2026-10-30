//! The account protocol: what an account-only connection says (the
//! launcher's Profile page; no game data needed) and the account part of
//! every game lobby: sign-up (open, or with an invite code), log-in, session
//! resume, log-out, password change, and email (address, verification,
//! password reset).
//!
//! The messages are JSON objects tagged by `"type"`, framed as in
//! [`crate::transport`]. A game's own lobby protocol (its challenges, rooms,
//! match choices) is a superset: its request and event enums contain these
//! variants with exactly these shapes and these payload types, so the two
//! read each other (`roses_net::lobby` re-exports the types below). What a
//! game's server needs to accept an account-only hello besides the fields
//! here (e.g. its protocol version) is game-defined JSON carried in
//! [`AccountRequest::LobbyHello::game`]; the launcher takes it from the
//! game's manifest (`online.account_hello`).

use crate::transport::{Connection, Received};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::io;

/// The lobby version of the account messages (a game lobby that carries
/// them speaks the same number).
pub const LOBBY_VERSION: u32 = 1;

/// Usernames: 3..=12 letters and digits, starting with a letter (they are
/// drawn in the game's own fonts); unique ignoring case.
pub const USERNAME_MIN: usize = 3;
pub const USERNAME_MAX: usize = 12;
/// Passwords: 8..=64 printable ASCII characters.
pub const PASSWORD_MIN: usize = 8;
pub const PASSWORD_MAX: usize = 64;
/// Invite and room codes use this alphabet (no 0/O, 1/I/L).
pub const CODE_ALPHABET: &[u8] = b"ABCDEFGHJKMNPQRSTUVWXYZ23456789";

pub fn check_username(name: &str) -> Result<(), AuthProblem> {
    let ok = (USERNAME_MIN..=USERNAME_MAX).contains(&name.len())
        && name.chars().all(|c| c.is_ascii_alphanumeric())
        && name.starts_with(|c: char| c.is_ascii_alphabetic());
    if ok { Ok(()) } else { Err(AuthProblem::BadUsername) }
}

pub fn check_password(password: &str) -> Result<(), AuthProblem> {
    let ok = (PASSWORD_MIN..=PASSWORD_MAX).contains(&password.len())
        && password.chars().all(|c| c.is_ascii_graphic() || c == ' ');
    if ok { Ok(()) } else { Err(AuthProblem::WeakPassword) }
}

/// Names nobody may sign up with (compared ignoring case): the server's
/// own players and staff-sounding names that could fool the others.
pub const RESERVED_NAMES: &[&str] = &[
    "computer", "cpu", "server", "system", "admin", "administrator", "root", "owner", "host", "moderator", "mod",
    "staff", "support", "help", "official", "roses", "konami", "guest", "player", "nobody", "null", "none",
    "anonymous", "everyone", "unknown", "yugioh",
];

/// A name for a new account: [`check_username`] and not reserved (no
/// reserved word, nor one of the staff words with digits after it, such
/// as `Admin2`).
pub fn check_new_username(name: &str) -> Result<(), AuthProblem> {
    check_username(name)?;
    let key = username_key(name);
    let stem = key.trim_end_matches(|c: char| c.is_ascii_digit());
    let staff = ["admin", "administrator", "moderator", "mod", "server", "system", "root", "owner", "staff", "support", "official", "computer"];
    if RESERVED_NAMES.contains(&key.as_str()) || staff.contains(&stem) || key.starts_with("admin") || key.starts_with("moderator") {
        return Err(AuthProblem::ReservedName);
    }
    Ok(())
}

/// The comparison key of a username (unique ignoring case).
pub fn username_key(name: &str) -> String {
    name.to_ascii_lowercase()
}

/// An invite or room code as typed: upper case, without dashes or spaces.
pub fn normalize_code(code: &str) -> String {
    code.chars().filter(|c| !matches!(c, '-' | ' ')).map(|c| c.to_ascii_uppercase()).collect()
}

fn yes() -> bool {
    true
}

fn is_false(b: &bool) -> bool {
    !*b
}

/// An account's email state ([`AccountEvent::AccountStatus`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EmailState {
    /// No address yet (accounts from before email).
    NoEmail,
    /// An address waits for its code (and none is verified).
    Unverified,
    Verified,
    Disabled,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccountStatus {
    pub state: EmailState,
    /// The verified address.
    #[serde(default)]
    pub email: Option<String>,
    /// An address waiting for its code.
    #[serde(default)]
    pub pending_email: Option<String>,
    /// Ask the player for an address (none yet, not dismissed).
    #[serde(default)]
    pub prompt_email: bool,
    /// Online play needs a verified address on this server.
    #[serde(default)]
    pub verification_required: bool,
    /// Supporter perks (platform flags; cosmetic).
    #[serde(default)]
    pub supporter: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EmailProblem {
    BadEmail,
    EmailTaken,
    AlreadyVerified,
    TooSoon,
    TooManyEmails,
    BadCode,
    NoCode,
    TooManyTries,
    NoVerifiedEmail,
    Disabled,
    WeakPassword,
    /// This server sends no email.
    NotOffered,
    Busy,
    NotSignedIn,
}

impl std::fmt::Display for EmailProblem {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::BadEmail => "that is not an email address",
            Self::EmailTaken => "another account uses that email address",
            Self::AlreadyVerified => "that address is already verified",
            Self::TooSoon => "an email was just sent; wait a minute",
            Self::TooManyEmails => "too many emails today; try tomorrow",
            Self::BadCode => "that code is not right",
            Self::NoCode => "no code is waiting; ask for a new email",
            Self::TooManyTries => "too many wrong codes; ask for a new email",
            Self::NoVerifiedEmail => "this account has no verified email address",
            Self::Disabled => "this account is disabled",
            Self::WeakPassword => "a password is 8 to 64 characters",
            Self::NotOffered => "this server does not send email",
            Self::Busy => "the server is busy; try again",
            Self::NotSignedIn => "log in first",
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuthProblem {
    BadUsername,
    WeakPassword,
    UsernameTaken,
    BadInvite,
    BadCredentials,
    SessionExpired,
    Disabled,
    TooManyAttempts,
    /// The server is busy hashing passwords; try again shortly.
    Busy,
    /// A lobby request that needs a signed-in player.
    NotSignedIn,
    /// That name is kept for the server (COMPUTER, ADMIN, ...).
    ReservedName,
    /// Too many new accounts from this address lately (open sign-up).
    SignUpLimit,
    /// The server needs a verified email address before online play (the
    /// launcher's Profile page adds one). Sent only by servers that switched
    /// verification on.
    EmailNotVerified,
}

impl std::fmt::Display for AuthProblem {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::BadUsername => "a name is 3 to 12 letters or digits, starting with a letter",
            Self::WeakPassword => "a password is 8 to 64 characters",
            Self::UsernameTaken => "that name is taken",
            Self::BadInvite => "that invite code is not valid",
            Self::BadCredentials => "wrong name or password",
            Self::SessionExpired => "please log in again",
            Self::Disabled => "this account is disabled",
            Self::TooManyAttempts => "too many attempts; wait a few minutes",
            Self::Busy => "the server is busy; try again",
            Self::NotSignedIn => "log in first",
            Self::ReservedName => "that name is reserved; choose another",
            Self::SignUpLimit => "too many new accounts from here; try later",
            Self::EmailNotVerified => "verify your email address in the launcher's Profile page first",
        })
    }
}

/// What an account-only connection sends (a game lobby's requests include
/// these variants unchanged).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AccountRequest {
    /// The first message of a connection.
    LobbyHello {
        lobby_version: u32,
        client: String,
        /// An account-only connection: sign up, log in, log out, change the
        /// password and the email requests, without a game's rules data; it
        /// never enters a game lobby, and signing in there does not sign
        /// the game out.
        #[serde(default, skip_serializing_if = "is_false")]
        account: bool,
        /// The game's own hello fields (game-defined JSON, e.g. Roses'
        /// `compat`), sent as they are.
        #[serde(flatten)]
        game: Map<String, Value>,
    },
    /// Create an account. With an invite code from the server's owner, or
    /// with an empty `invite` when the server has open sign-up
    /// ([`AccountEvent::LobbyWelcome::invite_required`] false). A reset
    /// invite made for an existing name sets that account's password.
    SignUp {
        username: String,
        password: String,
        #[serde(default)]
        invite: String,
    },
    LogIn { username: String, password: String },
    /// Sign in again with a session token from an earlier sign-in.
    ResumeSession { session: String },
    /// End this session (the token stops working).
    LogOut,
    /// Change the signed-in account's password (the other sessions of the
    /// account end; this one stays).
    ChangePassword { old_password: String, new_password: String },
    Ping,
    // ---- email (only to a server whose welcome says `email`) ----------
    /// The signed-in account's email state: [`AccountEvent::AccountStatus`].
    AccountStatus,
    /// Give or change the account's address: a code is mailed to it
    /// ([`AccountEvent::EmailCodeSent`]); a verified address stays until
    /// the code is entered.
    SetEmail { email: String },
    /// The code from that email (answered with an `AccountStatus`).
    VerifyEmail { code: String },
    /// "Not now" to the launcher's request for an address.
    DismissEmailPrompt,
    /// Signed out: mail a password-reset code to the verified address of
    /// `account` (its name or its address). The answer
    /// ([`AccountEvent::ResetCodeSent`]) is the same whether or not there is
    /// such an account.
    RequestPasswordReset { account: String },
    /// Signed out: a new password with the code from that email
    /// ([`AccountEvent::PasswordReset`]; every session of the account ends).
    ResetPassword { account: String, code: String, new_password: String },
}

/// Why the server refused a connection's hello.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AccountRefusal {
    LobbyVersion { client: u32, server: u32 },
    /// The game's own compatibility check failed (game-defined detail).
    Incompatible { detail: Value },
    ServerFull,
    ExpectedHello,
}

/// What an account-only connection receives (a game lobby's events include
/// these variants; others are read as [`AccountEvent::Other`]).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AccountEvent {
    LobbyWelcome {
        lobby_version: u32,
        server: String,
        /// Sign-up needs an invite code (false: open sign-up). Servers from
        /// before open sign-up do not say, which means an invite is needed.
        #[serde(default = "yes")]
        invite_required: bool,
        /// The server sends email (addresses, verification, password
        /// reset): the email requests may be sent. Absent from older servers.
        #[serde(default, skip_serializing_if = "is_false")]
        email: bool,
        /// The game lobby's own welcome fields.
        #[serde(flatten)]
        game: Map<String, Value>,
    },
    LobbyRefused { reason: AccountRefusal },
    SignedIn {
        username: String,
        /// Secret: keep it to sign in again without the password.
        session: String,
    },
    AuthFailed { problem: AuthProblem },
    /// The session ended (log-out, or the same account signed in elsewhere).
    SignedOut { elsewhere: bool },
    /// The password was changed (other sessions of the account ended).
    PasswordChanged,
    Pong,
    // ---- email (answers to the email requests only) -------------------
    AccountStatus { status: AccountStatus },
    /// A verification code was mailed to the address just given.
    EmailCodeSent,
    /// If the account has a verified address, a reset code was mailed to it.
    ResetCodeSent,
    /// The password was reset: log in with the new one.
    PasswordReset,
    EmailFailed { problem: EmailProblem },
    /// A game lobby's own event (an account-only connection gets none).
    #[serde(other)]
    Other,
}

pub type AccountConn = Box<dyn Connection<AccountRequest, AccountEvent> + Send>;

/// An account-only connection as this client knows it, updated by
/// [`AccountClient::pump`].
pub struct AccountClient {
    conn: Option<AccountConn>,
    /// The server's name (from its welcome).
    pub server: Option<String>,
    /// Whether sign-up needs an invite code (from the welcome; true until
    /// the server says otherwise).
    pub invite_required: bool,
    /// The server sends email (verification, password reset).
    pub email: bool,
    /// The account's email state (the answer to `AccountStatus`).
    pub account_status: Option<AccountStatus>,
    /// The server confirmed a password change.
    pub password_changed: bool,
    pub refused: Option<AccountRefusal>,
    pub user: Option<String>,
    pub session: Option<String>,
    /// The last thing worth telling the player.
    pub note: Option<String>,
    /// The last authentication problem (cleared by the next request).
    pub auth_problem: Option<AuthProblem>,
}

impl AccountClient {
    /// Say hello on a fresh connection as an account-only client; `game`:
    /// the game server's own hello fields (its manifest's
    /// `online.account_hello`).
    pub fn start(mut conn: AccountConn, client: &str, game: Map<String, Value>) -> io::Result<Self> {
        conn.send(&AccountRequest::LobbyHello { lobby_version: LOBBY_VERSION, client: client.into(), account: true, game })?;
        Ok(Self {
            conn: Some(conn),
            server: None,
            invite_required: true,
            email: false,
            account_status: None,
            password_changed: false,
            refused: None,
            user: None,
            session: None,
            note: None,
            auth_problem: None,
        })
    }

    pub fn connected(&self) -> bool {
        self.conn.is_some()
    }

    pub fn send(&mut self, request: AccountRequest) {
        if matches!(
            request,
            AccountRequest::SignUp { .. }
                | AccountRequest::LogIn { .. }
                | AccountRequest::ResumeSession { .. }
                | AccountRequest::ChangePassword { .. }
        ) {
            self.auth_problem = None;
            self.password_changed = false;
        }
        let Some(conn) = self.conn.as_mut() else { return };
        if conn.send(&request).is_err() {
            self.close("the connection to the server broke");
        }
    }

    pub fn close(&mut self, why: &str) {
        if let Some(mut c) = self.conn.take() {
            c.close();
        }
        self.note = Some(why.into());
    }

    /// Read everything waiting; returns the events in order.
    pub fn pump(&mut self) -> Vec<AccountEvent> {
        let mut out = Vec::new();
        loop {
            let Some(conn) = self.conn.as_mut() else { break };
            match conn.poll() {
                Received::Message(e) => {
                    self.apply(&e);
                    out.push(e);
                }
                Received::Empty => break,
                Received::Closed => {
                    self.close("the server closed the connection");
                    break;
                }
                Received::Malformed(m) => {
                    self.close(&format!("the server sent something unreadable: {m}"));
                    break;
                }
            }
        }
        out
    }

    fn apply(&mut self, e: &AccountEvent) {
        match e {
            AccountEvent::LobbyWelcome { server, invite_required, email, .. } => {
                self.email = *email;
                self.server = Some(server.clone());
                self.invite_required = *invite_required;
            }
            AccountEvent::PasswordChanged => {
                self.password_changed = true;
                self.auth_problem = None;
                self.note = Some("password changed".into());
            }
            AccountEvent::LobbyRefused { reason } => {
                self.refused = Some(reason.clone());
                self.note = Some(match reason {
                    AccountRefusal::LobbyVersion { .. } | AccountRefusal::Incompatible { .. } => {
                        "this version does not match the server's; update".into()
                    }
                    AccountRefusal::ServerFull => "the server is full".into(),
                    AccountRefusal::ExpectedHello => "the server did not understand this client".into(),
                });
            }
            AccountEvent::SignedIn { username, session } => {
                self.user = Some(username.clone());
                self.session = Some(session.clone());
                self.auth_problem = None;
                self.note = None;
            }
            AccountEvent::AuthFailed { problem } => {
                self.auth_problem = Some(*problem);
                self.note = Some(problem.to_string());
            }
            AccountEvent::SignedOut { elsewhere } => {
                self.user = None;
                self.session = None;
                self.note = Some(if *elsewhere { "signed in from another place".into() } else { "logged out".into() });
            }
            AccountEvent::AccountStatus { status } => self.account_status = Some(status.clone()),
            AccountEvent::EmailFailed { problem } => self.note = Some(problem.to_string()),
            AccountEvent::Pong
            | AccountEvent::EmailCodeSent
            | AccountEvent::ResetCodeSent
            | AccountEvent::PasswordReset
            | AccountEvent::Other => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{from_str, json, to_value};

    #[test]
    fn new_names_skip_reserved_ones() {
        for ok in ["Yugi", "Joey2", "Moderna", "Hosting", "Player1", "Rootbeer"] {
            assert_eq!(check_new_username(ok), Ok(()), "{ok}");
        }
        for reserved in ["COMPUTER", "computer", "Admin", "admin7", "Administra", "AdminYugi", "Server", "Mod", "Moderator2", "Roses", "Guest"] {
            assert_eq!(check_new_username(reserved), Err(AuthProblem::ReservedName), "{reserved}");
        }
        assert_eq!(check_new_username("ad"), Err(AuthProblem::BadUsername));
        // Existing accounts still log in with any valid name.
        assert!(check_username("Admin").is_ok());
    }

    #[test]
    fn names_passwords_and_codes() {
        assert!(check_username("Yugi").is_ok());
        assert!(check_username("Joey2").is_ok());
        for bad in ["Yu", "2Joey", "Yugi Muto", "Yugi_M", "ThirteenChars"] {
            assert!(check_username(bad).is_err(), "{bad}");
        }
        assert!(check_password("eight ch").is_ok());
        assert!(check_password("seven c").is_err());
        assert!(check_password("tab\tinside").is_err());
        assert_eq!(normalize_code("abcd-ef gh"), "ABCDEFGH");
    }

    /// The account hello carries the game's own fields at the top level, as
    /// a game client's hello does; nothing else is added.
    #[test]
    fn the_hello_carries_the_game_fields_as_they_are() {
        let game = json!({"compat": {"protocol": 4, "mods": []}}).as_object().unwrap().clone();
        let hello = AccountRequest::LobbyHello { lobby_version: 1, client: "launcher 1".into(), account: true, game };
        assert_eq!(
            to_value(&hello).unwrap(),
            json!({"type": "lobby_hello", "lobby_version": 1, "client": "launcher 1", "account": true, "compat": {"protocol": 4, "mods": []}})
        );
        assert_eq!(from_str::<AccountRequest>(&serde_json::to_string(&hello).unwrap()).unwrap(), hello);
        assert_eq!(to_value(AccountRequest::LogOut).unwrap(), json!({"type": "log_out"}));
        let up: AccountRequest = from_str(r#"{"type":"sign_up","username":"Yugi","password":"password1"}"#).unwrap();
        assert_eq!(up, AccountRequest::SignUp { username: "Yugi".into(), password: "password1".into(), invite: String::new() });
    }

    /// A game server's welcome and its other events read as account events.
    #[test]
    fn game_lobby_events_read_as_account_events() {
        let welcome: AccountEvent = from_str(r#"{"type":"lobby_welcome","lobby_version":1,"server":"S","computer":true,"choices":true,"email":true}"#).unwrap();
        let AccountEvent::LobbyWelcome { invite_required, email, game, .. } = welcome else { panic!("{welcome:?}") };
        assert!(invite_required && email, "older servers do not say: invites needed");
        assert_eq!(game.get("computer"), Some(&json!(true)));
        let players: AccountEvent = from_str(r#"{"type":"players","players":[]}"#).unwrap();
        assert_eq!(players, AccountEvent::Other);
        let refused: AccountEvent =
            from_str(r#"{"type":"lobby_refused","reason":{"kind":"incompatible","detail":{"kind":"protocol","client":"3","server":"4"}}}"#).unwrap();
        assert!(matches!(refused, AccountEvent::LobbyRefused { reason: AccountRefusal::Incompatible { .. } }));
    }
}
