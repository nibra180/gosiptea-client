use crate::domain::{self, CallDirection, CallOutcome, HistoryEntry};
use crate::settings::Preferences;
use anyhow::{Context, Result, bail, ensure};
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::path::{Component, Path, PathBuf};
use std::sync::{
    LazyLock,
    atomic::{AtomicU64, Ordering},
};
use std::time::{Duration, Instant};

pub const MAX_FIELD_LENGTH: usize = 255;
pub const MAX_CONTACT_NAME: usize = 64;
pub const MAX_FILE_SIZE: usize = 1 << 20;
pub const MAX_CALL_HISTORY_ENTRIES: usize = 200;
pub const LOCK_TIMEOUT: Duration = Duration::from_secs(5);
const DEFAULT_CONFIG: &str = include_str!("config.tmpl");
const ACCOUNT_HEADER: &str = "# SIP account managed by GoSipTea.\n# Contains the extension password: 600 permission required.\n# TLS + SRTP by default; unencrypted transport requires an explicit choice.\n# GoSipTea rewrites this file when saving the account.\n";
const CONTACTS_HEADER: &str = "#\n# SIP contacts managed by GoSipTea.\n# One contact per line: \"Display name\" <sip:user@host>;addr-params\n# See baresip's modules/contact for the addr-params\n# (;presence=, ;access=allow|block, ;audio=, ;video=).\n#\n\n";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StorageError {
    Duplicate,
    Invalid,
    NotFound,
    NotRegular,
    Symlink,
    TooLarge,
    LockTimeout,
}
impl fmt::Display for StorageError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Duplicate => "storage: duplicate entry",
            Self::Invalid => "storage: invalid input",
            Self::NotFound => "storage: not found",
            Self::NotRegular => "storage: path is not a regular file",
            Self::Symlink => "storage: refusing to write through a symlink",
            Self::TooLarge => "storage: file is too large",
            Self::LockTimeout => "storage: timed out waiting for the file lock",
        })
    }
}
impl std::error::Error for StorageError {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Paths {
    pub dir: PathBuf,
    pub accounts: PathBuf,
    pub contacts: PathBuf,
    pub config: PathBuf,
    pub history: PathBuf,
    pub lock: PathBuf,
}

#[derive(Debug, Clone)]
pub struct Store {
    paths: Paths,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Account {
    pub configured: bool,
    pub username: String,
    pub domain: String,
    pub server: String,
    pub login: String,
    pub has_password: bool,
    pub secure: bool,
    pub transport: String,
}
impl Default for Account {
    fn default() -> Self {
        Self {
            configured: false,
            username: String::new(),
            domain: String::new(),
            server: String::new(),
            login: String::new(),
            has_password: false,
            secure: true,
            transport: String::new(),
        }
    }
}

#[derive(Clone, Default, PartialEq, Eq)]
pub struct Credentials {
    pub server: String,
    pub username: String,
    pub domain: String,
    pub login: String,
    pub password: String,
    pub secure: Option<bool>,
}
impl fmt::Debug for Credentials {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Credentials")
            .field("server", &self.server)
            .field("username", &self.username)
            .field("domain", &self.domain)
            .field("login", &self.login)
            .field("password", &"[REDACTED]")
            .field("secure", &self.secure)
            .finish()
    }
}
pub type AccountCredentials = Credentials;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StoredContact {
    pub name: String,
    pub uri: String,
    pub params: String,
}
pub type Contact = StoredContact;
impl From<&StoredContact> for domain::Contact {
    fn from(value: &StoredContact) -> Self {
        Self {
            name: value.name.clone(),
            uri: value.uri.clone(),
        }
    }
}
impl From<domain::Contact> for StoredContact {
    fn from(value: domain::Contact) -> Self {
        Self {
            name: value.name,
            uri: value.uri,
            params: String::new(),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ContactList {
    pub configured: bool,
    pub contacts: Vec<StoredContact>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AudioConfig {
    pub output: String,
    pub input: String,
    pub alert: String,
}
impl AudioConfig {
    fn device(&self, key: &str) -> &str {
        match key {
            "audio_source" => &self.input,
            "audio_alert" if !self.alert.is_empty() => &self.alert,
            _ => &self.output,
        }
    }
}

impl Store {
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        let dir = clean_path(&dir.into());
        Self {
            paths: Paths {
                accounts: dir.join("accounts"),
                contacts: dir.join("contacts"),
                config: dir.join("config"),
                history: dir.join("gosiptea-call-history.json"),
                lock: dir.join(".gosiptea.lock"),
                dir,
            },
        }
    }
    pub fn new_with_paths(paths: Paths) -> Result<Self> {
        let paths = Paths {
            dir: clean_path(&paths.dir),
            accounts: clean_path(&paths.accounts),
            contacts: clean_path(&paths.contacts),
            config: clean_path(&paths.config),
            history: clean_path(&paths.history),
            lock: clean_path(&paths.lock),
        };
        for path in [
            &paths.accounts,
            &paths.contacts,
            &paths.config,
            &paths.history,
            &paths.lock,
        ] {
            ensure!(
                path != Path::new(".")
                    && path.parent().map(clean_path).as_ref() == Some(&paths.dir),
                StorageError::Invalid
            );
        }
        Ok(Self { paths })
    }
    pub fn default_dir() -> Result<Self> {
        let home = std::env::var_os("HOME")
            .filter(|s| !s.is_empty())
            .context("storage: find home directory")?;
        Ok(Self::new(PathBuf::from(home).join(".baresip")))
    }
    pub fn paths(&self) -> &Paths {
        &self.paths
    }
    pub fn ensure_config(&self) -> Result<()> {
        ensure_config(&self.paths.dir)
    }

    pub fn load_account(&self) -> Result<Account> {
        Ok(
            match read_optional(&self.paths.accounts, false).context("storage: read accounts")? {
                Some(data) => parse_account(&String::from_utf8_lossy(&data)).0,
                None => Account::default(),
            },
        )
    }
    pub fn save_account(&self, input: &Credentials) -> Result<()> {
        self.with_exclusive_lock(|| {
            refuse_symlink_or_special(&self.paths.accounts, true)?;
            let (current, current_password) = match read_optional(&self.paths.accounts, false)? {
                Some(data) => parse_account(&String::from_utf8_lossy(&data)),
                None => (Account::default(), String::new()),
            };
            let server = input.server.trim();
            let username = input.username.trim();
            let domain = if input.domain.trim().is_empty() { server } else { input.domain.trim() };
            let login = if input.login.trim().is_empty() { username } else { input.login.trim() };
            let password = if input.password.is_empty() { &current_password } else { &input.password };
            ensure!(!server.is_empty() && !username.is_empty(), anyhow::Error::new(StorageError::Invalid).context("server and username are required"));
            ensure!(!password.is_empty(), anyhow::Error::new(StorageError::Invalid).context("set a password because none is saved"));
            for (name, value) in [("server", server), ("username", username), ("domain", domain), ("login", login), ("password", password.as_str())] {
                validate_account_field(name, value)?;
            }
            let secure = input.secure.unwrap_or(true);
            let transport = if secure { ";transport=tls" } else if current.transport == "tcp" { ";transport=tcp" } else { "" };
            let media = if secure { ";mediaenc=srtp" } else { "" };
            let line = format!("{ACCOUNT_HEADER}<sip:{username}@{domain}{transport}>;auth_user={login};auth_pass={password};outbound=\"sip:{server}{transport}\"{media};answermode=manual;regint=300;fbregint=30;audio_codecs=opus/48000/2,pcma,pcmu\n");
            atomic_write(&self.paths.accounts, line.as_bytes(), 0o600).context("storage: write accounts")
        })
    }

    pub fn list_contacts(&self) -> Result<ContactList> {
        let Some(data) =
            read_optional(&self.paths.contacts, true).context("storage: read contacts")?
        else {
            return Ok(ContactList::default());
        };
        Ok(ContactList {
            configured: true,
            contacts: split_lines(&String::from_utf8_lossy(&data))
                .iter()
                .filter_map(|line| parse_contact_line(line))
                .collect(),
        })
    }
    pub fn load_contacts(&self) -> Result<Vec<domain::Contact>> {
        Ok(self
            .list_contacts()?
            .contacts
            .iter()
            .map(domain::Contact::from)
            .collect())
    }
    pub fn add_contact(&self, contact: &StoredContact) -> Result<()> {
        let name = contact.name.trim();
        let uri = contact.uri.trim();
        ensure!(contact.params.is_empty(), StorageError::Invalid);
        validate_contact_name(name)?;
        validate_contact_uri(uri)?;
        self.with_exclusive_lock(|| {
            refuse_symlink_or_special(&self.paths.contacts, true)?;
            let (mut lines, mode) = match read_optional(&self.paths.contacts, false)? {
                Some(data) => (
                    split_lines(&String::from_utf8_lossy(&data)),
                    fs::metadata(&self.paths.contacts)?.permissions().mode() & 0o777,
                ),
                None => (split_lines(CONTACTS_HEADER), 0o600),
            };
            ensure!(
                !lines
                    .iter()
                    .filter_map(|line| parse_contact_line(line))
                    .any(|c| c.uri == uri),
                StorageError::Duplicate
            );
            lines.push(if name.is_empty() {
                format!("<{uri}>")
            } else {
                format!("\"{name}\" <{uri}>")
            });
            atomic_write(&self.paths.contacts, join_lines(&lines).as_bytes(), mode)
                .context("storage: write contacts")
        })
    }
    pub fn remove_contact(&self, uri: &str) -> Result<()> {
        let uri = uri.trim();
        validate_removal_uri(uri)?;
        self.with_exclusive_lock(|| {
            let data = read_optional(&self.paths.contacts, false)?.ok_or(StorageError::NotFound)?;
            let mode = fs::metadata(&self.paths.contacts)?.permissions().mode() & 0o777;
            let lines = split_lines(&String::from_utf8_lossy(&data));
            let kept: Vec<_> = lines
                .iter()
                .filter(|line| !parse_contact_line(line).is_some_and(|c| c.uri == uri))
                .cloned()
                .collect();
            ensure!(kept.len() < lines.len(), StorageError::NotFound);
            atomic_write(&self.paths.contacts, join_lines(&kept).as_bytes(), mode)
                .context("storage: write contacts")
        })
    }

    pub fn load_audio(&self) -> Result<AudioConfig> {
        let mut result = AudioConfig::default();
        let Some(data) =
            read_optional(&self.paths.config, false).context("storage: read config")?
        else {
            return Ok(result);
        };
        for line in split_lines(&String::from_utf8_lossy(&data)) {
            let Some(c) = AUDIO_LINE.captures(&line) else {
                continue;
            };
            let device = c[2]
                .split_once(',')
                .map(|(_, name)| name.trim())
                .unwrap_or("")
                .to_owned();
            match &c[1] {
                "audio_player" => result.output = device,
                "audio_alert" => result.alert = device,
                _ => result.input = device,
            }
        }
        if result.alert == result.output {
            result.alert.clear();
        }
        Ok(result)
    }
    pub fn save_audio(&self, config: &AudioConfig) -> Result<()> {
        let config = AudioConfig {
            output: config.output.trim().into(),
            input: config.input.trim().into(),
            alert: config.alert.trim().into(),
        };
        for (name, value) in [
            ("output", &config.output),
            ("input", &config.input),
            ("alert", &config.alert),
        ] {
            validate_device_name(name, value)?;
        }
        self.with_exclusive_lock(|| {
            let data = read_optional(&self.paths.config, false)?.ok_or(StorageError::NotFound)?;
            let mut seen = Vec::new();
            let mut lines = Vec::new();
            for line in split_lines(&String::from_utf8_lossy(&data)) {
                if let Some(c) = AUDIO_LINE.captures(&line) {
                    let key = c[1].to_owned();
                    lines.push(render_audio_line(&key, config.device(&key)));
                    seen.push(key);
                } else {
                    lines.push(line);
                }
            }
            for key in ["audio_player", "audio_source", "audio_alert"] {
                if !seen.iter().any(|s| s == key) {
                    lines.push(render_audio_line(key, config.device(key)));
                }
            }
            atomic_write(&self.paths.config, join_lines(&lines).as_bytes(), 0o644)
                .context("storage: write config")
        })
    }

    pub fn load_preferences(&self) -> Result<Preferences> {
        let path = self.paths.dir.join("gosiptea-settings.json");
        let Some(data) = read_optional(&path, false)
            .with_context(|| format!("storage: read {}", path.display()))?
        else {
            return Ok(Preferences::default());
        };
        ensure!(
            data.iter().find(|byte| !byte.is_ascii_whitespace()) == Some(&b'{'),
            anyhow::Error::new(StorageError::Invalid).context(format!(
                "storage: expected settings object in {}",
                path.display()
            ))
        );
        serde_json::from_slice(&data).map_err(|error| {
            anyhow::Error::new(StorageError::Invalid)
                .context(format!("storage: decode {}: {error}", path.display()))
        })
    }

    pub fn save_preferences(&self, preferences: &Preferences) -> Result<()> {
        let mut data = serde_json::to_vec_pretty(preferences)?;
        data.push(b'\n');
        let path = self.paths.dir.join("gosiptea-settings.json");
        self.with_exclusive_lock(|| {
            atomic_write(&path, &data, 0o600)
                .with_context(|| format!("storage: write {}", path.display()))
        })
    }

    pub fn load_history(&self) -> Result<Vec<HistoryEntry>> {
        let Some(data) =
            read_optional(&self.paths.history, false).context("storage: read call history")?
        else {
            return Ok(vec![]);
        };
        let document: HistoryDocument = serde_json::from_slice(&data).map_err(|e| {
            anyhow::Error::new(StorageError::Invalid).context(format!("decode call history: {e}"))
        })?;
        ensure!(
            document.version == 1,
            anyhow::Error::new(StorageError::Invalid).context("unsupported call history version")
        );
        let calls = document.calls.unwrap_or_default();
        ensure!(
            calls.len() <= MAX_CALL_HISTORY_ENTRIES,
            StorageError::Invalid
        );
        for (i, entry) in calls.iter().enumerate() {
            validate_history_entry(entry)
                .with_context(|| format!("storage: call history entry {i}"))?;
        }
        Ok(calls)
    }
    pub fn save_history(&self, entries: &[HistoryEntry]) -> Result<()> {
        let calls: Vec<_> = entries
            .iter()
            .take(MAX_CALL_HISTORY_ENTRIES)
            .cloned()
            .collect();
        for (i, entry) in calls.iter().enumerate() {
            validate_history_entry(entry)
                .with_context(|| format!("storage: call history entry {i}"))?;
        }
        let mut data = serde_json::to_vec_pretty(&HistoryDocument {
            version: 1,
            calls: if calls.is_empty() { None } else { Some(calls) },
        })?;
        data.push(b'\n');
        self.with_exclusive_lock(|| {
            atomic_write(&self.paths.history, &data, 0o600).context("storage: write call history")
        })
    }

    pub fn read_account(&self) -> Result<Account> {
        self.load_account()
    }
    pub fn write_account(&self, input: &Credentials) -> Result<()> {
        self.save_account(input)
    }
    pub fn read_audio_config(&self) -> Result<AudioConfig> {
        self.load_audio()
    }
    pub fn write_audio_config(&self, config: &AudioConfig) -> Result<()> {
        self.save_audio(config)
    }
    pub fn read_call_history(&self) -> Result<Vec<HistoryEntry>> {
        self.load_history()
    }
    pub fn write_call_history(&self, entries: &[HistoryEntry]) -> Result<()> {
        self.save_history(entries)
    }

    fn with_exclusive_lock<T>(&self, operation: impl FnOnce() -> Result<T>) -> Result<T> {
        ensure_secure_dir(&self.paths.dir)?;
        refuse_symlink_or_special(&self.paths.lock, true)?;
        let file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(&self.paths.lock)
            .map_err(map_open_error)
            .context("storage: open storage lock")?;
        ensure!(file.metadata()?.is_file(), StorageError::NotRegular);
        file.set_permissions(fs::Permissions::from_mode(0o600))?;
        let deadline = Instant::now() + LOCK_TIMEOUT;
        loop {
            // The file owns this descriptor for the entire lock lifetime.
            if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0 {
                break;
            }
            let err = io::Error::last_os_error();
            if err.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            if err.kind() != io::ErrorKind::WouldBlock {
                return Err(err).context("storage: lock storage");
            }
            ensure!(Instant::now() < deadline, StorageError::LockTimeout);
            std::thread::sleep(Duration::from_millis(25));
        }
        let result = operation();
        // Closing the file also releases the lock if operation unwinds.
        let unlocked = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_UN) } == 0;
        if result.is_ok() && !unlocked {
            return Err(io::Error::last_os_error()).context("storage: unlock storage");
        }
        result
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct HistoryDocument {
    version: i64,
    calls: Option<Vec<HistoryEntry>>,
}

pub fn validate_history_entry(entry: &HistoryEntry) -> Result<()> {
    ensure!(
        matches!(
            entry.direction,
            CallDirection::Incoming | CallDirection::Outgoing
        ),
        anyhow::Error::new(StorageError::Invalid).context("unknown direction")
    );
    ensure!(
        entry.outcome != CallOutcome::Unknown,
        anyhow::Error::new(StorageError::Invalid).context("unknown outcome")
    );
    ensure!(
        entry.target.len() <= MAX_FIELD_LENGTH
            && domain::validate_input(&entry.target, MAX_FIELD_LENGTH).is_ok(),
        anyhow::Error::new(StorageError::Invalid).context("invalid target")
    );
    ensure!(
        entry.peer.chars().count() <= MAX_FIELD_LENGTH,
        anyhow::Error::new(StorageError::Invalid).context("invalid peer")
    );
    let (Some(start), Some(end)) = (entry.started_at, entry.ended_at) else {
        bail!(anyhow::Error::new(StorageError::Invalid).context("invalid timestamps"));
    };
    let zero = chrono::DateTime::parse_from_rfc3339("0001-01-01T00:00:00Z")
        .unwrap()
        .with_timezone(&chrono::Utc);
    ensure!(
        start != zero && end != zero && end >= start,
        anyhow::Error::new(StorageError::Invalid).context("invalid timestamps")
    );
    let connected = entry.connected_at.filter(|time| *time != zero);
    ensure!(
        entry.outcome != CallOutcome::Connected || connected.is_some(),
        anyhow::Error::new(StorageError::Invalid)
            .context("connected call has no connected timestamp")
    );
    ensure!(
        connected.is_none_or(|time| time >= start && time <= end),
        anyhow::Error::new(StorageError::Invalid).context("invalid connected timestamp")
    );
    Ok(())
}
pub use validate_history_entry as validate_call_history_entry;

static ACCOUNT_URI: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"<sips?:([^@>]+)@([^;>]+)([^>]*)>").unwrap());
static AUTH_PASS: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"auth_pass=([^;]*)").unwrap());
static AUTH_USER: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"auth_user=([^;]*)").unwrap());
static OUTBOUND: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"outbound="sips?:([^";]+)([^"]*)""#).unwrap());
static TRANSPORT: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)(?:^|;)transport=(udp|tcp|tls)(?:;|$)").unwrap());
static CONTACT_LINE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"^[\t\n\f\r ]*"?([^"<>]*?)"?[\t\n\f\r ]*<([^<>]+)>([^<>]*)[\t\n\f\r ]*$"#).unwrap()
});
static AUDIO_LINE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^[\t\n\f\r ]*(audio_player|audio_alert|audio_source)[\t\n\f\r ]+([^\t\n\f\r ]*)")
        .unwrap()
});

fn parse_account(data: &str) -> (Account, String) {
    let mut account = Account::default();
    for line in split_lines(data) {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some(c) = ACCOUNT_URI.captures(line) else {
            continue;
        };
        account.configured = true;
        account.username = c[1].into();
        account.domain = c[2].into();
        account.transport = TRANSPORT
            .captures(&c[3])
            .map(|t| t[1].to_lowercase())
            .unwrap_or_else(|| "udp".into());
        account.secure = line.contains("transport=tls") && line.contains("mediaenc=srtp");
        let password = AUTH_PASS
            .captures(line)
            .map(|p| p[1].to_owned())
            .unwrap_or_default();
        account.has_password = !password.is_empty();
        account.login = AUTH_USER
            .captures(line)
            .map(|u| u[1].to_owned())
            .unwrap_or_default();
        if let Some(outbound) = OUTBOUND.captures(line) {
            account.server = outbound[1].into();
            if let Some(transport) = TRANSPORT.captures(&outbound[2]) {
                account.transport = transport[1].to_lowercase();
            }
        }
        return (account, password);
    }
    (account, String::new())
}

fn validate_account_field(name: &str, value: &str) -> Result<()> {
    ensure!(
        value.chars().count() <= MAX_FIELD_LENGTH,
        anyhow::Error::new(StorageError::Invalid).context(format!("{name} is too long"))
    );
    ensure!(
        !value
            .chars()
            .any(|c| c <= '\u{1f}' || c.is_whitespace() || matches!(c, ';' | '"' | '<' | '>')),
        anyhow::Error::new(StorageError::Invalid)
            .context(format!("{name} contains an unsupported character"))
    );
    Ok(())
}
fn parse_contact_line(line: &str) -> Option<StoredContact> {
    if line.trim().is_empty() || line.trim_start().starts_with('#') {
        return None;
    }
    let c = CONTACT_LINE.captures(line)?;
    Some(StoredContact {
        name: c[1].trim().into(),
        uri: c[2].trim().into(),
        params: c[3].trim().into(),
    })
}
fn validate_contact_name(name: &str) -> Result<()> {
    ensure!(
        name.chars().count() <= MAX_CONTACT_NAME
            && !name
                .chars()
                .any(|c| c <= '\u{1f}' || matches!(c, '"' | '<' | '>' | ';' | '\\')),
        anyhow::Error::new(StorageError::Invalid).context("invalid contact name")
    );
    Ok(())
}
fn validate_removal_uri(uri: &str) -> Result<()> {
    let lower = uri.to_ascii_lowercase();
    ensure!(
        !uri.is_empty()
            && uri.chars().count() <= MAX_FIELD_LENGTH
            && (lower.starts_with("sip:") || lower.starts_with("sips:"))
            && !uri
                .chars()
                .any(|c| c.is_control() || c.is_whitespace() || matches!(c, '<' | '>' | '"')),
        anyhow::Error::new(StorageError::Invalid).context("invalid contact address")
    );
    Ok(())
}
fn validate_contact_uri(uri: &str) -> Result<()> {
    let invalid =
        || anyhow::Error::new(StorageError::Invalid).context("enter an address like sip:user@host");
    ensure!(
        !uri.is_empty() && uri.chars().count() <= MAX_FIELD_LENGTH,
        invalid()
    );
    let address = uri
        .strip_prefix("sip:")
        .or_else(|| uri.strip_prefix("sips:"))
        .ok_or_else(invalid)?;
    let (user, host) = address.split_once('@').ok_or_else(invalid)?;
    ensure!(
        !user.is_empty()
            && !host.is_empty()
            && !host.contains('@')
            && !address
                .chars()
                .any(|c| c <= '\u{1f}' || c.is_whitespace() || matches!(c, '<' | '>' | ';' | '"')),
        invalid()
    );
    Ok(())
}
fn validate_device_name(name: &str, value: &str) -> Result<()> {
    ensure!(
        value.chars().count() <= MAX_FIELD_LENGTH
            && !value
                .chars()
                .any(|c| c <= '\u{1f}' || c.is_whitespace() || c == ','),
        anyhow::Error::new(StorageError::Invalid).context(format!("invalid {name} device name"))
    );
    Ok(())
}
fn render_audio_line(key: &str, device: &str) -> String {
    format!(
        "{key:<24}pipewire{}",
        if device.is_empty() {
            String::new()
        } else {
            format!(",{device}")
        }
    )
}

fn clean_path(path: &Path) -> PathBuf {
    let mut result = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => (),
            Component::ParentDir if result.file_name().is_some_and(|name| name != "..") => {
                result.pop();
            }
            Component::ParentDir if result.has_root() => (),
            other => result.push(other.as_os_str()),
        }
    }
    if result.as_os_str().is_empty() {
        result.push(".");
    }
    result
}
fn ensure_secure_dir(dir: &Path) -> Result<()> {
    if let Err(error) = fs::symlink_metadata(dir) {
        if error.kind() != io::ErrorKind::NotFound {
            return Err(error).context("storage: inspect baresip directory");
        }
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(dir)
            .context("storage: create baresip directory")?;
    }
    let info = fs::symlink_metadata(dir).context("storage: inspect baresip directory")?;
    ensure!(!info.file_type().is_symlink(), StorageError::Symlink);
    ensure!(info.is_dir(), StorageError::NotRegular);
    fs::set_permissions(dir, fs::Permissions::from_mode(0o700))
        .context("storage: set baresip directory mode")
}
fn refuse_symlink_or_special(path: &Path, allow_missing: bool) -> Result<()> {
    match fs::symlink_metadata(path) {
        Ok(info) => {
            ensure!(!info.file_type().is_symlink(), StorageError::Symlink);
            ensure!(info.is_file(), StorageError::NotRegular);
            Ok(())
        }
        Err(e) if allow_missing && e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e.into()),
    }
}
fn map_open_error(e: io::Error) -> anyhow::Error {
    if e.raw_os_error() == Some(libc::ELOOP) {
        StorageError::Symlink.into()
    } else {
        e.into()
    }
}
fn read_regular_file(path: &Path, follow_symlink: bool) -> Result<Vec<u8>> {
    if !follow_symlink {
        refuse_symlink_or_special(path, false)?;
    }
    let flags =
        libc::O_CLOEXEC | libc::O_NONBLOCK | if follow_symlink { 0 } else { libc::O_NOFOLLOW };
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(flags)
        .open(path)
        .map_err(map_open_error)?;
    ensure!(file.metadata()?.is_file(), StorageError::NotRegular);
    let mut data = Vec::new();
    file.take((MAX_FILE_SIZE + 1) as u64)
        .read_to_end(&mut data)?;
    ensure!(data.len() <= MAX_FILE_SIZE, StorageError::TooLarge);
    Ok(data)
}
fn read_optional(path: &Path, follow_symlink: bool) -> Result<Option<Vec<u8>>> {
    match read_regular_file(path, follow_symlink) {
        Ok(data) => Ok(Some(data)),
        Err(e)
            if e.downcast_ref::<io::Error>()
                .is_some_and(|e| e.kind() == io::ErrorKind::NotFound) =>
        {
            Ok(None)
        }
        Err(e) => Err(e),
    }
}

static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);
struct TempGuard(PathBuf);
impl Drop for TempGuard {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

fn atomic_write(path: &Path, data: &[u8], mode: u32) -> Result<()> {
    let dir = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let name = path
        .file_name()
        .context("storage: missing filename")?
        .to_string_lossy();
    let (temporary, mut file) = loop {
        let temporary = dir.join(format!(
            ".{name}.{}-{}",
            std::process::id(),
            NEXT_TEMP.fetch_add(1, Ordering::Relaxed)
        ));
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&temporary)
        {
            Ok(file) => break (temporary, file),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e).context("storage: create temporary file"),
        }
    };
    let guard = TempGuard(temporary);
    file.set_permissions(fs::Permissions::from_mode(mode))?;
    file.write_all(data)
        .context("storage: write temporary file")?;
    file.sync_all().context("storage: sync temporary file")?;
    drop(file);
    refuse_symlink_or_special(path, true)?;
    fs::rename(&guard.0, path).context("storage: replace file")?;
    File::open(dir)?
        .sync_all()
        .context("storage: sync directory")?;
    Ok(())
}
fn split_lines(data: &str) -> Vec<String> {
    if data.is_empty() {
        return vec![];
    }
    data.split_terminator('\n')
        .map(|line| line.strip_suffix('\r').unwrap_or(line).to_owned())
        .collect()
}
fn join_lines(lines: &[String]) -> String {
    lines.iter().map(|line| format!("{line}\n")).collect()
}

pub fn ensure_config(dir: &Path) -> Result<()> {
    ensure!(
        !dir.as_os_str().is_empty(),
        "bootstrap: empty configuration directory"
    );
    let dir = clean_path(dir);
    ensure_secure_dir(&dir)?;
    let path = dir.join("config");
    match fs::symlink_metadata(&path) {
        Ok(info) => {
            ensure!(
                info.is_file() && !info.file_type().is_symlink(),
                "bootstrap: refusing non-regular config"
            );
            return validate_config(&path);
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => (),
        Err(e) => return Err(e).context("bootstrap: inspect baresip config"),
    }
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&path)
        .context("bootstrap: create baresip config")?;
    let write_result = (|| -> Result<()> {
        file.write_all(DEFAULT_CONFIG.as_bytes())?;
        file.sync_all()?;
        Ok(())
    })();
    drop(file);
    if let Err(e) = write_result {
        let _ = fs::remove_file(&path);
        return Err(e).context("bootstrap: write baresip config");
    }
    File::open(&dir)?.sync_all()?;
    validate_config(&path)
}

pub fn validate_config(path: &Path) -> Result<()> {
    let data = read_regular_file(path, false).context("bootstrap: read baresip config")?;
    let text = String::from_utf8_lossy(&data);
    let mut dbus = false;
    let mut session = false;
    for line in text.split('\n') {
        ensure!(line.len() < 65536, "bootstrap: config line is too long");
        let fields: Vec<_> = line
            .split('#')
            .next()
            .unwrap_or("")
            .split_whitespace()
            .collect();
        if fields.len() < 2 {
            continue;
        }
        match fields[0] {
            "module" | "module_app" => {
                let module = Path::new(fields[1])
                    .file_name()
                    .and_then(|s| s.to_str())
                    .unwrap_or("");
                ensure!(
                    !matches!(module, "ctrl_tcp.so" | "httpd.so" | "cons.so" | "mqtt.so"),
                    "bootstrap: unsafe baresip control module is enabled: {module}"
                );
                if module == "ctrl_dbus.so" {
                    dbus = true;
                }
            }
            "ctrl_dbus_use" => session = fields[1] == "session",
            _ => (),
        }
    }
    ensure!(
        dbus && session,
        "bootstrap: baresip config must enable ctrl_dbus.so on the session bus"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{Duration as TimeDelta, TimeZone, Utc};
    use std::os::unix::fs::symlink;

    fn store() -> (tempfile::TempDir, Store) {
        let temp = tempfile::tempdir().unwrap();
        let store = Store::new(temp.path().join("baresip"));
        (temp, store)
    }
    fn credentials() -> Credentials {
        Credentials {
            server: "pbx.example.com:5061".into(),
            username: "101".into(),
            password: "first-secret".into(),
            ..Default::default()
        }
    }
    fn history_entry() -> HistoryEntry {
        let at = Utc.with_ymd_and_hms(2026, 2, 3, 4, 5, 6).unwrap();
        HistoryEntry {
            direction: CallDirection::Incoming,
            outcome: CallOutcome::Missed,
            peer: "Alice".into(),
            target: "sip:alice@example.com".into(),
            started_at: Some(at),
            connected_at: None,
            ended_at: Some(at),
        }
    }
    fn text(path: &Path) -> String {
        fs::read_to_string(path).unwrap()
    }
    fn mode(path: &Path) -> u32 {
        fs::metadata(path).unwrap().permissions().mode() & 0o777
    }
    fn fixture(store: &Store, path: &Path, data: &str, mode: u32) {
        fs::create_dir_all(&store.paths.dir).unwrap();
        fs::write(path, data).unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(mode)).unwrap();
    }
    fn assert_error<T: fmt::Debug>(result: Result<T>, expected: StorageError) {
        let error = result.unwrap_err();
        assert_eq!(
            error.downcast_ref::<StorageError>(),
            Some(&expected),
            "{error:#}"
        );
    }
    fn no_temporary_files(store: &Store) {
        for entry in fs::read_dir(&store.paths.dir).unwrap() {
            let name = entry.unwrap().file_name().to_string_lossy().into_owned();
            assert!(
                ![
                    ".accounts.",
                    ".contacts.",
                    ".config.",
                    ".gosiptea-call-history.json.",
                    ".gosiptea-settings.json."
                ]
                .iter()
                .any(|prefix| name.starts_with(prefix)),
                "{name}"
            );
        }
    }

    #[test]
    fn missing_files_are_empty_and_new_does_not_touch_disk() {
        let (_temp, store) = store();
        assert!(!store.paths.dir.exists());
        assert_eq!(store.load_account().unwrap(), Account::default());
        assert!(!store.list_contacts().unwrap().configured);
        assert!(store.load_contacts().unwrap().is_empty());
        assert_eq!(store.load_audio().unwrap(), AudioConfig::default());
        assert!(store.load_history().unwrap().is_empty());
        assert!(!store.paths.dir.exists());
    }

    #[test]
    fn preferences_missing_and_partial_documents_use_defaults() {
        use crate::settings::{Language, Theme};
        let (_temp, store) = store();
        assert_eq!(store.load_preferences().unwrap(), Preferences::default());
        assert!(!store.paths.dir.exists());
        let path = store.paths.dir.join("gosiptea-settings.json");
        for (data, expected) in [
            ("{}", Preferences::default()),
            (
                r#"{"language":"german"}"#,
                Preferences {
                    language: Language::German,
                    theme: Theme::Dark,
                },
            ),
            (
                r#"{"theme":"light"}"#,
                Preferences {
                    language: Language::English,
                    theme: Theme::Light,
                },
            ),
        ] {
            fixture(&store, &path, data, 0o600);
            assert_eq!(store.load_preferences().unwrap(), expected);
        }
    }

    #[test]
    fn preferences_reject_invalid_documents() {
        let (_temp, store) = store();
        let path = store.paths.dir.join("gosiptea-settings.json");
        for data in [
            "",
            "{",
            "null",
            "[]",
            r#"{"language":"French"}"#,
            r#"{"theme":"Dark"}"#,
            r#"{"theme":null}"#,
            r#"{"extra":true}"#,
        ] {
            fixture(&store, &path, data, 0o600);
            assert_error(store.load_preferences(), StorageError::Invalid);
            assert_eq!(text(&path), data);
        }
    }

    #[test]
    fn preferences_round_trip_permissions_and_replacement() {
        use crate::settings::{Language, Theme};
        let (_temp, store) = store();
        let path = store.paths.dir.join("gosiptea-settings.json");
        for language in [Language::English, Language::German] {
            for theme in [Theme::Dark, Theme::Light] {
                let preferences = Preferences { language, theme };
                store.save_preferences(&preferences).unwrap();
                assert_eq!(
                    Store::new(&store.paths.dir).load_preferences().unwrap(),
                    preferences
                );
                let json: serde_json::Value = serde_json::from_str(&text(&path)).unwrap();
                assert_eq!(
                    json["language"],
                    if language == Language::English {
                        "english"
                    } else {
                        "german"
                    }
                );
                assert_eq!(
                    json["theme"],
                    if theme == Theme::Dark {
                        "dark"
                    } else {
                        "light"
                    }
                );
                assert_eq!(mode(&path), 0o600);
                assert_eq!(mode(&store.paths.dir), 0o700);
                assert_eq!(mode(&store.paths.lock), 0o600);
                no_temporary_files(&store);
            }
        }
    }

    #[test]
    fn preferences_refuse_symlinks_and_oversized_files() {
        let (temp, store) = store();
        let path = store.paths.dir.join("gosiptea-settings.json");
        let target = temp.path().join("target");
        fs::write(&target, "{}").unwrap();
        fs::create_dir_all(&store.paths.dir).unwrap();
        symlink(&target, &path).unwrap();
        assert_error(store.load_preferences(), StorageError::Symlink);
        assert_error(
            store.save_preferences(&Preferences::default()),
            StorageError::Symlink,
        );
        assert_eq!(text(&target), "{}");
        fs::remove_file(&path).unwrap();
        fixture(&store, &path, &" ".repeat(MAX_FILE_SIZE + 1), 0o600);
        assert_error(store.load_preferences(), StorageError::TooLarge);
        no_temporary_files(&store);
    }

    #[test]
    fn account_round_trip_permissions_defaults_and_password_retention() {
        let (_temp, store) = store();
        let original = credentials();
        store.save_account(&original).unwrap();
        assert_eq!(mode(&store.paths.dir), 0o700);
        assert_eq!(mode(&store.paths.accounts), 0o600);
        assert_eq!(mode(&store.paths.lock), 0o600);
        assert!(text(&store.paths.accounts).starts_with(ACCOUNT_HEADER));
        let account = store.load_account().unwrap();
        assert_eq!(
            account,
            Account {
                configured: true,
                username: "101".into(),
                domain: "pbx.example.com:5061".into(),
                server: "pbx.example.com:5061".into(),
                login: "101".into(),
                has_password: true,
                secure: true,
                transport: "tls".into()
            }
        );
        assert!(!format!("{account:?}").contains(&original.password));
        assert!(!format!("{original:?}").contains(&original.password));
        store
            .save_account(&Credentials {
                server: "proxy.example.com".into(),
                username: "102".into(),
                domain: "voice.example.com".into(),
                login: "login-102".into(),
                ..Default::default()
            })
            .unwrap();
        let data = text(&store.paths.accounts);
        for fragment in [
            "<sip:102@voice.example.com;transport=tls>",
            "auth_user=login-102",
            "auth_pass=first-secret",
            "outbound=\"sip:proxy.example.com;transport=tls\";mediaenc=srtp",
        ] {
            assert!(data.contains(fragment), "{fragment}");
        }
        assert_eq!(data.matches("auth_pass=").count(), 1);
        store
            .save_account(&Credentials {
                secure: Some(false),
                ..original
            })
            .unwrap();
        assert!(!store.load_account().unwrap().secure);
        assert!(!text(&store.paths.accounts).contains("mediaenc=srtp"));
        no_temporary_files(&store);
    }

    #[test]
    fn account_tcp_retention_and_outbound_precedence() {
        for (uri, outbound, expected) in [
            (
                "<sip:101@voice.example.com>",
                "outbound=\"sip:proxy.example.com;transport=tcp\"",
                "tcp",
            ),
            (
                "<sip:101@voice.example.com;transport=tcp>",
                "outbound=\"sip:proxy.example.com\"",
                "tcp",
            ),
            (
                "<sip:101@voice.example.com>",
                "outbound=\"sip:proxy.example.com;transport=tcp;lr\"",
                "tcp",
            ),
            (
                "<sip:101@voice.example.com;transport=tcp>",
                "outbound=\"sip:proxy.example.com;transport=udp\"",
                "udp",
            ),
            (
                "<sip:101@voice.example.com>",
                "outbound=\"sip:proxy.example.com\"",
                "udp",
            ),
        ] {
            let (_temp, store) = store();
            fixture(
                &store,
                &store.paths.accounts,
                &format!("{uri};auth_user=101;auth_pass=saved-secret;{outbound}\n"),
                0o600,
            );
            let account = store.load_account().unwrap();
            assert_eq!(account.transport, expected);
            let mut input = Credentials {
                server: account.server,
                username: account.username,
                domain: account.domain,
                login: account.login,
                secure: Some(false),
                ..Default::default()
            };
            for _ in 0..2 {
                store.save_account(&input).unwrap();
                let account = store.load_account().unwrap();
                assert_eq!(account.transport, expected);
                assert!(!account.secure);
                assert!(text(&store.paths.accounts).contains("auth_pass=saved-secret;"));
            }
            input.secure = Some(true);
            store.save_account(&input).unwrap();
            let account = store.load_account().unwrap();
            assert_eq!(account.transport, "tls");
            assert!(account.secure);
        }
    }

    #[test]
    fn account_parsing_uses_first_valid_account_and_secure_pair() {
        let (_temp, store) = store();
        fixture(
            &store,
            &store.paths.accounts,
            "# <sip:ignored@pbx>\ninvalid\n<sip:101@pbx;transport=tls>;auth_pass=secret\n<sip:102@other>;auth_pass=other\n",
            0o600,
        );
        let account = store.load_account().unwrap();
        assert_eq!(account.username, "101");
        assert!(!account.secure);
        assert_eq!(account.transport, "tls");
        assert!(account.has_password);
        assert!(account.server.is_empty());
        fixture(
            &store,
            &store.paths.accounts,
            "# only comments\ninvalid\n",
            0o600,
        );
        assert_eq!(store.load_account().unwrap(), Account::default());
    }

    #[test]
    fn account_validation_never_replaces_good_file_or_leaks_secret() {
        let (_temp, store) = store();
        assert_error(
            store.save_account(&Credentials {
                password: String::new(),
                ..credentials()
            }),
            StorageError::Invalid,
        );
        store.save_account(&credentials()).unwrap();
        let before = text(&store.paths.accounts);
        for password in [
            "space secret",
            "semi;secret",
            "quote\"secret",
            "line\nbreak",
            "angle<secret",
        ] {
            let error = store
                .save_account(&Credentials {
                    password: password.into(),
                    ..credentials()
                })
                .unwrap_err();
            assert_eq!(
                error.downcast_ref::<StorageError>(),
                Some(&StorageError::Invalid)
            );
            assert!(!format!("{error:#}").contains(password));
            assert_eq!(text(&store.paths.accounts), before);
        }
        for input in [
            Credentials {
                server: "a".repeat(256),
                ..credentials()
            },
            Credentials {
                username: String::new(),
                ..credentials()
            },
            Credentials {
                domain: "bad host".into(),
                ..credentials()
            },
            Credentials {
                login: "bad;login".into(),
                ..credentials()
            },
        ] {
            assert_error(store.save_account(&input), StorageError::Invalid);
        }
        assert_eq!(text(&store.paths.accounts), before);
    }

    #[test]
    fn contacts_preserve_comments_unknown_lines_params_modes_and_duplicates() {
        let (_temp, store) = store();
        let original = "# personal contacts\nunparsed text stays here\n\"Alice\" <sip:alice@example.com>;presence=yes\n\"Alice duplicate\" <sip:alice@example.com>;access=allow\n<sips:bob@example.com>\n";
        fixture(&store, &store.paths.contacts, original, 0o640);
        let list = store.list_contacts().unwrap();
        assert!(list.configured);
        assert_eq!(list.contacts.len(), 3);
        assert_eq!(list.contacts[0].params, ";presence=yes");
        assert_error(
            store.add_contact(&StoredContact {
                name: "Another Alice".into(),
                uri: "sip:alice@example.com".into(),
                ..Default::default()
            }),
            StorageError::Duplicate,
        );
        assert_eq!(text(&store.paths.contacts), original);
        store
            .add_contact(&StoredContact {
                name: "Carol".into(),
                uri: "sip:carol@example.com".into(),
                ..Default::default()
            })
            .unwrap();
        let after = text(&store.paths.contacts);
        assert!(after.starts_with(original));
        assert!(after.ends_with("\"Carol\" <sip:carol@example.com>\n"));
        assert_eq!(mode(&store.paths.contacts), 0o640);
        assert_eq!(mode(&store.paths.dir), 0o700);
        store.remove_contact("sip:alice@example.com").unwrap();
        let after = text(&store.paths.contacts);
        assert!(!after.contains("sip:alice@example.com"));
        for preserved in [
            "# personal contacts",
            "unparsed text stays here",
            "sips:bob@example.com",
            "sip:carol@example.com",
        ] {
            assert!(after.contains(preserved));
        }
        assert_error(
            store.remove_contact("sip:missing@pbx"),
            StorageError::NotFound,
        );
        assert_eq!(mode(&store.paths.contacts), 0o640);
        no_temporary_files(&store);
    }

    #[test]
    fn contacts_new_header_nameless_entries_crlf_and_parameterized_removal() {
        let (_temp, store) = store();
        store
            .add_contact(&StoredContact {
                uri: "sip:201@pbx".into(),
                ..Default::default()
            })
            .unwrap();
        assert!(text(&store.paths.contacts).starts_with(CONTACTS_HEADER));
        assert!(text(&store.paths.contacts).ends_with("<sip:201@pbx>\n"));
        assert_eq!(mode(&store.paths.contacts), 0o600);
        fixture(
            &store,
            &store.paths.contacts,
            "# keep\r\n<sip:alice@example.com;transport=tls>\r\n<SIP:other@pbx>\r",
            0o600,
        );
        store
            .remove_contact("sip:alice@example.com;transport=tls")
            .unwrap();
        store.remove_contact("SIP:other@pbx").unwrap();
        assert_eq!(text(&store.paths.contacts), "# keep\n");
        assert_eq!(split_lines("a\r\nb\n\n"), ["a", "b", ""]);
    }

    #[test]
    fn contacts_validate_names_addresses_and_params() {
        let (_temp, store) = store();
        for uri in [
            "",
            "alice@example.com",
            "sip:@example.com",
            "sip:alice@",
            "sip:a@b@c",
            "sip:alice@example.com;transport=tls",
            "sip:ali\"ce@example.com",
            "SIP:alice@pbx",
            "sip:alice@bad host",
        ] {
            assert_error(
                store.add_contact(&StoredContact {
                    name: "Alice".into(),
                    uri: uri.into(),
                    ..Default::default()
                }),
                StorageError::Invalid,
            );
        }
        for name in ["a;b", "a\\b", "a<b", "a\"b", "a\nb", &"😀".repeat(65)] {
            assert_error(
                store.add_contact(&StoredContact {
                    name: name.into(),
                    uri: "sip:alice@pbx".into(),
                    ..Default::default()
                }),
                StorageError::Invalid,
            );
        }
        assert_error(
            store.add_contact(&StoredContact {
                uri: "sip:alice@pbx".into(),
                params: ";presence=yes".into(),
                ..Default::default()
            }),
            StorageError::Invalid,
        );
        assert_error(
            store.remove_contact("sip:missing@pbx"),
            StorageError::NotFound,
        );
        for uri in [
            "",
            "alice@pbx",
            "sip:a\nb@pbx",
            "sip:a b@pbx",
            "<sip:a@pbx>",
        ] {
            assert_error(store.remove_contact(uri), StorageError::Invalid);
        }
    }

    #[test]
    fn concurrent_contact_adds_share_go_compatible_file_lock() {
        let (_temp, store) = store();
        std::thread::scope(|scope| {
            let handles: Vec<_> = (0..32)
                .map(|i| {
                    let store = &store;
                    scope.spawn(move || {
                        store.add_contact(&StoredContact {
                            name: format!("Contact {i}"),
                            uri: format!("sip:{i}@pbx"),
                            ..Default::default()
                        })
                    })
                })
                .collect();
            for handle in handles {
                handle.join().unwrap().unwrap();
            }
        });
        let contacts = store.load_contacts().unwrap();
        assert_eq!(contacts.len(), 32);
        for i in 0..32 {
            assert!(contacts.iter().any(|c| c.uri == format!("sip:{i}@pbx")));
        }
        assert_eq!(mode(&store.paths.contacts), 0o600);
        no_temporary_files(&store);
    }

    #[test]
    fn audio_updates_all_managed_lines_and_preserves_unrelated_config() {
        let (_temp, store) = store();
        let original = "# audio comment\npoll_method             epoll\naudio_player            pipewire,old-output\nmodule                  opus.so\naudio_player pipewire,second-output\naudio_source            pipewire,old-input\n";
        fixture(&store, &store.paths.config, original, 0o600);
        assert_eq!(
            store.load_audio().unwrap(),
            AudioConfig {
                output: "second-output".into(),
                input: "old-input".into(),
                alert: String::new()
            }
        );
        let config = AudioConfig {
            output: "sink.node".into(),
            input: "source.node".into(),
            alert: String::new(),
        };
        store.save_audio(&config).unwrap();
        let after = text(&store.paths.config);
        for kept in [
            "# audio comment",
            "poll_method             epoll",
            "module                  opus.so",
        ] {
            assert!(after.contains(kept));
        }
        assert_eq!(
            after
                .matches("audio_player            pipewire,sink.node")
                .count(),
            2
        );
        assert!(after.contains("audio_alert             pipewire,sink.node"));
        assert!(after.contains("audio_source            pipewire,source.node"));
        assert_eq!(store.load_audio().unwrap(), config);
        assert_eq!(mode(&store.paths.config), 0o644);
        no_temporary_files(&store);
    }

    #[test]
    fn audio_ringtone_can_follow_output_or_use_separate_device() {
        let (_temp, store) = store();
        fixture(
            &store,
            &store.paths.config,
            "audio_player pipewire,old\naudio_alert pipewire,old\naudio_source pipewire,mic\n",
            0o600,
        );
        assert!(store.load_audio().unwrap().alert.is_empty());
        let config = AudioConfig {
            output: "headset".into(),
            input: "mic".into(),
            alert: "hdmi".into(),
        };
        store.save_audio(&config).unwrap();
        assert_eq!(store.load_audio().unwrap(), config);
        let config = AudioConfig {
            alert: String::new(),
            ..config
        };
        store.save_audio(&config).unwrap();
        assert_eq!(store.load_audio().unwrap(), config);
        assert!(text(&store.paths.config).contains("audio_alert             pipewire,headset"));
        store.save_audio(&AudioConfig::default()).unwrap();
        assert_eq!(store.load_audio().unwrap(), AudioConfig::default());
        assert!(!text(&store.paths.config).contains(','));
        fixture(
            &store,
            &store.paths.config,
            "audio_player pipewire,headset\n",
            0o600,
        );
        assert!(store.load_audio().unwrap().alert.is_empty());
    }

    #[test]
    fn audio_validation_and_missing_config() {
        let (_temp, store) = store();
        assert_error(
            store.save_audio(&AudioConfig::default()),
            StorageError::NotFound,
        );
        for device in [
            "node with space",
            "node,with-comma",
            "line\nbreak",
            &"x".repeat(256),
        ] {
            for config in [
                AudioConfig {
                    output: device.into(),
                    ..Default::default()
                },
                AudioConfig {
                    input: device.into(),
                    ..Default::default()
                },
                AudioConfig {
                    alert: device.into(),
                    ..Default::default()
                },
            ] {
                assert_error(store.save_audio(&config), StorageError::Invalid);
            }
        }
    }

    #[test]
    fn history_round_trip_go_zero_times_unicode_and_limit() {
        let (_temp, store) = store();
        let entries: Vec<_> = (0..205)
            .map(|i| HistoryEntry {
                peer: format!("Peer {i}"),
                ..history_entry()
            })
            .collect();
        store.save_history(&entries).unwrap();
        assert_eq!(
            store.load_history().unwrap(),
            entries[..MAX_CALL_HISTORY_ENTRIES]
        );
        assert_eq!(mode(&store.paths.history), 0o600);
        assert!(text(&store.paths.history).contains("\"connected_at\": \"0001-01-01T00:00:00Z\""));
        for peer in [
            "😀".repeat(MAX_CONTACT_NAME),
            "a".repeat(MAX_FIELD_LENGTH),
            "legacy\nlabel".into(),
        ] {
            let entry = HistoryEntry {
                peer,
                target: format!(
                    "sip:{}@example.com",
                    "a".repeat(MAX_FIELD_LENGTH - "sip:@example.com".len())
                ),
                ..history_entry()
            };
            store.save_history(std::slice::from_ref(&entry)).unwrap();
            assert_eq!(store.load_history().unwrap(), [entry]);
        }
        store.save_history(&[]).unwrap();
        assert!(text(&store.paths.history).contains("\"calls\": null"));
        assert!(store.load_history().unwrap().is_empty());
        no_temporary_files(&store);
    }

    #[test]
    fn invalid_history_never_replaces_valid_data() {
        let (_temp, store) = store();
        let good = history_entry();
        store.save_history(std::slice::from_ref(&good)).unwrap();
        let before = text(&store.paths.history);
        let mut invalid = vec![
            HistoryEntry {
                target: "a".repeat(256),
                ..good.clone()
            },
            HistoryEntry {
                target: format!("sip:{}@example.com", "😀".repeat(64)),
                ..good.clone()
            },
            HistoryEntry {
                target: "sip:a\n@example.com".into(),
                ..good.clone()
            },
            HistoryEntry {
                peer: "😀".repeat(256),
                ..good.clone()
            },
            HistoryEntry {
                direction: CallDirection::Unknown,
                ..good.clone()
            },
            HistoryEntry {
                outcome: CallOutcome::Unknown,
                ..good.clone()
            },
            HistoryEntry {
                started_at: None,
                ..good.clone()
            },
            HistoryEntry {
                ended_at: None,
                ..good.clone()
            },
            HistoryEntry {
                ended_at: good.started_at.map(|t| t - TimeDelta::seconds(1)),
                ..good.clone()
            },
            HistoryEntry {
                outcome: CallOutcome::Connected,
                ..good.clone()
            },
            HistoryEntry {
                connected_at: good.started_at.map(|t| t - TimeDelta::seconds(1)),
                ..good.clone()
            },
            HistoryEntry {
                connected_at: good.ended_at.map(|t| t + TimeDelta::seconds(1)),
                ..good.clone()
            },
        ];
        let zero = Utc.with_ymd_and_hms(1, 1, 1, 0, 0, 0).unwrap();
        invalid.push(HistoryEntry {
            started_at: Some(zero),
            ..good.clone()
        });
        for entry in invalid {
            assert_error(store.save_history(&[entry]), StorageError::Invalid);
            assert_eq!(text(&store.paths.history), before);
        }
        let connected = HistoryEntry {
            outcome: CallOutcome::Connected,
            connected_at: good.started_at,
            ..good
        };
        store
            .save_history(std::slice::from_ref(&connected))
            .unwrap();
        assert_eq!(store.load_history().unwrap(), [connected]);
    }

    #[test]
    fn history_rejects_malformed_unknown_and_trailing_json() {
        let (_temp, store) = store();
        for json in [
            "not json",
            "",
            "{}",
            "null",
            r#"{"version":2,"calls":[]}"#,
            r#"{"version":1,"calls":[],"unexpected":true}"#,
            r#"{"version":1,"calls":[]} {}"#,
            r#"{"version":1,"calls":[{"unexpected":true}]}"#,
            r#"{"version":1,"calls":[{}]}"#,
        ] {
            fixture(&store, &store.paths.history, json, 0o600);
            assert_error(store.load_history(), StorageError::Invalid);
            assert_eq!(text(&store.paths.history), json);
        }
        let entries = vec![history_entry(); MAX_CALL_HISTORY_ENTRIES + 1];
        let json = serde_json::to_string(&HistoryDocument {
            version: 1,
            calls: Some(entries),
        })
        .unwrap();
        fixture(&store, &store.paths.history, &json, 0o600);
        assert_error(store.load_history(), StorageError::Invalid);
        for json in [
            r#"{"version":1}"#,
            r#"{"version":1,"calls":null}"#,
            r#"{"version":1,"calls":[]}"#,
        ] {
            fixture(&store, &store.paths.history, json, 0o600);
            assert!(store.load_history().unwrap().is_empty());
        }
    }

    #[test]
    fn symlinks_are_read_only_for_contacts_and_refused_elsewhere() {
        let (temp, store) = store();
        fs::create_dir(&store.paths.dir).unwrap();
        let outside = temp.path().join("outside");
        fs::write(&outside, "<sip:existing@example.com>\n").unwrap();
        for path in [
            &store.paths.contacts,
            &store.paths.accounts,
            &store.paths.config,
            &store.paths.history,
        ] {
            symlink(&outside, path).unwrap();
        }
        assert_eq!(
            store.load_contacts().unwrap()[0].uri,
            "sip:existing@example.com"
        );
        assert_error(
            store.add_contact(&StoredContact {
                uri: "sip:new@example.com".into(),
                ..Default::default()
            }),
            StorageError::Symlink,
        );
        assert_error(
            store.remove_contact("sip:existing@example.com"),
            StorageError::Symlink,
        );
        assert_error(store.load_account(), StorageError::Symlink);
        assert_error(store.save_account(&credentials()), StorageError::Symlink);
        assert_error(store.load_audio(), StorageError::Symlink);
        assert_error(
            store.save_audio(&AudioConfig::default()),
            StorageError::Symlink,
        );
        assert_error(store.load_history(), StorageError::Symlink);
        assert_error(
            store.save_history(&[history_entry()]),
            StorageError::Symlink,
        );
        assert!(store.ensure_config().is_err());
        assert_eq!(text(&outside), "<sip:existing@example.com>\n");
        symlink(&outside, &store.paths.lock).unwrap_err();
        fs::remove_file(&store.paths.lock).unwrap();
        symlink(&outside, &store.paths.lock).unwrap();
        assert_error(store.save_history(&[]), StorageError::Symlink);
        no_temporary_files(&store);
    }

    #[test]
    fn directory_symlink_and_special_files_are_refused() {
        let (temp, store) = store();
        let real = temp.path().join("real");
        fs::create_dir(&real).unwrap();
        symlink(&real, &store.paths.dir).unwrap();
        assert_error(store.save_account(&credentials()), StorageError::Symlink);
        assert!(store.ensure_config().is_err());
        fs::remove_file(&store.paths.dir).unwrap();
        fs::write(&store.paths.dir, "not directory").unwrap();
        assert_error(store.save_account(&credentials()), StorageError::NotRegular);
        fs::remove_file(&store.paths.dir).unwrap();
        fs::create_dir(&store.paths.dir).unwrap();
        fs::create_dir(&store.paths.accounts).unwrap();
        assert_error(store.load_account(), StorageError::NotRegular);
        assert_error(store.save_account(&credentials()), StorageError::NotRegular);
        fs::create_dir(&store.paths.lock).unwrap_err();
        fs::remove_file(&store.paths.lock).unwrap();
        fs::create_dir(&store.paths.lock).unwrap();
        assert_error(store.save_history(&[]), StorageError::NotRegular);
    }

    #[test]
    fn contacts_symlink_to_fifo_never_blocks() {
        let (temp, store) = store();
        fs::create_dir(&store.paths.dir).unwrap();
        let fifo = temp.path().join("fifo");
        let cpath = std::ffi::CString::new(fifo.as_os_str().as_encoded_bytes()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(cpath.as_ptr(), 0o600) }, 0);
        symlink(&fifo, &store.paths.contacts).unwrap();
        let (send, recv) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            send.send(store.list_contacts()).unwrap();
        });
        assert_error(
            recv.recv_timeout(Duration::from_secs(1))
                .expect("FIFO read blocked"),
            StorageError::NotRegular,
        );
    }

    #[test]
    fn bounded_reads_and_atomic_cleanup_on_refusal() {
        let (_temp, store) = store();
        fixture(
            &store,
            &store.paths.accounts,
            &"x".repeat(MAX_FILE_SIZE + 1),
            0o600,
        );
        assert_error(store.load_account(), StorageError::TooLarge);
        fs::remove_file(&store.paths.accounts).unwrap();
        fs::create_dir(&store.paths.accounts).unwrap();
        assert_error(
            atomic_write(&store.paths.accounts, b"data", 0o600),
            StorageError::NotRegular,
        );
        no_temporary_files(&store);
    }

    #[test]
    fn bootstrap_preserves_existing_config_and_creates_secure_defaults() {
        let (_temp, store) = store();
        store.ensure_config().unwrap();
        assert_eq!(mode(&store.paths.dir), 0o700);
        assert_eq!(mode(&store.paths.config), 0o600);
        assert_eq!(text(&store.paths.config), DEFAULT_CONFIG);
        let custom = "# custom\nmodule_app ctrl_dbus.so\nctrl_dbus_use session\n";
        fixture(&store, &store.paths.config, custom, 0o640);
        store.ensure_config().unwrap();
        assert_eq!(text(&store.paths.config), custom);
        assert_eq!(mode(&store.paths.config), 0o640);
        assert!(ensure_config(Path::new("")).is_err());
    }

    #[test]
    fn bootstrap_rejects_all_unsafe_controls_and_requires_session_dbus() {
        let (_temp, store) = store();
        for module in ["ctrl_tcp.so", "httpd.so", "cons.so", "mqtt.so"] {
            let config = format!(
                "module_app ctrl_dbus.so\nctrl_dbus_use session\nmodule /usr/lib/baresip/modules/{module}\n"
            );
            fixture(&store, &store.paths.config, &config, 0o600);
            assert!(format!("{:#}", store.ensure_config().unwrap_err()).contains(module));
            assert_eq!(text(&store.paths.config), config);
        }
        for config in [
            "module_app ctrl_dbus.so\n",
            "ctrl_dbus_use session\n",
            "module_app ctrl_dbus.so\nctrl_dbus_use session\nctrl_dbus_use system\n",
        ] {
            fixture(&store, &store.paths.config, config, 0o600);
            assert!(validate_config(&store.paths.config).is_err());
        }
        fixture(
            &store,
            &store.paths.config,
            "# module ctrl_tcp.so\nmodule_app /usr/lib/baresip/modules/ctrl_dbus.so # enabled\nctrl_dbus_use session\n",
            0o600,
        );
        validate_config(&store.paths.config).unwrap();
    }

    #[test]
    fn explicit_paths_must_be_direct_children() {
        let (_temp, store) = store();
        Store::new_with_paths(store.paths.clone()).unwrap();
        let mut paths = store.paths.clone();
        paths.lock = paths.dir.parent().unwrap().join("lock");
        assert_error(Store::new_with_paths(paths), StorageError::Invalid);
        let paths = Paths {
            dir: "".into(),
            accounts: "".into(),
            contacts: "".into(),
            config: "".into(),
            history: "".into(),
            lock: "".into(),
        };
        assert_error(Store::new_with_paths(paths), StorageError::Invalid);
        assert_eq!(clean_path(Path::new("/a/../b//./")), Path::new("/b"));
        assert_eq!(clean_path(Path::new("../../a/../b")), Path::new("../../b"));
        assert_eq!(clean_path(Path::new("/../../a")), Path::new("/a"));
    }

    #[test]
    fn lock_timeout_does_not_write_or_remove_shared_lock() {
        let (_temp, store) = store();
        ensure_secure_dir(&store.paths.dir).unwrap();
        let lock = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&store.paths.lock)
            .unwrap();
        assert_eq!(unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX) }, 0);
        assert_error(store.save_history(&[]), StorageError::LockTimeout);
        assert!(!store.paths.history.exists());
        assert!(store.paths.lock.exists());
        drop(lock);
        store.save_history(&[]).unwrap();
    }
}
