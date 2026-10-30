//! The launcher's Profile page jobs: talk to a game's online server over
//! the account protocol ([`crate::account`], an account-only connection, so
//! no game import is needed) and keep the signed-in session in the game's
//! `online-session.json`, where the game's online menus read it: signing in
//! here signs the game in.
//!
//! Every job is blocking and runs on a worker thread; it opens one
//! connection, does one thing and closes it. Errors are plain sentences for
//! the page.

use crate::account::{
    check_new_username, check_password, check_username, normalize_code, AccountClient, AccountEvent as LobbyEvent,
    AccountRefusal as LobbyRefusal, AccountRequest as LobbyRequest, AccountStatus, AuthProblem, EmailProblem,
    EmailState,
};
use crate::config::{self as friends, OnlineConfig, SavedSession};
use serde_json::{Map, Value};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// How long a job waits for the server.
pub const TIMEOUT: Duration = Duration::from_secs(15);

/// Where the server file and the session are for one data folder, and how
/// to greet that server.
#[derive(Clone, Debug)]
pub struct Paths {
    /// The `online.json` in use (the player's own, else the package's).
    pub config: PathBuf,
    pub session: PathBuf,
    /// What the client calls itself in its hello ("launcher 0.1.0").
    pub client: String,
    /// The game server's own hello fields (the game manifest's
    /// `online.account_hello`).
    pub hello: Map<String, Value>,
}

impl Paths {
    pub fn for_data_root(data_root: &Path) -> Self {
        Self {
            config: friends::find_config(data_root),
            session: data_root.join(friends::SESSION_FILE),
            client: concat!("platform-net ", env!("CARGO_PKG_VERSION")).into(),
            hello: Map::new(),
        }
    }

    /// The same with the client's name and the game server's hello fields.
    pub fn greeting(mut self, client: &str, hello: Map<String, Value>) -> Self {
        self.client = client.into();
        self.hello = hello;
        self
    }

    /// The server file, or why there is none (a sentence for the page).
    pub fn load_config(&self) -> Result<OnlineConfig, String> {
        if !self.config.is_file() {
            return Err("No server is set up: online.json is missing from this download.".into());
        }
        OnlineConfig::load(&self.config).map_err(|e| format!("The server file online.json cannot be read: {e}"))
    }

    /// The signed-in name saved for `config`'s server, without asking it.
    pub fn saved(&self, config: &OnlineConfig) -> Option<SavedSession> {
        SavedSession::load(&self.session, &config.server)
    }
}

/// What the page asks for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Job {
    /// Read the server's welcome (its name, whether sign-up needs an
    /// invite) and check the saved session.
    Check,
    SignUp { username: String, password: String, invite: String },
    LogIn { username: String, password: String },
    LogOut,
    ChangePassword { old_password: String, new_password: String },
    /// Give or change the account's email address (a code is mailed).
    SetEmail { email: String },
    /// The code from that email.
    VerifyEmail { code: String },
    /// "Not now" to adding an address.
    DismissEmailPrompt,
    /// Signed out: mail a reset code to the account's verified address.
    RequestReset { account: String },
    /// Signed out: a new password with that code.
    ResetPassword { account: String, code: String, new_password: String },
}

impl Job {
    /// A short name for the launcher log (never the fields: passwords).
    pub fn label(&self) -> &'static str {
        match self {
            Job::Check => "check",
            Job::SignUp { .. } => "sign up",
            Job::LogIn { .. } => "log in",
            Job::LogOut => "log out",
            Job::ChangePassword { .. } => "change password",
            Job::SetEmail { .. } => "set email",
            Job::VerifyEmail { .. } => "verify email",
            Job::DismissEmailPrompt => "not now (email)",
            Job::RequestReset { .. } => "request password reset",
            Job::ResetPassword { .. } => "reset password",
        }
    }
}

/// What a finished job found.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct Outcome {
    /// The server's name from its welcome.
    pub server_name: Option<String>,
    /// Whether sign-up needs an invite code (from the welcome).
    pub invite_required: Option<bool>,
    /// Whether the server sends email (from the welcome).
    pub email_offered: Option<bool>,
    /// Signed in now as (None: signed out).
    pub signed_in: Option<String>,
    /// The account's email state (servers that send email).
    pub email: Option<AccountStatus>,
    /// A sentence for the page.
    pub message: String,
}

/// The Profile page's line about the account's address.
pub fn email_line(status: &AccountStatus) -> String {
    let required = if status.verification_required { " Online play needs a verified address." } else { "" };
    match (status.state, &status.email, &status.pending_email) {
        (EmailState::Disabled, ..) => "This account is disabled.".into(),
        (EmailState::Verified, Some(email), Some(pending)) => {
            format!("Email: {email} (verified). Waiting for the code sent to {pending}.")
        }
        (EmailState::Verified, Some(email), None) => format!("Email: {email} (verified)."),
        (_, _, Some(pending)) => format!("Email: {pending}, not verified yet: type the code we sent.{required}"),
        _ => format!("No email address yet. Add one to reset a forgotten password.{required}"),
    }
}

/// Check an address before it is sent (the server checks properly).
pub fn check_email(email: &str) -> Result<(), String> {
    let email = email.trim();
    match email.split_once('@') {
        Some((local, domain)) if !local.is_empty() && domain.contains('.') && !email.contains(' ') && email.len() <= 254 => Ok(()),
        _ => Err("Type an email address, like name@example.com.".into()),
    }
}

/// Check a code from an email (`XXXX-XXXX`, dash and case optional).
pub fn check_code(code: &str) -> Result<(), String> {
    let code = normalize_code(code);
    if code.len() == 8 && code.chars().all(|c| c.is_ascii_alphanumeric()) {
        Ok(())
    } else {
        Err("Type the code from the email (XXXX-XXXX).".into())
    }
}

/// An email request's refusal as a sentence for the page.
pub fn email_problem_text(p: EmailProblem) -> String {
    match p {
        EmailProblem::BadEmail => "That is not an email address.".into(),
        EmailProblem::EmailTaken => "Another account uses that email address.".into(),
        EmailProblem::AlreadyVerified => "That address is already verified.".into(),
        EmailProblem::TooSoon => "An email was just sent. Wait a minute before asking again.".into(),
        EmailProblem::TooManyEmails => "Too many emails today. Try again tomorrow.".into(),
        EmailProblem::BadCode => "That code is not right. Check the email and try again.".into(),
        EmailProblem::NoCode => "No code is waiting (it may have expired). Ask for a new email.".into(),
        EmailProblem::TooManyTries => "Too many wrong codes. Ask for a new email.".into(),
        EmailProblem::NoVerifiedEmail => "This account has no verified email address.".into(),
        EmailProblem::Disabled => "This account is disabled. Ask the server's owner.".into(),
        EmailProblem::WeakPassword => "A password is 8 to 64 characters.".into(),
        EmailProblem::NotOffered => "This server does not send email yet.".into(),
        EmailProblem::Busy => "The server is busy. Try again in a moment.".into(),
        EmailProblem::NotSignedIn => "Please log in first.".into(),
    }
}

/// Check a sign-up form before anything is sent (same rules as the server).
pub fn check_sign_up(username: &str, password: &str, confirm: &str, invite: Option<&str>) -> Result<(), String> {
    check_new_username(username).map_err(problem_text)?;
    check_password(password).map_err(problem_text)?;
    if password != confirm {
        return Err("The two passwords are different. Type the same password twice.".into());
    }
    if let Some(invite) = invite {
        if normalize_code(invite).len() != 8 {
            return Err("Type the invite code you were given (XXXX-XXXX).".into());
        }
    }
    Ok(())
}

pub fn check_log_in(username: &str, password: &str) -> Result<(), String> {
    if username.trim().is_empty() || password.is_empty() {
        return Err("Type your name and your password.".into());
    }
    check_username(username.trim()).map_err(|_| "Wrong name or password.".to_string())
}

pub fn check_change(old: &str, new: &str, confirm: &str) -> Result<(), String> {
    if old.is_empty() {
        return Err("Type your current password.".into());
    }
    check_password(new).map_err(problem_text)?;
    if new != confirm {
        return Err("The two new passwords are different. Type the same password twice.".into());
    }
    Ok(())
}

/// A server's refusal as a sentence for the page.
pub fn problem_text(p: AuthProblem) -> String {
    match p {
        AuthProblem::BadUsername => "A name is 3 to 12 letters or digits and starts with a letter.".into(),
        AuthProblem::WeakPassword => "A password is 8 to 64 characters.".into(),
        AuthProblem::UsernameTaken => "That name is taken. Choose another one.".into(),
        AuthProblem::BadInvite => "That invite code is not valid.".into(),
        AuthProblem::BadCredentials => "Wrong name or password.".into(),
        AuthProblem::SessionExpired => "Your sign-in has expired. Please log in again.".into(),
        AuthProblem::Disabled => "This account is disabled. Ask the server's owner.".into(),
        AuthProblem::TooManyAttempts => "Too many tries. Wait a few minutes and try again.".into(),
        AuthProblem::Busy => "The server is busy. Try again in a moment.".into(),
        AuthProblem::NotSignedIn => "Please log in first.".into(),
        AuthProblem::ReservedName => "That name is reserved. Choose another one.".into(),
        AuthProblem::SignUpLimit => "Too many new accounts from your connection. Try again later.".into(),
        AuthProblem::EmailNotVerified => "This server needs a verified email address: add one on this page.".into(),
    }
}

fn refusal_text(r: &LobbyRefusal) -> String {
    match r {
        LobbyRefusal::LobbyVersion { .. } | LobbyRefusal::Incompatible { .. } => {
            "This launcher does not match the server's version. Get the newest download.".into()
        }
        LobbyRefusal::ServerFull => "The server is full right now. Try again later.".into(),
        LobbyRefusal::ExpectedHello => "The server did not understand the launcher.".into(),
    }
}

/// An open account-only connection with the server's welcome read.
struct Session {
    client: AccountClient,
    outcome: Outcome,
}

fn wait(client: &mut AccountClient, mut done: impl FnMut(&AccountClient, &[LobbyEvent]) -> bool) -> Result<(), String> {
    let start = Instant::now();
    loop {
        let events = client.pump();
        if let Some(r) = &client.refused {
            return Err(refusal_text(r));
        }
        if done(client, &events) {
            return Ok(());
        }
        if !client.connected() {
            return Err("The connection to the server was lost. Try again.".into());
        }
        if start.elapsed() > TIMEOUT {
            return Err("The server did not answer. Try again later.".into());
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn open(paths: &Paths, config: &OnlineConfig) -> Result<Session, String> {
    let conn = friends::connect(config).map_err(|e| {
        eprintln!("Profile: connecting to {} failed: {e}", config.server);
        format!("Can't reach the server ({}). Check your internet connection, or try again later.", config.server)
    })?;
    let mut client = AccountClient::start(conn, &paths.client, paths.hello.clone())
        .map_err(|e| format!("Can't reach the server ({}): {e}", config.server))?;
    wait(&mut client, |c, _| c.server.is_some())?;
    let outcome = Outcome {
        server_name: client.server.clone(),
        invite_required: Some(client.invite_required),
        email_offered: Some(client.email),
        ..Outcome::default()
    };
    Ok(Session { client, outcome })
}

/// Send `request` and wait for sign-in (Ok) or a refusal (Err).
fn sign_in(s: &mut Session, request: LobbyRequest) -> Result<(String, String), String> {
    s.client.send(request);
    let mut failed = None;
    wait(&mut s.client, |c, events| {
        failed = events.iter().find_map(|e| match e {
            LobbyEvent::AuthFailed { problem } => Some(*problem),
            _ => None,
        });
        failed.is_some() || c.session.is_some()
    })?;
    match (failed, s.client.user.clone(), s.client.session.clone()) {
        (None, Some(user), Some(session)) => Ok((user, session)),
        (Some(p), ..) => Err(problem_text(p)),
        _ => Err("The server did not sign you in. Try again.".into()),
    }
}

/// Resume the saved session (Ok(None): there is none or it has expired;
/// an expired one is forgotten).
fn resume(s: &mut Session, paths: &Paths, config: &OnlineConfig) -> Result<Option<String>, String> {
    let Some(saved) = paths.saved(config) else { return Ok(None) };
    match sign_in(s, LobbyRequest::ResumeSession { session: saved.session }) {
        Ok((user, _)) => Ok(Some(user)),
        Err(e) if e == problem_text(AuthProblem::SessionExpired) || e == problem_text(AuthProblem::Disabled) => {
            SavedSession::forget(&paths.session);
            Ok(None)
        }
        Err(e) => Err(e),
    }
}

fn keep(paths: &Paths, config: &OnlineConfig, username: &str, session: &str) -> Result<(), String> {
    SavedSession { server: config.server.clone(), username: username.into(), session: session.into() }
        .save(&paths.session)
        .map_err(|e| format!("Signed in, but the sign-in could not be saved in {}: {e}", paths.session.display()))
}

/// Send an email request and wait for its answer: the account's status
/// (stored in the outcome), a code sent, a password reset, or a refusal
/// (Err, as a sentence).
fn email_request(s: &mut Session, request: LobbyRequest, until: fn(&LobbyEvent) -> bool) -> Result<LobbyEvent, String> {
    if !s.client.email {
        return Err(email_problem_text(EmailProblem::NotOffered));
    }
    s.client.send(request);
    let mut answer = None;
    wait(&mut s.client, |_, events| {
        for e in events {
            if let LobbyEvent::AccountStatus { status } = e {
                s.outcome.email = Some(status.clone());
            }
            if answer.is_none() && (until(e) || matches!(e, LobbyEvent::EmailFailed { .. })) {
                answer = Some(e.clone());
            }
        }
        answer.is_some()
    })?;
    match answer {
        Some(LobbyEvent::EmailFailed { problem }) => Err(email_problem_text(problem)),
        Some(e) => Ok(e),
        None => Err("The server did not answer. Try again later.".into()),
    }
}

fn is_status(e: &LobbyEvent) -> bool {
    matches!(e, LobbyEvent::AccountStatus { .. })
}

/// The signed-in account's email state, when the server sends email
/// (best effort: an old server or a hiccup leaves it unknown).
fn fetch_status(s: &mut Session) {
    if s.client.email && s.client.user.is_some() {
        let _ = email_request(s, LobbyRequest::AccountStatus, is_status);
    }
}

/// Run one job against the server in `paths` (blocking).
pub fn run(paths: &Paths, job: Job) -> Result<Outcome, String> {
    let config = paths.load_config()?;
    let mut s = open(paths, &config)?;
    let result = match job {
        Job::Check => {
            let user = resume(&mut s, paths, &config)?;
            s.outcome.message = match &user {
                Some(name) => format!("Signed in as {name}. The game's Online Play uses this account."),
                None => String::new(),
            };
            s.outcome.signed_in = user;
            fetch_status(&mut s);
            Ok(())
        }
        Job::SignUp { username, password, invite } => {
            let invite = if s.outcome.invite_required == Some(true) { invite } else { String::new() };
            let (user, session) =
                sign_in(&mut s, LobbyRequest::SignUp { username: username.trim().into(), password, invite })?;
            keep(paths, &config, &user, &session)?;
            s.outcome.message = format!("Welcome, {user}! Your account is ready and the game is signed in.");
            s.outcome.signed_in = Some(user);
            fetch_status(&mut s);
            Ok(())
        }
        Job::LogIn { username, password } => {
            let (user, session) = sign_in(&mut s, LobbyRequest::LogIn { username: username.trim().into(), password })?;
            keep(paths, &config, &user, &session)?;
            s.outcome.message = format!("Signed in as {user}. The game's Online Play uses this account.");
            s.outcome.signed_in = Some(user);
            fetch_status(&mut s);
            Ok(())
        }
        Job::LogOut => {
            if resume(&mut s, paths, &config)?.is_some() {
                s.client.send(LobbyRequest::LogOut);
                wait(&mut s.client, |c, _| c.user.is_none())?;
            }
            SavedSession::forget(&paths.session);
            s.outcome.message = "Logged out. The game is signed out too.".into();
            Ok(())
        }
        Job::ChangePassword { old_password, new_password } => {
            let Some(user) = resume(&mut s, paths, &config)? else {
                return Err(problem_text(AuthProblem::SessionExpired));
            };
            s.client.send(LobbyRequest::ChangePassword { old_password, new_password });
            let mut failed = None;
            wait(&mut s.client, |c, events| {
                failed = events.iter().find_map(|e| match e {
                    LobbyEvent::AuthFailed { problem } => Some(*problem),
                    _ => None,
                });
                failed.is_some() || c.password_changed
            })?;
            if let Some(p) = failed {
                return Err(match p {
                    AuthProblem::BadCredentials => "Your current password is wrong.".into(),
                    p => problem_text(p),
                });
            }
            s.outcome.message = "Password changed. Other places signed in to this account were signed out.".into();
            s.outcome.signed_in = Some(user);
            Ok(())
        }
        Job::SetEmail { email } => {
            let Some(user) = resume(&mut s, paths, &config)? else {
                return Err(problem_text(AuthProblem::SessionExpired));
            };
            s.outcome.signed_in = Some(user);
            let email = email.trim().to_string();
            email_request(&mut s, LobbyRequest::SetEmail { email: email.clone() }, is_status)?;
            s.outcome.message = format!("We sent a code to {email}. Type it below to confirm the address.");
            Ok(())
        }
        Job::VerifyEmail { code } => {
            let Some(user) = resume(&mut s, paths, &config)? else {
                return Err(problem_text(AuthProblem::SessionExpired));
            };
            s.outcome.signed_in = Some(user);
            email_request(&mut s, LobbyRequest::VerifyEmail { code: code.trim().into() }, is_status)?;
            s.outcome.message = "Your email address is verified.".into();
            Ok(())
        }
        Job::DismissEmailPrompt => {
            let Some(user) = resume(&mut s, paths, &config)? else {
                return Err(problem_text(AuthProblem::SessionExpired));
            };
            s.outcome.signed_in = Some(user);
            email_request(&mut s, LobbyRequest::DismissEmailPrompt, is_status)?;
            Ok(())
        }
        Job::RequestReset { account } => {
            email_request(&mut s, LobbyRequest::RequestPasswordReset { account: account.trim().into() }, |e| {
                matches!(e, LobbyEvent::ResetCodeSent)
            })?;
            s.outcome.message =
                "If that account has a verified email address, a reset code is on its way. Type it below with a new password.".into();
            Ok(())
        }
        Job::ResetPassword { account, code, new_password } => {
            let request = LobbyRequest::ResetPassword { account: account.trim().into(), code: code.trim().into(), new_password };
            email_request(&mut s, request, |e| matches!(e, LobbyEvent::PasswordReset))?;
            // Every session of the account ended, this PC's too.
            SavedSession::forget(&paths.session);
            s.outcome.message = "Your password is reset. Log in with the new one.".into();
            Ok(())
        }
    };
    s.client.close("done");
    result.map(|()| s.outcome)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn forms_are_checked_before_anything_is_sent() {
        assert_eq!(check_sign_up("Yugi", "password1", "password1", None), Ok(()));
        assert!(check_sign_up("Yugi", "password1", "password2", None).unwrap_err().contains("different"));
        assert!(check_sign_up("Yu", "password1", "password1", None).unwrap_err().contains("3 to 12"));
        assert!(check_sign_up("Admin", "password1", "password1", None).unwrap_err().contains("reserved"));
        assert!(check_sign_up("Yugi", "short", "short", None).unwrap_err().contains("8 to 64"));
        assert!(check_sign_up("Yugi", "password1", "password1", Some("")).unwrap_err().contains("invite"));
        assert_eq!(check_sign_up("Yugi", "password1", "password1", Some("abcd-efgh")), Ok(()));
        assert!(check_log_in("", "x").is_err());
        assert!(check_change("old", "newpassword", "newpasswort").unwrap_err().contains("different"));
        assert_eq!(check_change("old", "newpassword", "newpassword"), Ok(()));
        assert_eq!(check_email(" yugi@example.com "), Ok(()));
        assert!(check_email("yugi").is_err() && check_email("a b@c.de").is_err());
        assert_eq!(Job::ResetPassword { account: "a".into(), code: "b".into(), new_password: "secret".into() }.label(), "reset password");
    }

    #[test]
    fn the_email_line_says_where_the_account_stands() {
        let status = |state, email: Option<&str>, pending: Option<&str>, required| AccountStatus {
            state,
            email: email.map(Into::into),
            pending_email: pending.map(Into::into),
            prompt_email: false,
            verification_required: required,
            supporter: 0,
        };
        assert!(email_line(&status(EmailState::NoEmail, None, None, false)).starts_with("No email address yet"));
        assert!(email_line(&status(EmailState::NoEmail, None, None, true)).contains("needs a verified address"));
        assert!(email_line(&status(EmailState::Unverified, None, Some("a@b.co"), false)).contains("a@b.co, not verified"));
        assert_eq!(email_line(&status(EmailState::Verified, Some("a@b.co"), None, true)), "Email: a@b.co (verified).");
        assert!(email_line(&status(EmailState::Verified, Some("a@b.co"), Some("c@d.co"), false)).contains("c@d.co"));
    }

    #[test]
    fn no_server_file_is_said_plainly() {
        let dir = std::env::temp_dir().join(format!("platform-profile-none-{}", std::process::id()));
        let paths = Paths::for_data_root(&dir.join("roses-data"));
        let paths = Paths { config: dir.join("online.json"), session: dir.join(friends::SESSION_FILE), ..paths };
        let e = run(&paths, Job::Check).unwrap_err();
        assert!(e.contains("online.json is missing"), "{e}");
    }

    #[test]
    fn an_unreachable_server_is_said_plainly() {
        let dir = std::env::temp_dir().join(format!("platform-profile-down-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        // A loopback port nobody listens on.
        let port = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
        std::fs::write(dir.join("online.json"), format!(r#"{{"server":"127.0.0.1:{port}","tls":false}}"#)).unwrap();
        let paths = Paths::for_data_root(&dir.join("roses-data"));
        let paths = Paths { config: dir.join("online.json"), session: dir.join(friends::SESSION_FILE), ..paths };
        let e = run(&paths, Job::Check).unwrap_err();
        assert!(e.starts_with("Can't reach the server (127.0.0.1:"), "{e}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
