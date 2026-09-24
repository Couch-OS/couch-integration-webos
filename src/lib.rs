//! LG webOS LAN control over SSAP.
//!
//! One package child owns one control WebSocket and, when navigation is used,
//! one pointer WebSocket. Commands are never retried. Pairing returns the TV's
//! client key and exact TLS certificate as a host-owned protocol-3 credential.

use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use couch_sdk::{
    couch_model::commands::{valid_input_id, Function},
    tls::{self, Socket},
    Capability, ClientSettings, Credential, DeviceClient, Error as SdkError, PairFailure, PairFlow,
    PairInput, PairPrompt, PairStep, Reason, Result as SdkResult, Selectable, Status,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    collections::HashSet,
    net::{IpAddr, SocketAddr, TcpStream},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use tungstenite::{Message, WebSocket};
use url::Url;

const IO_TIMEOUT: Duration = Duration::from_secs(4);
const REGISTER_TIMEOUT: Duration = Duration::from_secs(5);
const PAIR_BUDGET: Duration = Duration::from_secs(85);
const PAIR_POLL_MS: u32 = 1500;
const CERTIFICATE_CHANGED: &str = "The LG TV certificate changed; pair again";

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Settings {
    pub url: String,
}

impl ClientSettings for Settings {
    const FILE_PREFIX: &'static str = "webos";

    fn validate(&self) -> SdkResult<()> {
        endpoint(&self.url).map(|_| ()).map_err(|_| invalid_url())
    }
}

fn invalid_url() -> SdkError {
    SdkError::Invalid.because(Reason::InvalidSetting {
        field: "url".into(),
        text: "Enter a complete LG TV address such as wss://192.0.2.20:3001/".into(),
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Error {
    Configuration,
    Transport,
    Protocol,
    Timeout,
    Unpaired,
    Rejected,
    Certificate,
}

impl From<Error> for SdkError {
    fn from(error: Error) -> Self {
        match error {
            Error::Configuration => invalid_url(),
            Error::Transport => SdkError::Transport,
            Error::Protocol => SdkError::Protocol,
            Error::Timeout => SdkError::Timeout,
            Error::Unpaired | Error::Certificate => SdkError::Unpaired,
            Error::Rejected => SdkError::Rejected,
        }
    }
}

type Result<T> = std::result::Result<T, Error>;

fn endpoint(raw: &str) -> Result<(Url, IpAddr, u16)> {
    let url = Url::parse(raw).map_err(|_| Error::Configuration)?;
    if !matches!(url.scheme(), "ws" | "wss")
        || !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
        || url.path() != "/"
        || url.query().is_some()
    {
        return Err(Error::Configuration);
    }
    let host = url
        .host_str()
        .ok_or(Error::Configuration)?
        .trim_matches(['[', ']'])
        .parse::<IpAddr>()
        .map_err(|_| Error::Configuration)?;
    if host.is_unspecified() || host.is_multicast() {
        return Err(Error::Configuration);
    }
    let port = url.port_or_known_default().ok_or(Error::Configuration)?;
    if port == 0 {
        return Err(Error::Configuration);
    }
    Ok((url, host, port))
}

fn connect_socket(
    raw: &str,
    pin: Arc<Mutex<Vec<u8>>>,
    timeout: Duration,
) -> Result<WebSocket<Socket>> {
    let (url, host, port) = endpoint(raw)?;
    let tcp = TcpStream::connect_timeout(&SocketAddr::new(host, port), timeout)
        .map_err(|_| Error::Transport)?;
    tcp.set_read_timeout(Some(timeout))
        .map_err(|_| Error::Transport)?;
    tcp.set_write_timeout(Some(timeout))
        .map_err(|_| Error::Transport)?;
    let socket = if url.scheme() == "wss" {
        let config = tls::pinned_client_config(Arc::new(tls::Pin::new(pin, CERTIFICATE_CHANGED)))
            .map_err(|_| Error::Transport)?;
        let name = rustls::pki_types::ServerName::IpAddress(host.into());
        let connection =
            rustls::ClientConnection::new(Arc::new(config), name).map_err(|_| Error::Transport)?;
        Socket::Tls(Box::new(rustls::StreamOwned::new(connection, tcp)))
    } else {
        Socket::Plain(tcp)
    };
    let mut config = tungstenite::protocol::WebSocketConfig::default();
    config.max_message_size = Some(1024 * 1024);
    config.max_frame_size = Some(1024 * 1024);
    tungstenite::client::client_with_config(raw, socket, Some(config))
        .map(|(socket, _)| socket)
        .map_err(|_| Error::Transport)
}

fn registration(key: Option<&str>) -> Value {
    let mut payload = json!({
        "forcePairing": false,
        "pairingType": "PROMPT",
        "manifest": {
            "manifestVersion": 1,
            "appVersion": "1.0",
            "permissions": [
                "CONTROL_AUDIO", "CONTROL_INPUT_TV", "CONTROL_INPUT_MEDIA_PLAYBACK",
                "CONTROL_MOUSE_AND_KEYBOARD", "CONTROL_POWER", "READ_POWER_STATE",
                "READ_INPUT_DEVICE_LIST", "READ_RUNNING_APPS"
            ]
        }
    });
    if let Some(key) = key {
        payload["client-key"] = json!(key);
    }
    json!({"id": "register", "type": "register", "payload": payload})
}

fn send(socket: &mut WebSocket<Socket>, value: Value) -> Result<()> {
    socket
        .send(Message::Text(value.to_string().into()))
        .map_err(|_| Error::Transport)
}

fn read(socket: &mut WebSocket<Socket>, until: Instant) -> Result<Value> {
    loop {
        let left = until
            .checked_duration_since(Instant::now())
            .ok_or(Error::Timeout)?;
        socket
            .get_ref()
            .timeout(left)
            .map_err(|_| Error::Transport)?;
        match socket.read() {
            Ok(Message::Text(text)) => {
                return serde_json::from_str(&text).map_err(|_| Error::Protocol)
            }
            Ok(Message::Ping(_)) => socket.flush().map_err(|_| Error::Transport)?,
            Ok(Message::Pong(_)) => {}
            Ok(Message::Close(_)) => return Err(Error::Transport),
            Ok(_) => return Err(Error::Protocol),
            Err(tungstenite::Error::Io(error))
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) =>
            {
                return Err(Error::Timeout)
            }
            Err(_) => return Err(Error::Transport),
        }
    }
}

fn registered(socket: &mut WebSocket<Socket>, key: &str) -> Result<()> {
    send(socket, registration(Some(key)))?;
    let until = Instant::now() + REGISTER_TIMEOUT;
    loop {
        let value = read(socket, until)?;
        if value["type"] == "error" {
            return Err(Error::Rejected);
        }
        if value["type"] == "registered" {
            return Ok(());
        }
        if value["payload"]["pairingType"].is_string() {
            return Err(Error::Unpaired);
        }
    }
}

#[derive(Clone)]
struct WebOsCredential {
    key: String,
    certificate: Vec<u8>,
}

impl WebOsCredential {
    fn parse(credential: &Credential, secure: bool) -> Result<Self> {
        let map = credential.get();
        let key = map
            .get("client_key")
            .and_then(Value::as_str)
            .filter(|key| !key.is_empty() && key.len() <= 4096)
            .ok_or(Error::Unpaired)?
            .to_owned();
        let certificate = map
            .get("certificate")
            .and_then(Value::as_str)
            .and_then(|value| BASE64.decode(value).ok())
            .ok_or(Error::Certificate)?;
        if secure && certificate.is_empty() {
            return Err(Error::Certificate);
        }
        Ok(Self { key, certificate })
    }

    fn credential(&self) -> Credential {
        Credential::new(json!({
            "client_key": self.key,
            "certificate": BASE64.encode(&self.certificate),
        }))
        .expect("a webOS key and certificate fit the credential limit")
    }
}

pub struct WebOsTv {
    socket: WebSocket<Socket>,
    pointer: Option<WebSocket<Socket>>,
    url: String,
    pin: Arc<Mutex<Vec<u8>>>,
    next: u64,
}

impl WebOsTv {
    fn request(&mut self, uri: &str, payload: Value) -> Result<Value> {
        if !uri.starts_with("ssap://") || uri.len() > 256 || uri.chars().any(char::is_control) {
            return Err(Error::Configuration);
        }
        self.next += 1;
        let id = format!("couch-{}", self.next);
        send(
            &mut self.socket,
            json!({"id": id, "type": "request", "uri": uri, "payload": payload}),
        )?;
        let until = Instant::now() + IO_TIMEOUT;
        loop {
            let value = read(&mut self.socket, until)?;
            if value["id"] != id {
                continue;
            }
            if value["type"] == "error" || value["payload"]["returnValue"] == false {
                return Err(Error::Rejected);
            }
            if value["type"] != "response" {
                return Err(Error::Protocol);
            }
            return Ok(value["payload"].clone());
        }
    }

    fn simple(&mut self, uri: &str) -> Result<()> {
        self.request(uri, json!({})).map(|_| ())
    }

    fn button(&mut self, name: &str) -> Result<()> {
        if self.pointer.is_none() {
            let response = self.request(
                "ssap://com.webos.service.networkinput/getPointerInputSocket",
                json!({}),
            )?;
            let raw = response["socketPath"].as_str().ok_or(Error::Protocol)?;
            let (pointer, host, port) = endpoint(raw)?;
            let (control, own_host, own_port) = endpoint(&self.url)?;
            if host != own_host || port != own_port || pointer.scheme() != control.scheme() {
                return Err(Error::Protocol);
            }
            self.pointer = Some(connect_socket(raw, self.pin.clone(), IO_TIMEOUT)?);
        }
        let pointer = self.pointer.as_mut().ok_or(Error::Transport)?;
        pointer
            .get_ref()
            .timeout(IO_TIMEOUT)
            .map_err(|_| Error::Transport)?;
        if pointer
            .send(Message::Text(
                format!("type:button\nname:{name}\n\n").into(),
            ))
            .is_err()
        {
            self.pointer = None;
            return Err(Error::Transport);
        }
        Ok(())
    }

    fn volume(&mut self) -> Result<Value> {
        self.request("ssap://audio/getVolume", json!({}))
    }

    fn muted(&mut self, on: bool) -> Result<()> {
        self.request("ssap://audio/setMute", json!({"mute": on}))
            .map(|_| ())
    }
}

impl DeviceClient for WebOsTv {
    type Settings = Settings;

    const KIND: &'static str = "webos";
    const LABEL: &'static str = "LG webOS TV";

    fn capabilities() -> &'static [Capability] {
        &[
            ("power-off", "Power off"),
            ("volume-up", "Volume up"),
            ("volume-down", "Volume down"),
            ("mute", "Mute"),
            ("mute-on", "Mute on"),
            ("mute-off", "Mute off"),
            ("up", "Up"),
            ("down", "Down"),
            ("left", "Left"),
            ("right", "Right"),
            ("ok", "OK"),
            ("back", "Back"),
            ("home", "Home"),
            ("menu", "Menu"),
            ("red", "Red"),
            ("green", "Green"),
            ("yellow", "Yellow"),
            ("blue", "Blue"),
            ("channel-up", "Channel up"),
            ("channel-down", "Channel down"),
            ("play", "Play"),
            ("pause", "Pause"),
            ("stop", "Stop"),
            ("rewind", "Rewind"),
            ("fast-forward", "Fast forward"),
        ]
    }

    fn connect(_settings: &Settings) -> SdkResult<Self> {
        Err(SdkError::Unpaired)
    }

    fn connect_with(settings: &Settings, credential: Option<&Credential>) -> SdkResult<Self> {
        settings.validate()?;
        let secure = settings.url.starts_with("wss://");
        let credential = WebOsCredential::parse(credential.ok_or(SdkError::Unpaired)?, secure)
            .map_err(SdkError::from)?;
        let pin = Arc::new(Mutex::new(credential.certificate));
        let mut socket =
            connect_socket(&settings.url, pin.clone(), IO_TIMEOUT).map_err(SdkError::from)?;
        registered(&mut socket, &credential.key).map_err(SdkError::from)?;
        Ok(Self {
            socket,
            pointer: None,
            url: settings.url.clone(),
            pin,
            next: 0,
        })
    }

    fn pair_start(
        settings: &Settings,
        _existing: Option<&Credential>,
    ) -> SdkResult<Box<dyn PairFlow>> {
        settings.validate()?;
        Pairing::new(settings).map(|flow| Box::new(flow) as Box<dyn PairFlow>)
    }

    fn execute(&mut self, function: &Function) -> SdkResult<()> {
        let result = match function {
            Function::PowerOff => self.simple("ssap://system/turnOff"),
            Function::VolumeUp => self.simple("ssap://audio/volumeUp"),
            Function::VolumeDown => self.simple("ssap://audio/volumeDown"),
            Function::Mute => {
                let muted = self
                    .volume()?
                    .get("mute")
                    .and_then(Value::as_bool)
                    .ok_or(Error::Protocol)?;
                self.muted(!muted)
            }
            Function::MuteOn => self.muted(true),
            Function::MuteOff => self.muted(false),
            Function::Input(id) if Self::supports_input(id) => self
                .request("ssap://tv/switchInput", json!({"inputId": id}))
                .map(|_| ()),
            Function::Up => self.button("UP"),
            Function::Down => self.button("DOWN"),
            Function::Left => self.button("LEFT"),
            Function::Right => self.button("RIGHT"),
            Function::Ok => self.button("ENTER"),
            Function::Back => self.button("BACK"),
            Function::Home => self.button("HOME"),
            Function::Menu => self.button("MENU"),
            Function::Red => self.button("RED"),
            Function::Green => self.button("GREEN"),
            Function::Yellow => self.button("YELLOW"),
            Function::Blue => self.button("BLUE"),
            Function::ChannelUp => self.simple("ssap://tv/channelUp"),
            Function::ChannelDown => self.simple("ssap://tv/channelDown"),
            Function::Play => self.simple("ssap://media.controls/play"),
            Function::Pause => self.simple("ssap://media.controls/pause"),
            Function::Stop => self.simple("ssap://media.controls/stop"),
            Function::Rewind => self.simple("ssap://media.controls/rewind"),
            Function::FastForward => self.simple("ssap://media.controls/fastForward"),
            _ => return Err(SdkError::Unsupported),
        };
        result.map_err(Into::into)
    }

    fn status(&mut self) -> SdkResult<Status> {
        let power = self
            .request(
                "ssap://com.webos.service.tvpower/power/getPowerState",
                json!({}),
            )
            .map_err(SdkError::from)?;
        let volume = self.volume().map_err(SdkError::from)?;
        let foreground = self
            .request(
                "ssap://com.webos.applicationManager/getForegroundAppInfo",
                json!({}),
            )
            .map_err(SdkError::from)?;
        let on = power["state"]
            .as_str()
            .map(|state| !matches!(state, "Power Off" | "Suspend" | "Screen Off"));
        let level = volume["volume"]
            .as_u64()
            .and_then(|value| u8::try_from(value).ok())
            .filter(|value| *value <= 100);
        let muted = volume["mute"].as_bool();
        let input = foreground["appId"]
            .as_str()
            .filter(|id| !id.is_empty() && id.len() <= 256 && !id.chars().any(char::is_control))
            .map(str::to_owned);
        Ok(Status {
            on,
            muted,
            volume: level,
            input,
            ..Status::default()
        })
    }

    fn inputs(&mut self) -> SdkResult<Vec<Selectable>> {
        let response = self
            .request("ssap://tv/getExternalInputList", json!({}))
            .map_err(SdkError::from)?;
        let rows = response["devices"].as_array().ok_or(SdkError::Protocol)?;
        let mut seen = HashSet::new();
        Ok(rows
            .iter()
            .filter_map(|row| {
                let id = row["id"].as_str().or_else(|| row["appId"].as_str())?;
                let name = row["label"].as_str().unwrap_or(id);
                (valid_input_id(id)
                    && seen.insert(id.to_owned())
                    && !name.is_empty()
                    && name.len() <= 4096
                    && !name.chars().any(char::is_control))
                .then(|| Selectable::new(id, name))
            })
            .collect())
    }

    fn supports_input(id: &str) -> bool {
        valid_input_id(id)
    }
}

struct Pairing {
    socket: Option<WebSocket<Socket>>,
    pin: Arc<Mutex<Vec<u8>>>,
    settings: Settings,
    started: Instant,
}

impl Pairing {
    fn new(settings: &Settings) -> SdkResult<Self> {
        let pin = Arc::new(Mutex::new(Vec::new()));
        let mut socket =
            connect_socket(&settings.url, pin.clone(), IO_TIMEOUT).map_err(SdkError::from)?;
        send(&mut socket, registration(None)).map_err(SdkError::from)?;
        Ok(Self {
            socket: Some(socket),
            pin,
            settings: settings.clone(),
            started: Instant::now(),
        })
    }

    fn failure(error: Error) -> PairStep {
        match error {
            Error::Timeout => PairStep::waiting(
                PairPrompt::approve_on_device().saying("Approve Couch on the LG TV"),
                PAIR_POLL_MS,
            ),
            Error::Transport => {
                PairStep::failed(PairFailure::Unreachable).because("The LG TV could not be reached")
            }
            Error::Rejected | Error::Unpaired => {
                PairStep::failed(PairFailure::Refused).because("The LG TV did not approve Couch")
            }
            Error::Certificate => PairStep::failed(PairFailure::Refused)
                .because("The LG TV certificate changed during pairing"),
            Error::Configuration => PairStep::failed(PairFailure::Unsupported),
            Error::Protocol => PairStep::failed(PairFailure::Refused)
                .because("The LG TV sent a pairing reply Couch could not use"),
        }
    }
}

impl PairFlow for Pairing {
    fn step(&mut self, _input: Option<PairInput>) -> SdkResult<PairStep> {
        if self.started.elapsed() >= PAIR_BUDGET {
            return Ok(PairStep::failed(PairFailure::TimedOut)
                .because("The LG TV was not approved before pairing closed"));
        }
        let Some(socket) = self.socket.as_mut() else {
            return Ok(PairStep::failed(PairFailure::Refused));
        };
        let value = match read(socket, Instant::now() + IO_TIMEOUT) {
            Ok(value) => value,
            Err(error) => return Ok(Self::failure(error)),
        };
        if value["type"] == "registered" {
            let key = value["payload"]["client-key"]
                .as_str()
                .filter(|key| !key.is_empty() && key.len() <= 4096)
                .ok_or(SdkError::Protocol)?;
            let certificate = self.pin.lock().map(|pin| pin.clone()).unwrap_or_default();
            if self.settings.url.starts_with("wss://") && certificate.is_empty() {
                return Ok(Self::failure(Error::Certificate));
            }
            let credential = WebOsCredential {
                key: key.to_owned(),
                certificate,
            }
            .credential();
            self.socket = None;
            return Ok(PairStep::done(credential, "Paired with LG webOS TV"));
        }
        if value["type"] == "error" {
            self.socket = None;
            return Ok(Self::failure(Error::Rejected));
        }
        Ok(PairStep::waiting(
            PairPrompt::approve_on_device().saying("Approve Couch on the LG TV"),
            PAIR_POLL_MS,
        ))
    }

    fn cancel(&mut self) {
        self.socket = None;
        if let Ok(mut pin) = self.pin.lock() {
            pin.clear();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_require_one_root_websocket_url_at_a_unicast_ip() {
        for valid in ["ws://127.0.0.1:3000/", "wss://192.0.2.10:3001/"] {
            assert!(Settings { url: valid.into() }.validate().is_ok(), "{valid}");
        }
        for invalid in [
            "",
            "192.0.2.10",
            "https://192.0.2.10/",
            "wss://example.com/",
            "wss://0.0.0.0:3001/",
            "wss://224.0.0.1:3001/",
            "wss://192.0.2.10:3001/path",
            "wss://user@192.0.2.10:3001/",
        ] {
            assert!(
                Settings {
                    url: invalid.into()
                }
                .validate()
                .is_err(),
                "{invalid}"
            );
        }
    }

    #[test]
    fn credential_round_trips_without_printing_or_accepting_an_empty_secure_pin() {
        let held = WebOsCredential {
            key: "private-key".into(),
            certificate: vec![1, 2, 3, 4],
        };
        let credential = held.credential();
        assert_eq!(format!("{credential:?}"), "Credential(..)");
        let parsed = WebOsCredential::parse(&credential, true).unwrap();
        assert_eq!(parsed.key, "private-key");
        assert_eq!(parsed.certificate, vec![1, 2, 3, 4]);
        let no_pin = WebOsCredential {
            key: "private-key".into(),
            certificate: Vec::new(),
        }
        .credential();
        assert!(matches!(
            WebOsCredential::parse(&no_pin, true),
            Err(Error::Certificate)
        ));
        assert!(WebOsCredential::parse(&no_pin, false).is_ok());
    }

    #[test]
    fn manifest_and_client_capabilities_match() {
        let manifest: couch_plugin::Manifest =
            serde_json::from_str(include_str!("../plugin.json")).unwrap();
        manifest.validate().unwrap();
        let client: Vec<_> = WebOsTv::capabilities()
            .iter()
            .map(|(id, label)| (*id, *label))
            .collect();
        let declared: Vec<_> = manifest
            .capabilities
            .iter()
            .map(|capability| (capability.id.as_str(), capability.label.as_str()))
            .collect();
        assert_eq!(client, declared);
    }
}
