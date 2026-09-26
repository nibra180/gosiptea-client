use chrono::{DateTime, Utc};
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::fmt;
use std::sync::LazyLock;

pub const MAX_DIAL_TARGET_LENGTH: usize = 255;
pub const MAX_PEER_INPUT_LENGTH: usize = 255;
pub const MAX_PEER_DISPLAY_LENGTH: usize = 64;
pub const MAX_REGISTRATION_AOR: usize = 96;
pub const MAX_COMMAND_ERROR: usize = 160;
pub const MAX_BRIDGE_LINE_LENGTH: usize = 65536;
pub const DEFAULT_COUNTRY_CODE: &str = "49";

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Contact {
    pub name: String,
    pub uri: String,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CallState {
    #[default]
    Idle,
    Incoming,
    Outgoing,
    Active,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CallDirection {
    #[default]
    #[serde(rename = "")]
    Unknown,
    Incoming,
    Outgoing,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CallOutcome {
    #[default]
    #[serde(rename = "")]
    Unknown,
    Connected,
    Missed,
    Rejected,
    RejectedDnd,
    RejectedBusy,
    NotConnected,
    Canceled,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RegistrationState {
    #[default]
    Unknown,
    Registering,
    Registered,
    Failed,
    Unregistered,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct State {
    pub call_state: CallState,
    pub call_id: String,
    pub call_direction: CallDirection,
    pub call_target: String,
    pub peer: String,
    pub muted: bool,
    pub dnd: bool,
    pub end_requested: bool,
    pub registered: bool,
    pub registration: RegistrationState,
    pub registration_detail: String,
    pub call_created_at: Option<DateTime<Utc>>,
    pub call_started_at: Option<DateTime<Utc>>,
    pub call_ended_at: Option<DateTime<Utc>>,
    pub call_state_changed_at: Option<DateTime<Utc>>,
    pub last_call_id: String,
    pub ignored_call_ids: [String; 8],
    pub ignored_call_index: u8,
}

impl State {
    pub fn new() -> Self {
        Self::default()
    }
}

// Go encodes an absent time.Time as this timestamp, not JSON null.
mod go_time {
    use super::*;
    const ZERO: &str = "0001-01-01T00:00:00Z";
    pub fn serialize<S: serde::Serializer>(
        value: &Option<DateTime<Utc>>,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        match value {
            Some(time) => time.serialize(serializer),
            None => serializer.serialize_str(ZERO),
        }
    }
    pub fn deserialize<'de, D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Option<DateTime<Utc>>, D::Error> {
        let value = Option::<DateTime<Utc>>::deserialize(deserializer)?;
        Ok(value.filter(|time| time.to_rfc3339_opts(chrono::SecondsFormat::AutoSi, true) != ZERO))
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct HistoryEntry {
    pub direction: CallDirection,
    pub outcome: CallOutcome,
    pub peer: String,
    pub target: String,
    #[serde(with = "go_time")]
    pub started_at: Option<DateTime<Utc>>,
    #[serde(with = "go_time")]
    pub connected_at: Option<DateTime<Utc>>,
    #[serde(with = "go_time")]
    pub ended_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    Dial(String),
    Answer,
    Hangup,
    ToggleMute,
    ToggleDnd,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CallEventType {
    Incoming,
    Established,
    Closed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RegisterEventType {
    Ok,
    Fail,
    Unregistering,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    Action(Action),
    Call {
        kind: CallEventType,
        id: String,
        peer_uri: String,
        peer_display_name: String,
    },
    Register {
        kind: RegisterEventType,
        account_aor: String,
        detail: String,
    },
    RegistrationSnapshot(RegistrationInfo),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandKind {
    Dial,
    Accept,
    Hangup,
    Reject,
    Mute,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Command {
    pub kind: CommandKind,
    pub parameter: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NotificationKind {
    Incoming,
    Missed,
    RejectedDnd,
    RejectedBusy,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Notification {
    pub kind: NotificationKind,
    pub body: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DomainError {
    EmptyTarget,
    CallInProgress,
    NotRegistered,
    InputTooLong,
    ControlCharacter,
}
impl fmt::Display for DomainError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::EmptyTarget => "empty target",
            Self::CallInProgress => "a call is already in progress",
            Self::NotRegistered => "not registered to the server",
            Self::InputTooLong => "input exceeds length limit",
            Self::ControlCharacter => "input contains a control character",
        })
    }
}
impl std::error::Error for DomainError {}

#[derive(Debug, Clone)]
pub struct Transition {
    pub state: State,
    pub commands: Vec<Command>,
    pub notifications: Vec<Notification>,
    pub history: Vec<HistoryEntry>,
    pub error: Option<DomainError>,
}

pub fn reduce(
    state: &State,
    event: Event,
    contacts: &[Contact],
    country_code: &str,
    now: DateTime<Utc>,
) -> Transition {
    let mut result = Transition {
        state: state.clone(),
        commands: vec![],
        notifications: vec![],
        history: vec![],
        error: None,
    };
    match event {
        Event::Action(action) => reduce_action(&mut result, action, contacts, country_code, now),
        Event::Call {
            kind,
            id,
            peer_uri,
            peer_display_name,
        } => {
            if !id.is_empty() && result.state.ignored_call_ids.contains(&id) {
                return result;
            }
            match kind {
                CallEventType::Incoming => reduce_incoming(
                    &mut result,
                    &id,
                    &peer_uri,
                    &peer_display_name,
                    contacts,
                    country_code,
                    now,
                ),
                CallEventType::Established => {
                    let s = &mut result.state;
                    if matches!(s.call_state, CallState::Incoming | CallState::Outgoing)
                        && event_matches_current_call(s, &id)
                    {
                        if s.call_id.is_empty() {
                            s.call_id = id;
                        }
                        if s.peer.is_empty() && !peer_uri.is_empty() {
                            s.peer = peer_display(&peer_uri);
                        }
                        s.call_state = CallState::Active;
                        s.muted = false;
                        s.call_started_at = Some(now);
                        s.call_state_changed_at = Some(now);
                    }
                }
                CallEventType::Closed => reduce_closed(&mut result, &id, now),
            }
        }
        Event::Register {
            kind,
            account_aor,
            detail,
        } => {
            let s = &mut result.state;
            match kind {
                RegisterEventType::Ok => {
                    s.registered = true;
                    s.registration = RegistrationState::Registered;
                    let aor = strip_sip_scheme(account_aor.trim());
                    s.registration_detail = clamp_text(
                        if aor.is_empty() { "registered" } else { aor },
                        MAX_REGISTRATION_AOR,
                    );
                }
                RegisterEventType::Fail => {
                    s.registered = false;
                    s.registration = RegistrationState::Failed;
                    s.registration_detail = if detail.trim().is_empty() {
                        "registration failed".into()
                    } else {
                        clamp_text(
                            &format!("registration failed: {}", detail.trim()),
                            MAX_COMMAND_ERROR,
                        )
                    };
                }
                RegisterEventType::Unregistering => set_unregistered(s),
            }
        }
        Event::RegistrationSnapshot(info) => {
            if info.known {
                let s = &mut result.state;
                if info.count == 0 {
                    set_unregistered(s);
                } else if info.registered {
                    s.registered = true;
                    s.registration = RegistrationState::Registered;
                    s.registration_detail = clamp_text(&info.aor, MAX_REGISTRATION_AOR);
                    if s.registration_detail.is_empty() {
                        s.registration_detail = "registered".into();
                    }
                } else {
                    s.registered = false;
                    s.registration = if info.failed {
                        RegistrationState::Failed
                    } else {
                        RegistrationState::Registering
                    };
                    s.registration_detail = if info.failed {
                        "registration failed"
                    } else {
                        "registering"
                    }
                    .into();
                }
            }
        }
    }
    result
}

fn set_unregistered(s: &mut State) {
    s.registered = false;
    s.registration = RegistrationState::Unregistered;
    s.registration_detail = "unregistered".into();
}

fn reduce_action(
    result: &mut Transition,
    action: Action,
    contacts: &[Contact],
    country_code: &str,
    now: DateTime<Utc>,
) {
    let s = &mut result.state;
    let command = match action {
        Action::Dial(raw) => {
            let target = normalize_dial_target(&raw);
            result.error = if target.is_empty() {
                Some(DomainError::EmptyTarget)
            } else if s.call_state != CallState::Idle {
                Some(DomainError::CallInProgress)
            } else if !s.registered {
                Some(DomainError::NotRegistered)
            } else {
                None
            };
            if result.error.is_some() {
                return;
            }
            let peer = contact_name(contacts, &target, country_code)
                .unwrap_or_else(|| peer_display(&target));
            begin_call(
                s,
                CallState::Outgoing,
                CallDirection::Outgoing,
                "",
                &peer,
                &target,
                now,
            );
            Some(Command {
                kind: CommandKind::Dial,
                parameter: target,
            })
        }
        Action::Answer if s.call_state == CallState::Incoming => Some(Command {
            kind: CommandKind::Accept,
            parameter: String::new(),
        }),
        Action::Hangup if s.call_state != CallState::Idle => {
            s.end_requested = true;
            Some(Command {
                kind: if s.call_state == CallState::Incoming {
                    CommandKind::Reject
                } else {
                    CommandKind::Hangup
                },
                parameter: String::new(),
            })
        }
        Action::ToggleMute if s.call_state == CallState::Active => {
            s.muted = !s.muted;
            Some(Command {
                kind: CommandKind::Mute,
                parameter: String::new(),
            })
        }
        Action::ToggleDnd => {
            s.dnd = !s.dnd;
            None
        }
        _ => None,
    };
    if let Some(command) = command {
        result.commands.push(command);
    }
}

fn begin_call(
    s: &mut State,
    state: CallState,
    direction: CallDirection,
    id: &str,
    peer: &str,
    target: &str,
    now: DateTime<Utc>,
) {
    s.call_state = state;
    s.call_direction = direction;
    s.call_id = id.into();
    s.peer = peer.into();
    s.call_target = target.into();
    s.muted = false;
    s.end_requested = false;
    s.call_created_at = Some(now);
    s.call_started_at = None;
    s.call_ended_at = None;
    s.call_state_changed_at = Some(now);
}

fn reduce_incoming(
    result: &mut Transition,
    id: &str,
    uri: &str,
    display: &str,
    contacts: &[Contact],
    country_code: &str,
    now: DateTime<Utc>,
) {
    let who = caller_name(contacts, uri, display, country_code);
    let s = &mut result.state;
    if !id.is_empty() && id == s.last_call_id && s.call_state == CallState::Idle {
        return;
    }
    let rejection = if s.dnd {
        result.commands.push(Command {
            kind: CommandKind::Hangup,
            parameter: if s.call_state == CallState::Idle {
                String::new()
            } else {
                id.into()
            },
        });
        Some((NotificationKind::RejectedDnd, CallOutcome::RejectedDnd))
    } else if s.call_state != CallState::Idle {
        if !id.is_empty() && id == s.call_id {
            return;
        }
        if !id.is_empty() {
            result.commands.push(Command {
                kind: CommandKind::Hangup,
                parameter: id.into(),
            });
        }
        Some((NotificationKind::RejectedBusy, CallOutcome::RejectedBusy))
    } else {
        None
    };
    if let Some((kind, outcome)) = rejection {
        result.notifications.push(Notification {
            kind,
            body: who.clone(),
        });
        result.history.push(HistoryEntry {
            direction: CallDirection::Incoming,
            outcome,
            peer: who,
            target: uri.into(),
            started_at: Some(now),
            connected_at: None,
            ended_at: Some(now),
        });
        if !id.is_empty() {
            s.ignored_call_ids[s.ignored_call_index as usize % 8] = id.into();
            s.ignored_call_index = s.ignored_call_index.wrapping_add(1);
        }
    } else {
        begin_call(
            s,
            CallState::Incoming,
            CallDirection::Incoming,
            id,
            &who,
            uri,
            now,
        );
        result.notifications.push(Notification {
            kind: NotificationKind::Incoming,
            body: who,
        });
    }
}

fn reduce_closed(result: &mut Transition, id: &str, now: DateTime<Utc>) {
    let s = &mut result.state;
    if s.call_state == CallState::Idle || !event_matches_current_call(s, id) {
        return;
    }
    let outcome = match s.call_state {
        CallState::Incoming if s.end_requested => CallOutcome::Rejected,
        CallState::Incoming => {
            result.notifications.push(Notification {
                kind: NotificationKind::Missed,
                body: s.peer.clone(),
            });
            CallOutcome::Missed
        }
        CallState::Outgoing if s.end_requested => CallOutcome::Canceled,
        CallState::Outgoing => CallOutcome::NotConnected,
        _ => CallOutcome::Connected,
    };
    result.history.push(HistoryEntry {
        direction: s.call_direction,
        outcome,
        peer: s.peer.clone(),
        target: s.call_target.clone(),
        started_at: s.call_created_at,
        connected_at: s.call_started_at,
        ended_at: Some(now),
    });
    s.last_call_id = if s.call_id.is_empty() {
        id.into()
    } else {
        s.call_id.clone()
    };
    s.call_state = CallState::Idle;
    s.call_id.clear();
    s.call_direction = CallDirection::Unknown;
    s.call_target.clear();
    s.peer.clear();
    s.muted = false;
    s.end_requested = false;
    s.call_created_at = None;
    s.call_ended_at = Some(now);
    s.call_state_changed_at = Some(now);
}

fn event_matches_current_call(s: &State, id: &str) -> bool {
    if !id.is_empty() && id == s.last_call_id && s.call_id.is_empty() {
        return false;
    }
    s.call_id.is_empty() || id.is_empty() || s.call_id == id
}

pub fn validate_input(text: &str, max_runes: usize) -> Result<(), DomainError> {
    if text.chars().count() > max_runes {
        return Err(DomainError::InputTooLong);
    }
    if text.chars().any(char::is_control) {
        return Err(DomainError::ControlCharacter);
    }
    Ok(())
}

pub fn clamp_text(text: &str, max_runes: usize) -> String {
    if max_runes == 0 {
        return String::new();
    }
    let mut chars = text.chars().map(|c| if c.is_control() { ' ' } else { c });
    let mut result: String = chars.by_ref().take(max_runes).collect();
    if chars.next().is_some() {
        result.pop();
        result.push('…');
    }
    result
}

pub fn notification_text(text: &str) -> String {
    clamp_text(text, 80)
        .chars()
        .filter(|c| !matches!(c, '<' | '>' | '&' | '"'))
        .collect()
}

pub fn normalize_dial_target(raw: &str) -> String {
    let target = raw.trim();
    if target.is_empty() || target.chars().count() > MAX_DIAL_TARGET_LENGTH {
        return String::new();
    }
    let lower = target.to_ascii_lowercase();
    let uri = lower.starts_with("sip:") || lower.starts_with("sips:");
    if uri || (target.contains('@') && !target.starts_with('@')) {
        if !target.chars().all(|c| ('!'..='~').contains(&c)) {
            return String::new();
        }
        return if uri {
            target.into()
        } else {
            format!("sip:{target}")
        };
    }
    target
        .chars()
        .filter(|c| c.is_ascii_digit() || matches!(c, '+' | '*' | '#'))
        .collect()
}

static CONTACT_URI: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"(?i)^sips?:[^<>\t\n\f\r ;"@]+@[^<>\t\n\f\r ;"]+$"#).unwrap());

pub fn normalize_contact_uri(raw: &str, domain: &str) -> String {
    let value = raw.trim();
    if value.is_empty() || value.chars().count() > 200 {
        return String::new();
    }
    let lower = value.to_ascii_lowercase();
    let uri = if lower.starts_with("sip:") || lower.starts_with("sips:") {
        let colon = value.find(':').unwrap() + 1;
        format!("{}{}", &lower[..colon], &value[colon..])
    } else if value.contains('@') && !value.starts_with('@') {
        format!("sip:{value}")
    } else {
        let extension: String = value
            .chars()
            .filter(|c| c.is_ascii_digit() || matches!(c, '+' | '*' | '#'))
            .collect();
        if extension.is_empty() || domain.trim().is_empty() {
            return String::new();
        }
        return {
            let uri = format!("sip:{extension}@{}", domain.trim());
            if CONTACT_URI.is_match(&uri) {
                uri
            } else {
                String::new()
            }
        };
    };
    if uri.matches('@').count() == 1 && CONTACT_URI.is_match(&uri) {
        uri
    } else {
        String::new()
    }
}

fn strip_sip_scheme(text: &str) -> &str {
    if text
        .get(..5)
        .is_some_and(|s| s.eq_ignore_ascii_case("sips:"))
    {
        &text[5..]
    } else if text
        .get(..4)
        .is_some_and(|s| s.eq_ignore_ascii_case("sip:"))
    {
        &text[4..]
    } else {
        text
    }
}

fn path_unescape(text: &str) -> Option<Vec<u8>> {
    let bytes = text.as_bytes();
    let mut result = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let hi = (*bytes.get(i + 1)? as char).to_digit(16)?;
            let lo = (*bytes.get(i + 2)? as char).to_digit(16)?;
            result.push((hi * 16 + lo) as u8);
            i += 3;
        } else {
            result.push(bytes[i]);
            i += 1;
        }
    }
    Some(result)
}

pub fn peer_display(uri: &str) -> String {
    let bounded = clamp_text(uri, MAX_PEER_INPUT_LENGTH);
    let mut text = bounded.as_str();
    let mut display = "";
    if let (Some(open), Some(close)) = (text.find('<'), text.rfind('>'))
        && close > open
    {
        display = text[..open].trim().trim_matches('"').trim();
        text = &text[open + 1..close];
    }
    text = strip_sip_scheme(text.trim_matches(['<', '>']).trim());
    if let Some(at) = text.rfind('@').filter(|at| *at > 0) {
        text = &text[..at];
    }
    if let Some(semi) = text.find(';') {
        text = &text[..semi];
    }
    let user = path_unescape(text)
        .map(|bytes| {
            let mut decoded = String::new();
            for chunk in bytes.utf8_chunks() {
                decoded.push_str(chunk.valid());
                // Go emits one replacement character for each invalid byte.
                decoded.extend(chunk.invalid().iter().map(|_| char::REPLACEMENT_CHARACTER));
            }
            decoded
        })
        .unwrap_or_else(|| text.into());
    clamp_text(
        &if display.is_empty() {
            user
        } else {
            format!("{display} ({user})")
        },
        MAX_PEER_DISPLAY_LENGTH,
    )
}

fn parse_caller_address(raw: &str) -> Option<(Vec<u8>, String)> {
    let mut text = raw.trim();
    if text.is_empty() || text.chars().count() > MAX_PEER_INPUT_LENGTH {
        return None;
    }
    if let Some(open) = text.find('<') {
        let close = text[open + 1..].find('>')? + open + 1;
        text = &text[open + 1..close];
    }
    text = strip_sip_scheme(text);
    text = text.split('?').next()?;
    let (user, host) = text.rsplit_once('@')?;
    if user.is_empty() || user.contains('@') {
        return None;
    }
    let host = host.split(';').next()?;
    if host.is_empty() {
        return None;
    }
    let user = path_unescape(user)?;
    if user.is_empty() {
        return None;
    }
    Some((user, simple_lowercase(host)))
}

fn address_number(user: &[u8], country_code: &str) -> String {
    std::str::from_utf8(user)
        .map(|text| normalize_caller_number(text, country_code))
        .unwrap_or_default()
}

pub fn normalize_caller_number(user: &str, country_code: &str) -> String {
    let digits = user.strip_prefix('+').unwrap_or(user);
    if !digits.starts_with(|c: char| c.is_ascii_digit()) {
        return String::new();
    }
    if user.chars().enumerate().any(|(i, c)| {
        !(c.is_ascii_digit() || matches!(c, ' ' | '(' | ')' | '.' | '-') || (i == 0 && c == '+'))
    }) {
        return String::new();
    }
    let mut number: String = user
        .chars()
        .filter(|c| !matches!(c, ' ' | '(' | ')' | '.' | '-'))
        .collect();
    if number.starts_with("00") {
        number = format!("+{}", &number[2..]);
    }
    let country = country_code
        .trim()
        .strip_prefix('+')
        .unwrap_or(country_code.trim());
    let valid = (1..=3).contains(&country.len())
        && !country.starts_with('0')
        && country.bytes().all(|c| c.is_ascii_digit());
    if valid
        && number.len() >= 7
        && number.starts_with('0')
        && matches!(number.as_bytes()[1], b'1'..=b'9')
    {
        number = format!("+{country}{}", &number[1..]);
    }
    number
}

fn is_international_number(number: &str) -> bool {
    (8..=16).contains(&number.len())
        && number.starts_with('+')
        && number.as_bytes()[1] != b'0'
        && number.as_bytes()[1..].iter().all(u8::is_ascii_digit)
}

pub fn contact_name(contacts: &[Contact], target: &str, country_code: &str) -> Option<String> {
    let Some((user, host)) = parse_caller_address(target) else {
        if target.contains('@') {
            return None;
        }
        let number = normalize_caller_number(target.trim(), country_code);
        if number.is_empty() {
            return None;
        }
        let mut found: Option<String> = None;
        for contact in contacts {
            let Some((user, _)) = parse_caller_address(&contact.uri) else {
                continue;
            };
            if address_number(&user, country_code) != number {
                continue;
            }
            let name = clamp_text(&contact.name, MAX_PEER_DISPLAY_LENGTH)
                .trim()
                .to_owned();
            if name.is_empty() {
                continue;
            }
            if found.as_ref().is_some_and(|previous| previous != &name) {
                return None;
            }
            found = Some(name);
        }
        return found;
    };
    let number = address_number(&user, country_code);
    let mut best_score = 0;
    let mut best_name = String::new();
    let mut ambiguous = false;
    for contact in contacts {
        let Some((other_user, other_host)) = parse_caller_address(&contact.uri) else {
            continue;
        };
        let score = if user == other_user && host == other_host {
            3
        } else if !number.is_empty() && number == address_number(&other_user, country_code) {
            if host == other_host {
                2
            } else if is_international_number(&number) {
                1
            } else {
                0
            }
        } else {
            0
        };
        if score == 0 || score < best_score {
            continue;
        }
        let name = clamp_text(&contact.name, MAX_PEER_DISPLAY_LENGTH)
            .trim()
            .to_owned();
        if score > best_score {
            best_score = score;
            best_name = name;
            ambiguous = false;
        } else if name != best_name {
            ambiguous = true;
        }
    }
    if best_name.is_empty() || ambiguous {
        None
    } else {
        Some(best_name)
    }
}

pub fn caller_name(contacts: &[Contact], uri: &str, display: &str, country_code: &str) -> String {
    contact_name(contacts, uri, country_code).unwrap_or_else(|| {
        let name = clamp_text(display, MAX_PEER_DISPLAY_LENGTH)
            .trim()
            .to_owned();
        if name.is_empty() {
            peer_display(uri)
        } else {
            name
        }
    })
}

pub fn resolve_history_peers(
    entries: &[HistoryEntry],
    contacts: &[Contact],
    country_code: &str,
) -> Vec<HistoryEntry> {
    entries
        .iter()
        .cloned()
        .map(|mut entry| {
            if let Some(name) = contact_name(contacts, &entry.target, country_code) {
                entry.peer = name;
            }
            entry
        })
        .collect()
}

fn simple_lowercase(text: &str) -> String {
    // Go uses one-code-point case mappings, not contextual or expanding mappings.
    text.chars()
        .map(|c| c.to_lowercase().next().unwrap())
        .collect()
}

static LETTER_OR_DIGIT: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[\p{L}\p{Nd}]$").unwrap());

fn is_letter_or_digit(c: char) -> bool {
    LETTER_OR_DIGIT.is_match(c.encode_utf8(&mut [0; 4]))
}

pub fn fuzzy_score(needle: &str, text: &str) -> i32 {
    let query: Vec<char> = simple_lowercase(needle.trim()).chars().collect();
    let haystack: Vec<char> = simple_lowercase(text).chars().collect();
    if query.is_empty() {
        return 0;
    }
    if haystack.is_empty() {
        return -1;
    }
    if query == haystack {
        return 1000;
    }
    let mut score = 0;
    let mut query_index = 0;
    let mut previous = -2;
    for (i, c) in haystack.iter().enumerate() {
        if query_index >= query.len() {
            break;
        }
        if *c != query[query_index] {
            continue;
        }
        score += 10;
        if i as i32 == previous + 1 {
            score += 10;
        }
        if i == 0 || !is_letter_or_digit(haystack[i - 1]) {
            score += 15;
        }
        previous = i as i32;
        query_index += 1;
    }
    if query_index < query.len() {
        return -1;
    }
    if let Some(start) = haystack.windows(query.len()).position(|w| w == query) {
        score += if start == 0 { 200 } else { 100 };
    }
    score - haystack.len().min(50) as i32
}

pub fn filter_contacts(contacts: &[Contact], query: &str) -> Vec<Contact> {
    if query.trim().is_empty() {
        return contacts.to_vec();
    }
    let mut scored: Vec<_> = contacts
        .iter()
        .filter_map(|contact| {
            let score = fuzzy_score(query, &contact.name)
                .max(fuzzy_score(
                    query,
                    &format!("{} {}", contact.uri, contact.name),
                ))
                .max(fuzzy_score(query, &contact.uri) - 10);
            (score >= 0).then_some((score, contact.clone()))
        })
        .collect();
    scored.sort_by_key(|entry| std::cmp::Reverse(entry.0));
    scored.into_iter().map(|(_, contact)| contact).collect()
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RegistrationInfo {
    pub known: bool,
    pub count: usize,
    pub registered: bool,
    pub failed: bool,
    pub aor: String,
}

static ANSI: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\x1b\[[0-?]*[ -/]*[@-~]").unwrap());
static USER_AGENTS: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)User Agents \(([0-9]+)\)").unwrap());
static AOR: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)sips?:([^;>\t\n\f\r ]+)").unwrap());
static REG_OK: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?-u:\bOK\b)").unwrap());
static REG_FAIL: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)(?-u:\b(?:ERR|FAIL)\b)").unwrap());
static AUDIO_FAILURE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)no such|Format should be|failed").unwrap());
static CALL_FAILURE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)command not found|no active call|not found|could not|cannot|can't|unable to|failed|error|invalid").unwrap()
});

pub fn strip_ansi(text: &str) -> String {
    ANSI.replace_all(text, "").into_owned()
}

pub fn parse_registration_output(data: &str) -> RegistrationInfo {
    let clean = strip_ansi(data);
    let Some(count) = USER_AGENTS
        .captures(&clean)
        .and_then(|c| c[1].parse::<isize>().ok())
    else {
        return RegistrationInfo::default();
    };
    let failed = REG_FAIL.is_match(&clean);
    RegistrationInfo {
        known: true,
        count: count as usize,
        registered: count > 0 && REG_OK.is_match(&clean) && !failed,
        failed,
        aor: AOR
            .captures(&clean)
            .map(|c| clamp_text(&c[1], MAX_REGISTRATION_AOR))
            .unwrap_or_default(),
    }
}

fn first_line_error(data: &str, pattern: &Regex) -> String {
    let clean = strip_ansi(data);
    match clean
        .split('\n')
        .map(str::trim)
        .find(|line| !line.is_empty())
    {
        Some(line) if pattern.is_match(line) => clamp_text(line, MAX_COMMAND_ERROR),
        _ => String::new(),
    }
}
pub fn parse_audio_command_error(data: &str) -> String {
    first_line_error(data, &AUDIO_FAILURE)
}
pub fn parse_call_command_error(data: &str) -> String {
    first_line_error(data, &CALL_FAILURE)
}
pub use caller_name as incoming_caller;
pub use normalize_dial_target as normalize_target;
pub use parse_audio_command_error as audio_switch_error;
pub use parse_registration_output as parse_reginfo;

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{Duration, TimeZone};

    fn at() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 2, 3, 4, 5, 6).unwrap()
    }
    fn contact(uri: &str, name: &str) -> Contact {
        Contact {
            uri: uri.into(),
            name: name.into(),
        }
    }
    fn call(kind: CallEventType, id: &str, uri: &str) -> Event {
        Event::Call {
            kind,
            id: id.into(),
            peer_uri: uri.into(),
            peer_display_name: String::new(),
        }
    }
    fn step(s: &State, event: Event, seconds: i64) -> Transition {
        reduce(
            s,
            event,
            &[contact("sip:201@pbx", "Anna")],
            "49",
            at() + Duration::seconds(seconds),
        )
    }
    fn action(s: &State, action: Action) -> Transition {
        step(s, Event::Action(action), 0)
    }

    #[test]
    fn normalize_targets_and_contact_addresses() {
        for (raw, expected) in [
            (" 201 ", "201"),
            ("+49 (30) 12-34", "+49301234"),
            ("*123#", "*123#"),
            ("alice@example.com", "sip:alice@example.com"),
            (
                "sip:alice@example.com;transport=tcp",
                "sip:alice@example.com;transport=tcp",
            ),
            ("SIPS:alice@example.com", "SIPS:alice@example.com"),
            ("alice @example.com", ""),
            ("not a target", ""),
            ("@example.com", ""),
            ("sip:alice@example.com\ncommand", ""),
            ("sip:å@example.com", ""),
        ] {
            assert_eq!(normalize_dial_target(raw), expected, "{raw}");
        }
        assert!(normalize_dial_target(&"1".repeat(256)).is_empty());
        for (raw, domain, expected) in [
            ("201", "pbx.example.com", "sip:201@pbx.example.com"),
            (
                "+49 (30) 123",
                "pbx.example.com",
                "sip:+4930123@pbx.example.com",
            ),
            ("alice@example.com", "", "sip:alice@example.com"),
            ("SIP:Alice@Example.com", "", "sip:Alice@Example.com"),
            ("Sips:bob@example.com", "", "sips:bob@example.com"),
            ("201", "", ""),
            ("sip:alice", "pbx", ""),
            ("alice@@example.com", "", ""),
            ("sip:alice@pbx;transport=tcp", "", ""),
        ] {
            assert_eq!(normalize_contact_uri(raw, domain), expected, "{raw}");
        }
        assert!(normalize_contact_uri(&"a".repeat(201), "pbx").is_empty());
    }

    #[test]
    fn unicode_and_percent_decoding_keep_go_matching_semantics() {
        assert_eq!(fuzzy_score("i", "İ"), 1000);
        assert_eq!(fuzzy_score("οσ", "ΟΣ"), 1000);
        assert_eq!(fuzzy_score("a", "½a"), 123);
        assert_eq!(peer_display("sip:%E2%82@host"), "��");
        let contacts = [contact("sip:%FF@host", "Anna")];
        assert_eq!(
            contact_name(&contacts, "sip:%FF@HOST", ""),
            Some("Anna".into())
        );
        assert_eq!(contact_name(&contacts, "sip:%FE@host", ""), None);
        assert_eq!(
            normalize_contact_uri("sip:a@b\u{a0}c", ""),
            "sip:a@b\u{a0}c"
        );
    }

    #[test]
    fn peer_display_and_unicode_clamping() {
        for (raw, expected) in [
            ("sip:203@sip.example.com", "203"),
            (
                "\"Alice\" <sips:203@example.com;transport=tls>",
                "Alice (203)",
            ),
            ("sip:%2B4930123@example.com", "+4930123"),
            ("sip:bad%ZZ@pbx", "bad%ZZ"),
            ("Provider", "Provider"),
        ] {
            assert_eq!(peer_display(raw), expected);
        }
        assert_eq!(validate_input("hello", 5), Ok(()));
        assert_eq!(
            validate_input("hello\n", 10),
            Err(DomainError::ControlCharacter)
        );
        assert_eq!(validate_input("hello", 4), Err(DomainError::InputTooLong));
        assert_eq!(clamp_text("a\nb\u{85}c", 10), "a b c");
        assert_eq!(clamp_text("abcdef", 4), "abc…");
        assert_eq!(clamp_text("åäö", 3), "åäö");
        assert_eq!(clamp_text("😀😀😀", 2), "😀…");
        assert_eq!(clamp_text("ab", 1), "…");
        assert_eq!(clamp_text("a", 0), "");
        assert_eq!(notification_text("<Anna>\n&\""), "Anna ");
        assert_eq!(notification_text(&"x".repeat(100)).chars().count(), 80);
    }

    #[test]
    fn caller_matching_exact_numeric_and_fallbacks() {
        for (saved, peer, display, country, expected) in [
            (
                "sip:201@pbx",
                "sips:201@PBX;transport=tls",
                "Provider",
                "",
                "Anna",
            ),
            (
                "sip:004930123456@pbx",
                "sip:+49-30-123456@trunk",
                "",
                "",
                "Anna",
            ),
            (
                "sip:+4930123456@pbx",
                "\"Caller\" <sip:%2B4930123456@trunk>",
                "",
                "",
                "Anna",
            ),
            (
                "sip:030123456@pbx",
                "sip:+4930123456@trunk",
                "Provider",
                "",
                "Provider",
            ),
            (
                "sip:030123456@pbx",
                "sip:+4930123456@trunk",
                "Provider",
                "49",
                "Anna",
            ),
            (
                "sip:+4930123456@pbx",
                "sip:030123456@trunk",
                "",
                "+49",
                "Anna",
            ),
            (
                "sip:030123456@pbx",
                "sip:+4430123456@trunk",
                "Provider",
                "49",
                "Provider",
            ),
            ("sip:201@pbx", "sip:201@other", "", "", "201"),
            ("sip:Alice@pbx", "sip:alice@pbx", "", "", "alice"),
            (
                "sip:123456@pbx",
                "sip:+4930123456@pbx",
                "",
                "",
                "+4930123456",
            ),
            ("invalid", "sip:201@pbx", "", "", "201"),
            ("sip:201@pbx", "", "Provider", "", "Provider"),
            (
                "sip:alice%2Bdesk@example.com;transport=tcp",
                "\"Network\" <SIPS:alice%2Bdesk@EXAMPLE.COM;transport=tls?subject=x>",
                "Network",
                "",
                "Anna",
            ),
        ] {
            assert_eq!(
                caller_name(&[contact(saved, "Anna")], peer, display, country),
                expected,
                "{saved}, {peer}, {country}"
            );
        }
        assert_eq!(
            caller_name(&[], "sip:201@pbx", "support@example.com", "49"),
            "support@example.com"
        );
        assert_eq!(caller_name(&[], "sip:alice@pbx", "   ", "49"), "alice");
        assert_eq!(
            caller_name(
                &[contact("sip:201@pbx", "  ")],
                "sip:201@pbx",
                "Provider",
                ""
            ),
            "Provider"
        );
        assert_eq!(caller_name(&[], "", "", ""), "");
        assert!(parse_caller_address("<sip:alice@pbx").is_none());
        assert!(parse_caller_address("sip:a@b@c").is_none());
        assert!(parse_caller_address("sip:%ZZ@pbx").is_none());
        let long = contact("sip:201@pbx", &format!("<Anna>\n{}", "x".repeat(100)));
        let name = caller_name(&[long], "sip:201@pbx", "", "");
        assert_eq!(name.chars().count(), MAX_PEER_DISPLAY_LENGTH);
        assert!(!notification_text(&name).contains(['\n', '<', '>', '&', '"']));
    }

    #[test]
    fn ambiguous_matching_is_order_independent_and_exact_wins() {
        let mut contacts = vec![
            contact("sip:+4930123456@one", "Anna"),
            contact("sip:004930123456@two", "Bob"),
        ];
        for _ in 0..2 {
            assert_eq!(
                caller_name(&contacts, "sip:+4930123456@trunk", "Provider", ""),
                "Provider"
            );
            assert_eq!(
                caller_name(&contacts, "sip:+4930123456@one", "", ""),
                "Anna"
            );
            assert!(contact_name(&contacts, "030123456", "49").is_none());
            contacts.reverse();
        }
        contacts[0].name = "Anna".into();
        contacts[1].name = "Anna".into();
        assert_eq!(
            contact_name(&contacts, "+4930123456", "49"),
            Some("Anna".into())
        );
        let contacts = [
            contact("sip:alice@example.com", "Alice"),
            contact("sip:+4930123456@pbx", "Bob"),
        ];
        for (target, name) in [
            ("sip:alice@example.com", "Alice"),
            ("030123456", "Bob"),
            ("+4930123456", "Bob"),
        ] {
            assert_eq!(contact_name(&contacts, target, "49").as_deref(), Some(name));
        }
        assert!(contact_name(&contacts, "201", "49").is_none());
        for number in ["+", "(030)123456", "12x3", "12+34", "*123#"] {
            assert!(normalize_caller_number(number, "49").is_empty());
        }
        for country in ["", "0", "0049", "abc", "1234"] {
            assert_eq!(normalize_caller_number("030123456", country), "030123456");
        }
    }

    #[test]
    fn history_resolution_copies_and_search_is_stable() {
        let entries = vec![
            HistoryEntry {
                peer: "alice".into(),
                target: "sip:alice@example.com".into(),
                ..Default::default()
            },
            HistoryEntry {
                peer: "Provider".into(),
                target: "sip:unknown@pbx".into(),
                ..Default::default()
            },
        ];
        let resolved =
            resolve_history_peers(&entries, &[contact("sip:alice@example.com", "Alice")], "49");
        assert_eq!(resolved[0].peer, "Alice");
        assert_eq!(resolved[1].peer, "Provider");
        assert_eq!(entries[0].peer, "alice");
        let contacts = vec![
            contact("sip:201@pbx", "Support"),
            contact("sip:202@pbx", "Sam Porter"),
            contact("sip:support@example.com", "Desk"),
        ];
        assert_eq!(
            filter_contacts(&contacts, "support"),
            vec![contacts[0].clone(), contacts[2].clone()]
        );
        assert_eq!(
            filter_contacts(&contacts, "supt"),
            vec![contacts[0].clone(), contacts[2].clone()]
        );
        assert!(filter_contacts(&contacts, "zzz").is_empty());
        assert_eq!(filter_contacts(&contacts, "  "), contacts);
        let ties = [
            contact("sip:201@pbx", "Same"),
            contact("sip:202@pbx", "Same"),
        ];
        assert_eq!(filter_contacts(&ties, "same"), ties);
        assert_eq!(fuzzy_score("same", "SAME"), 1000);
        assert_eq!(fuzzy_score("", "text"), 0);
        assert_eq!(fuzzy_score("query", ""), -1);
    }

    #[test]
    fn reginfo_and_first_nonempty_command_line() {
        let info = parse_registration_output(
            "--- User Agents (1) ---\n\x1b[32msip:201@example.com OK\x1b[0m",
        );
        assert_eq!(
            info,
            RegistrationInfo {
                known: true,
                count: 1,
                registered: true,
                failed: false,
                aor: "201@example.com".into()
            }
        );
        let info = parse_reginfo("--- User Agents (1) ---\nsips:201@example.com FAIL timeout");
        assert!(info.known && info.failed && !info.registered);
        assert!(!parse_reginfo("User Agents (1)\nOK ERR").registered);
        assert!(!parse_reginfo("User Agents (1)\nok").registered);
        assert_eq!(parse_reginfo("User Agents (0)").count, 0);
        assert!(!parse_reginfo("not reginfo").known);
        assert!(!parse_reginfo(&format!("User Agents ({})", "9".repeat(100))).known);
        assert_eq!(
            parse_audio_command_error(
                "\n\x1b[31mno such device for pipewire audio-player: missing\x1b[0m\nnode list"
            ),
            "no such device for pipewire audio-player: missing"
        );
        assert_eq!(
            parse_audio_command_error("pipewire,headset\nfailed later"),
            ""
        );
        for refusal in [
            "no active call\n",
            "command not found (mute)\n",
            "could not answer call (Invalid argument [22])\n",
            "can't find a URI to dial to\n",
        ] {
            assert!(!parse_call_command_error(refusal).is_empty());
        }
        for success in ["", "\n\n", "dialing\nfailed later"] {
            assert!(parse_call_command_error(success).is_empty());
        }
        let long = format!("failed: {}", "x".repeat(300));
        assert_eq!(
            parse_call_command_error(&long).chars().count(),
            MAX_COMMAND_ERROR
        );
        assert_eq!(
            parse_audio_command_error(&long).chars().count(),
            MAX_COMMAND_ERROR
        );
    }

    #[test]
    fn reducer_incoming_missed_and_connected_calls() {
        let initial = State::new();
        let incoming = step(
            &initial,
            call(CallEventType::Incoming, "call-1", "sip:201@pbx"),
            0,
        );
        assert_eq!(initial.call_state, CallState::Idle);
        assert_eq!(incoming.state.peer, "Anna");
        assert_eq!(incoming.state.call_id, "call-1");
        assert_eq!(incoming.state.call_state_changed_at, Some(at()));
        assert_eq!(
            incoming.notifications,
            [Notification {
                kind: NotificationKind::Incoming,
                body: "Anna".into()
            }]
        );
        let missed = step(
            &incoming.state,
            call(CallEventType::Closed, "call-1", ""),
            10,
        );
        assert_eq!(missed.state.call_state, CallState::Idle);
        assert_eq!(missed.state.last_call_id, "call-1");
        assert!(missed.state.peer.is_empty());
        assert_eq!(missed.notifications[0].kind, NotificationKind::Missed);
        assert_eq!(
            missed.history,
            [HistoryEntry {
                direction: CallDirection::Incoming,
                outcome: CallOutcome::Missed,
                peer: "Anna".into(),
                target: "sip:201@pbx".into(),
                started_at: Some(at()),
                connected_at: None,
                ended_at: Some(at() + Duration::seconds(10))
            }]
        );
        let answer = action(&incoming.state, Action::Answer);
        assert_eq!(answer.commands[0].kind, CommandKind::Accept);
        assert_eq!(answer.state.call_state, CallState::Incoming);
        let established = step(
            &answer.state,
            call(CallEventType::Established, "call-1", ""),
            2,
        );
        assert_eq!(established.state.call_state, CallState::Active);
        assert_eq!(
            established.state.call_started_at,
            Some(at() + Duration::seconds(2))
        );
        let muted = action(&established.state, Action::ToggleMute);
        assert!(muted.state.muted);
        assert_eq!(muted.commands[0].kind, CommandKind::Mute);
        let closed = step(&muted.state, call(CallEventType::Closed, "call-1", ""), 62);
        assert_eq!(
            closed.state.call_started_at,
            established.state.call_started_at
        );
        assert_eq!(
            closed.state.call_ended_at,
            Some(at() + Duration::seconds(62))
        );
        assert!(!closed.state.muted);
        assert!(closed.notifications.is_empty());
        assert_eq!(closed.history[0].outcome, CallOutcome::Connected);
        assert_eq!(
            closed.history[0].connected_at,
            established.state.call_started_at
        );
    }

    #[test]
    fn reducer_actions_validate_and_preserve_single_call() {
        let registered = State {
            registered: true,
            ..State::default()
        };
        let dial = action(&registered, Action::Dial("alice@example.com".into()));
        assert!(dial.error.is_none());
        assert_eq!(dial.state.call_state, CallState::Outgoing);
        assert_eq!(dial.state.peer, "alice");
        assert_eq!(
            dial.commands,
            [Command {
                kind: CommandKind::Dial,
                parameter: "sip:alice@example.com".into()
            }]
        );
        assert_eq!(
            action(&dial.state, Action::Dial("202".into())).error,
            Some(DomainError::CallInProgress)
        );
        assert_eq!(
            action(&State::default(), Action::Dial("201".into())).error,
            Some(DomainError::NotRegistered)
        );
        assert_eq!(
            action(&registered, Action::Dial("not a target".into())).error,
            Some(DomainError::EmptyTarget)
        );
        assert!(action(&registered, Action::Answer).commands.is_empty());
        assert!(action(&registered, Action::Hangup).commands.is_empty());
        assert!(action(&registered, Action::ToggleMute).commands.is_empty());
        assert!(action(&registered, Action::ToggleDnd).state.dnd);
        let dial = action(&registered, Action::Dial("201".into()));
        assert_eq!(dial.state.peer, "Anna");
        let closed = step(&dial.state, call(CallEventType::Closed, "new", ""), 1);
        assert_eq!(closed.history[0].outcome, CallOutcome::NotConnected);
        assert_eq!(closed.history[0].target, "201");
        let established = step(&dial.state, call(CallEventType::Established, "new", ""), 1);
        assert_eq!(established.state.call_id, "new");
        assert_eq!(established.state.call_state, CallState::Active);
    }

    #[test]
    fn reducer_manual_reject_and_cancel_wait_for_closed() {
        for (state, command, outcome) in [
            (
                CallState::Incoming,
                CommandKind::Reject,
                CallOutcome::Rejected,
            ),
            (
                CallState::Outgoing,
                CommandKind::Hangup,
                CallOutcome::Canceled,
            ),
            (
                CallState::Active,
                CommandKind::Hangup,
                CallOutcome::Connected,
            ),
        ] {
            let current = State {
                call_state: state,
                call_id: "current".into(),
                ..State::default()
            };
            let requested = action(&current, Action::Hangup);
            assert!(requested.state.end_requested);
            assert_eq!(requested.state.call_state, state);
            assert_eq!(requested.commands[0].kind, command);
            let closed = step(
                &requested.state,
                call(CallEventType::Closed, "current", ""),
                2,
            );
            assert_eq!(closed.history[0].outcome, outcome);
            assert!(closed.notifications.is_empty());
        }
    }

    #[test]
    fn reducer_dnd_busy_and_stale_ids() {
        for dnd in [false, true] {
            let state = State {
                call_state: CallState::Active,
                call_id: "current".into(),
                peer: "Anna".into(),
                muted: true,
                dnd,
                call_created_at: Some(at()),
                call_started_at: Some(at()),
                ..State::default()
            };
            let rejected = step(
                &state,
                call(CallEventType::Incoming, "second", "sip:202@pbx"),
                1,
            );
            assert_eq!(rejected.state.call_state, CallState::Active);
            assert_eq!(rejected.state.call_id, "current");
            assert_eq!(rejected.state.peer, "Anna");
            assert!(rejected.state.muted);
            assert_eq!(
                rejected.commands,
                [Command {
                    kind: CommandKind::Hangup,
                    parameter: "second".into()
                }]
            );
            assert_eq!(
                rejected.history[0].outcome,
                if dnd {
                    CallOutcome::RejectedDnd
                } else {
                    CallOutcome::RejectedBusy
                }
            );
            for kind in [
                CallEventType::Incoming,
                CallEventType::Established,
                CallEventType::Closed,
            ] {
                let duplicate = step(&rejected.state, call(kind, "second", "sip:202@pbx"), 2);
                assert_eq!(duplicate.state, rejected.state);
                assert!(duplicate.commands.is_empty() && duplicate.history.is_empty());
            }
            let closed = step(
                &rejected.state,
                call(CallEventType::Closed, "current", ""),
                3,
            );
            let ready = State {
                registered: true,
                dnd: false,
                ..closed.state
            };
            let outgoing = action(&ready, Action::Dial("203".into()));
            for id in ["second", "current"] {
                assert_eq!(
                    step(&outgoing.state, call(CallEventType::Closed, id, ""), 5)
                        .state
                        .call_state,
                    CallState::Outgoing
                );
            }
        }
        let dnd = State {
            dnd: true,
            ..State::default()
        };
        let rejected = step(&dnd, call(CallEventType::Incoming, "dnd", "sip:201@pbx"), 0);
        assert_eq!(rejected.state.call_state, CallState::Idle);
        assert!(rejected.commands[0].parameter.is_empty());
        assert_eq!(rejected.history[0].started_at, rejected.history[0].ended_at);
        let active = State {
            call_state: CallState::Active,
            call_id: "current".into(),
            ..State::default()
        };
        assert!(
            step(&active, call(CallEventType::Incoming, "current", ""), 0)
                .notifications
                .is_empty()
        );
        let unknown = step(&active, call(CallEventType::Incoming, "", ""), 0);
        assert!(unknown.commands.is_empty());
        assert_eq!(unknown.history[0].outcome, CallOutcome::RejectedBusy);
        let closed = step(&active, call(CallEventType::Closed, "current", ""), 0);
        assert_eq!(
            step(
                &closed.state,
                call(CallEventType::Incoming, "current", ""),
                1
            )
            .state,
            closed.state
        );
        assert_eq!(
            step(
                &closed.state,
                call(CallEventType::Established, "current", ""),
                1
            )
            .state,
            closed.state
        );
        assert_eq!(
            step(&active, call(CallEventType::Closed, "wrong", ""), 1).state,
            active
        );
    }

    #[test]
    fn ignored_call_ring_wraps_without_overflow() {
        let mut state = State {
            dnd: true,
            ..State::default()
        };
        for i in 0..300 {
            state = step(
                &state,
                call(CallEventType::Incoming, &i.to_string(), "sip:201@pbx"),
                i,
            )
            .state;
        }
        assert!(state.ignored_call_ids.contains(&"299".to_owned()));
        assert!(!state.ignored_call_ids.contains(&"0".to_owned()));
        assert_eq!(state.ignored_call_index, 44);
    }

    #[test]
    fn registration_events_snapshots_and_bounds() {
        let event = |kind, aor: &str, detail: &str| Event::Register {
            kind,
            account_aor: aor.into(),
            detail: detail.into(),
        };
        let registered = step(
            &State::default(),
            event(RegisterEventType::Ok, "sips:201@example.com", ""),
            0,
        );
        assert!(registered.state.registered);
        assert_eq!(registered.state.registration, RegistrationState::Registered);
        assert_eq!(registered.state.registration_detail, "201@example.com");
        let failed = step(
            &registered.state,
            event(RegisterEventType::Fail, "", "timeout\nretry"),
            0,
        );
        assert!(!failed.state.registered);
        assert_eq!(
            failed.state.registration_detail,
            "registration failed: timeout retry"
        );
        let unregistered = step(
            &failed.state,
            event(RegisterEventType::Unregistering, "", ""),
            0,
        );
        assert_eq!(
            unregistered.state.registration,
            RegistrationState::Unregistered
        );
        assert_eq!(
            step(&State::default(), event(RegisterEventType::Ok, "", ""), 0)
                .state
                .registration_detail,
            "registered"
        );
        assert_eq!(
            step(&State::default(), event(RegisterEventType::Fail, "", ""), 0)
                .state
                .registration_detail,
            "registration failed"
        );
        assert_eq!(
            step(
                &State::default(),
                Event::RegistrationSnapshot(RegistrationInfo::default()),
                0
            )
            .state,
            State::default()
        );
        for (count, registered, failed, expected, detail) in [
            (
                0,
                true,
                false,
                RegistrationState::Unregistered,
                "unregistered",
            ),
            (1, true, false, RegistrationState::Registered, "registered"),
            (
                1,
                false,
                true,
                RegistrationState::Failed,
                "registration failed",
            ),
            (
                1,
                false,
                false,
                RegistrationState::Registering,
                "registering",
            ),
        ] {
            let result = step(
                &State::default(),
                Event::RegistrationSnapshot(RegistrationInfo {
                    known: true,
                    count,
                    registered,
                    failed,
                    aor: String::new(),
                }),
                0,
            );
            assert_eq!(result.state.registration, expected);
            assert_eq!(result.state.registration_detail, detail);
        }
        let result = step(
            &State::default(),
            event(RegisterEventType::Ok, &"x".repeat(200), ""),
            0,
        );
        assert_eq!(
            result.state.registration_detail.chars().count(),
            MAX_REGISTRATION_AOR
        );
    }

    #[test]
    fn history_json_uses_go_field_names_and_zero_times() {
        let entry = HistoryEntry {
            direction: CallDirection::Incoming,
            outcome: CallOutcome::RejectedDnd,
            peer: "Anna".into(),
            target: "sip:201@pbx".into(),
            started_at: Some(at()),
            connected_at: None,
            ended_at: Some(at()),
        };
        let value = serde_json::to_value(&entry).unwrap();
        assert_eq!(value["connected_at"], "0001-01-01T00:00:00Z");
        assert_eq!(value["outcome"], "rejected_dnd");
        assert_eq!(value.as_object().unwrap().len(), 7);
        assert_eq!(
            serde_json::from_value::<HistoryEntry>(value).unwrap(),
            entry
        );
        let zero: HistoryEntry =
            serde_json::from_str(r#"{"connected_at":"0001-01-01T01:00:00+01:00"}"#).unwrap();
        assert!(zero.connected_at.is_none());
        assert!(serde_json::from_str::<HistoryEntry>(r#"{"unexpected":true}"#).is_err());
        let mut value = serde_json::to_value(&entry).unwrap();
        value["started_at"] = "2026-02-03T05:05:06.123456789+01:00".into();
        let decoded: HistoryEntry = serde_json::from_value(value).unwrap();
        assert_eq!(
            decoded.started_at.unwrap(),
            at() + Duration::nanoseconds(123456789)
        );
    }
}
