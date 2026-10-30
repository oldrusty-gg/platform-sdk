//! Where a game's online server is (the server file, `online.json`), the
//! signed-in session kept between runs (`online-session.json`), and opening
//! a connection of any kind to that server, with or without TLS.

use crate::tls::{self, Trust};
use crate::transport::{Connection, FramedTcp};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use std::io;
use std::net::SocketAddr;
use std::path::Path;
use std::time::Duration;

/// The port used when the server file names none (the Roses hub's).
pub const DEFAULT_PORT: u16 = 7979;

fn yes() -> bool {
    true
}

/// Where the server is (`online.json` next to the game data):
///
/// ```json
/// { "server": "roses.example.net:7979" }
/// { "server": "203.0.113.7:7979", "pin": "sha256:3f5a..." }
/// { "server": "127.0.0.1:7979", "tls": false }
/// ```
/// `pin` trusts exactly that (self-signed) certificate; without it the
/// server needs a certificate for its name from a public authority (Let's
/// Encrypt). `tls: false` is accepted for loopback addresses only.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OnlineConfig {
    pub server: String,
    /// What the menu calls this server.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default = "yes")]
    pub tls: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pin: Option<String>,
}

impl OnlineConfig {
    pub fn load(path: &Path) -> Result<Self, String> {
        let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
        let config: Self = serde_json::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))?;
        config.host_port()?;
        config.trust()?;
        Ok(config)
    }

    /// The host and port (`host`, `host:port`, `[v6]:port`).
    pub fn host_port(&self) -> Result<(String, u16), String> {
        let s = self.server.trim();
        if let Ok(addr) = s.parse::<SocketAddr>() {
            return Ok((addr.ip().to_string(), addr.port()));
        }
        match s.rsplit_once(':') {
            Some((host, port)) if !host.contains(':') => {
                let port = port.parse().map_err(|_| format!("{s}: bad port"))?;
                Ok((host.trim_matches(['[', ']']).to_string(), port))
            }
            _ if !s.is_empty() => Ok((s.trim_matches(['[', ']']).to_string(), DEFAULT_PORT)),
            _ => Err("the server address is empty".into()),
        }
    }

    /// How the server's certificate is checked (`None`: no TLS).
    pub fn trust(&self) -> Result<Option<Trust>, String> {
        if !self.tls {
            if self.pin.is_some() {
                return Err("a pinned certificate needs \"tls\": true".into());
            }
            return Ok(None);
        }
        Ok(Some(match &self.pin {
            Some(pin) => Trust::Pinned(tls::parse_fingerprint(pin)?),
            None => Trust::WebPki,
        }))
    }

    /// The name shown for the server.
    pub fn label(&self) -> String {
        self.name.clone().unwrap_or_else(|| self.server.clone())
    }
}

/// The server file's name.
pub const CONFIG_FILE: &str = "online.json";
/// The signed-in session kept between runs, beside the game data
/// (`roses-data/online-session.json`): the game's online menus and the
/// launcher's Profile page both read and write it.
pub const SESSION_FILE: &str = "online-session.json";

/// Where the server file is looked for, first match wins: the player's
/// own `<data>/online.json`, then the default a package ships beside the
/// executables (`<data>/../online.json`).
pub fn config_candidates(data_root: &Path) -> Vec<std::path::PathBuf> {
    let mut out = vec![data_root.join(CONFIG_FILE)];
    if let Some(parent) = data_root.parent() {
        out.push(parent.join(CONFIG_FILE));
    }
    out
}

/// The first existing server file of [`config_candidates`] (the first
/// candidate when none exists, for the message saying where it goes).
pub fn find_config(data_root: &Path) -> std::path::PathBuf {
    let candidates = config_candidates(data_root);
    candidates.iter().find(|p| p.is_file()).unwrap_or(&candidates[0]).clone()
}

/// A signed-in session kept between runs ([`SESSION_FILE`]). The token is
/// a secret: it signs the account in without the password.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SavedSession {
    /// `OnlineConfig::server` it belongs to (another server ignores it).
    pub server: String,
    pub username: String,
    pub session: String,
}

impl SavedSession {
    /// The session saved at `path` for `server`, if any.
    pub fn load(path: &Path, server: &str) -> Option<Self> {
        let bytes = std::fs::read(path).ok()?;
        if bytes.len() > 64 * 1024 {
            return None;
        }
        let s: Self = serde_json::from_slice(&bytes).ok()?;
        (s.server == server).then_some(s)
    }

    /// Write it (through a temporary file, so a reader never sees half).
    pub fn save(&self, path: &Path) -> io::Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_vec_pretty(self).map_err(io::Error::other)?)?;
        std::fs::rename(&tmp, path)
    }

    pub fn forget(path: &Path) {
        let _ = std::fs::remove_file(path);
    }
}

/// Open a connection of any kind to the configured server.
pub fn connect<Out, In>(config: &OnlineConfig) -> io::Result<Box<dyn Connection<Out, In> + Send>>
where
    Out: Serialize + Send + 'static,
    In: DeserializeOwned + Send + 'static,
{
    let (host, port) = config.host_port().map_err(io::Error::other)?;
    match config.trust().map_err(io::Error::other)? {
        None => {
            let addr: SocketAddr = format!("{host}:{port}")
                .parse()
                .or_else(|_| format!("[{host}]:{port}").parse())
                .map_err(|_| io::Error::other("without TLS the server must be a loopback address (127.0.0.1)"))?;
            Ok(Box::new(FramedTcp::<Out, In>::connect(addr)?))
        }
        Some(trust) => Ok(Box::new(tls::connect::<Out, In>(&host, port, &trust, Duration::from_secs(10))?)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn server_addresses_and_pins_parse() {
        let c = |s: &str| OnlineConfig { server: s.into(), name: None, tls: true, pin: None };
        assert_eq!(c("roses.example.net:7000").host_port().unwrap(), ("roses.example.net".into(), 7000));
        assert_eq!(c("roses.example.net").host_port().unwrap(), ("roses.example.net".into(), DEFAULT_PORT));
        assert_eq!(c("[::1]:7979").host_port().unwrap(), ("::1".into(), 7979));
        assert_eq!(c("100.64.0.7:7979").host_port().unwrap(), ("100.64.0.7".into(), 7979));
        assert!(c("").host_port().is_err());
        assert_eq!(c("x").trust().unwrap(), Some(Trust::WebPki));
        let pin = format!("SHA256:{}", "AB:".repeat(31) + "AB");
        let pinned = OnlineConfig { pin: Some(pin), ..c("x") };
        assert_eq!(pinned.trust().unwrap(), Some(Trust::Pinned([0xAB; 32])));
        assert!(OnlineConfig { pin: Some("sha256:12".into()), ..c("x") }.trust().is_err());
        assert_eq!(OnlineConfig { tls: false, ..c("127.0.0.1:1") }.trust().unwrap(), None);
        let json: OnlineConfig = serde_json::from_str(r#"{"server":"h:1"}"#).unwrap();
        assert!(json.tls, "TLS unless switched off");
    }

    #[test]
    fn the_session_file_is_shared_and_per_server() {
        let dir = std::env::temp_dir().join(format!("platform-session-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join(SESSION_FILE);
        let s = SavedSession { server: "h:7979".into(), username: "Yugi".into(), session: "secret".into() };
        s.save(&path).unwrap();
        assert_eq!(SavedSession::load(&path, "h:7979"), Some(s));
        assert_eq!(SavedSession::load(&path, "other:7979"), None);
        SavedSession::forget(&path);
        assert_eq!(SavedSession::load(&path, "h:7979"), None);
        // The player's own online.json wins over the package default.
        let data = dir.join("roses-data");
        std::fs::create_dir_all(&data).unwrap();
        assert_eq!(find_config(&data), data.join(CONFIG_FILE));
        std::fs::write(dir.join(CONFIG_FILE), "{}").unwrap();
        assert_eq!(find_config(&data), dir.join(CONFIG_FILE));
        std::fs::write(data.join(CONFIG_FILE), "{}").unwrap();
        assert_eq!(find_config(&data), data.join(CONFIG_FILE));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
