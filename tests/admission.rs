//! First package-boundary admission case: a real adapter subprocess, a fake
//! loopback SSAP television, protocol-3 pairing, then commands, status,
//! inputs, apps and television selectors.

use couch_plugin::{
    testing::{self, Adapter, FakeDevice, Package},
    testing_v3::{self, PairingCase, PairingScenario},
    PairStep, Request,
};
use serde_json::{json, Value};
use std::{
    net::{TcpListener, TcpStream},
    path::Path,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    thread,
    time::Duration,
};
use tungstenite::{Message, WebSocket};

fn adapter() -> Adapter<'static> {
    Adapter {
        binary: Path::new(env!("CARGO_BIN_EXE_couch-plugin-webos")),
        manifest_json: include_str!("../plugin.json"),
    }
}

struct FakeTv {
    address: std::net::SocketAddr,
    log: Arc<Mutex<Vec<String>>>,
    stop: Arc<AtomicBool>,
    thread: Option<thread::JoinHandle<()>>,
}

impl FakeTv {
    fn start(scenario: PairingScenario) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let address = listener.local_addr().unwrap();
        let log = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let shared_log = log.clone();
        let shared_stop = stop.clone();
        let thread = thread::spawn(move || {
            let mut clients = Vec::new();
            while !shared_stop.load(Ordering::SeqCst) {
                match listener.accept() {
                    Ok((stream, _)) => {
                        stream.set_nonblocking(false).unwrap();
                        let log = shared_log.clone();
                        clients.push(thread::spawn(move || serve(stream, log, scenario, address)));
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(5));
                    }
                    Err(_) => break,
                }
            }
            for client in clients {
                let _ = client.join();
            }
        });
        Self {
            address,
            log,
            stop,
            thread: Some(thread),
        }
    }
}

impl FakeDevice for FakeTv {
    fn settings(&self) -> Value {
        json!({"url": format!("ws://{}/", self.address)})
    }

    fn requests(&self) -> Vec<String> {
        self.log.lock().unwrap().clone()
    }
}

impl FakeTv {
    fn wait_for(&self, expected: &str) {
        for _ in 0..100 {
            if self.requests().iter().any(|request| request == expected) {
                return;
            }
            thread::sleep(Duration::from_millis(10));
        }
        panic!("missing {expected:?}; requests={:?}", self.requests());
    }
}

impl Drop for FakeTv {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        let _ = TcpStream::connect(self.address);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn receive(socket: &mut WebSocket<TcpStream>) -> Option<Value> {
    let text = socket.read().ok()?.into_text().ok()?;
    serde_json::from_str(&text).ok()
}

fn reply(socket: &mut WebSocket<TcpStream>, request: &Value, payload: Value) {
    socket
        .send(Message::Text(
            json!({"id": request["id"], "type": "response", "payload": payload})
                .to_string()
                .into(),
        ))
        .unwrap();
}

fn serve(
    stream: TcpStream,
    log: Arc<Mutex<Vec<String>>>,
    scenario: PairingScenario,
    address: std::net::SocketAddr,
) {
    let Ok(mut socket) = tungstenite::accept(stream) else {
        return;
    };
    let register: Value = match socket.read() {
        Ok(message) => match message.into_text() {
            Ok(text) => {
                if let Some(name) = text.lines().find_map(|line| line.strip_prefix("name:")) {
                    log.lock().unwrap().push(format!("pointer:{name}"));
                    return;
                }
                match serde_json::from_str(&text) {
                    Ok(value) => value,
                    Err(error) => {
                        log.lock()
                            .unwrap()
                            .push(format!("register-json:{error}:{text}"));
                        return;
                    }
                }
            }
            Err(error) => {
                log.lock().unwrap().push(format!("register-text:{error}"));
                return;
            }
        },
        Err(error) => {
            log.lock().unwrap().push(format!("register-read:{error}"));
            return;
        }
    };
    if register["type"] != "register" {
        return;
    }
    log.lock().unwrap().push("register".into());
    if register["payload"]["client-key"].is_null() {
        reply(&mut socket, &register, json!({"pairingType": "PROMPT"}));
        match scenario {
            PairingScenario::Refused => {
                socket
                    .send(Message::Text(
                        json!({"type": "error", "payload": {"returnValue": false}})
                            .to_string()
                            .into(),
                    ))
                    .unwrap();
                return;
            }
            PairingScenario::TimedOut | PairingScenario::Cancelled => {
                while receive(&mut socket).is_some() {}
                return;
            }
            PairingScenario::Paired => {}
        }
    }
    socket
        .send(Message::Text(
            json!({"type": "registered", "payload": {"client-key": "fixture-key"}})
                .to_string()
                .into(),
        ))
        .unwrap();
    while let Some(request) = receive(&mut socket) {
        let Some(uri) = request["uri"].as_str() else {
            continue;
        };
        log.lock().unwrap().push(uri.into());
        let payload = match uri {
            "ssap://audio/volumeUp" => json!({"returnValue": true}),
            "ssap://com.webos.service.networkinput/getPointerInputSocket" => json!({
                "returnValue": true,
                "socketPath": format!("ws://{address}/resources/pointer?token=fixture")
            }),
            "ssap://com.webos.service.tvpower/power/getPowerState" => {
                json!({"returnValue": true, "state": "Active"})
            }
            "ssap://audio/getVolume" => {
                json!({"returnValue": true, "volume": 37, "mute": false})
            }
            "ssap://com.webos.applicationManager/getForegroundAppInfo" => {
                json!({"returnValue": true, "appId": "HDMI_1", "appName": "Game console"})
            }
            "ssap://com.webos.service.apiadapter/audio/getSoundOutput" => {
                json!({"returnValue": true, "soundOutput": "external_arc"})
            }
            "ssap://settings/getSystemSettings" => json!({
                "returnValue": true,
                "settings": {"pictureMode": "cinema"}
            }),
            "ssap://tv/getExternalInputList" => json!({
                "returnValue": true,
                "devices": [
                    {"id": "HDMI_1", "label": "Game console"},
                    {"id": "HDMI_2", "label": "Blu-ray"}
                ]
            }),
            "ssap://com.webos.applicationManager/listLaunchPoints" => json!({
                "returnValue": true,
                "launchPoints": [
                    {"id": "netflix", "title": "Netflix"},
                    {"id": "com.webos.app.settings", "title": "Settings"}
                ]
            }),
            "ssap://system.launcher/launch"
            | "ssap://com.webos.service.apiadapter/audio/changeSoundOutput" => {
                json!({"returnValue": true})
            }
            _ => json!({"returnValue": false}),
        };
        reply(&mut socket, &request, payload);
    }
}

#[test]
fn pairs_then_controls_a_fake_tv_through_the_package_process() {
    let tv = FakeTv::start(PairingScenario::Paired);
    let package = Package::new(adapter());
    let mut pairing = package.host();
    let (session, first) = pairing.pair_start(tv.settings(), None).unwrap();
    assert!(
        matches!(first, PairStep::Waiting { .. }),
        "{first:?}; requests={:?}",
        tv.requests()
    );
    let paired = pairing.pair_continue(None).unwrap();
    let PairStep::Done { credential, .. } = paired else {
        panic!("pairing did not finish: {paired:?}");
    };

    let mut host = package.host();
    host.configure_with(tv.settings(), Some(&credential))
        .unwrap();
    host.command("volume-up").unwrap();
    host.command("up").unwrap();
    tv.wait_for("pointer:UP");
    let status = host.status().unwrap();
    assert_eq!(status.on, Some(true));
    assert_eq!(status.volume, Some(37));
    assert_eq!(status.muted, Some(false));
    assert_eq!(status.input.as_deref(), Some("HDMI_1"));
    assert_eq!(status.sound_output.as_deref(), Some("external_arc"));
    assert_eq!(status.picture_mode.as_deref(), Some("cinema"));
    let inputs = host.inputs().unwrap();
    assert_eq!(inputs[0].id, "HDMI_1");
    assert_eq!(inputs[0].name, "Game console");
    let apps = host.apps().unwrap();
    assert_eq!(apps[0].name, "Netflix");
    host.command("app:netflix").unwrap();
    host.command("x:sound-tv-speaker").unwrap();
    assert_eq!(host.pair_session(), None);
    assert!(host.is_alive());
    assert_eq!(
        tv.requests(),
        [
            "register",
            "register",
            "ssap://audio/volumeUp",
            "ssap://com.webos.service.networkinput/getPointerInputSocket",
            "pointer:UP",
            "ssap://com.webos.service.tvpower/power/getPowerState",
            "ssap://audio/getVolume",
            "ssap://com.webos.applicationManager/getForegroundAppInfo",
            "ssap://com.webos.service.apiadapter/audio/getSoundOutput",
            "ssap://settings/getSystemSettings",
            "ssap://tv/getExternalInputList",
            "ssap://com.webos.applicationManager/listLaunchPoints",
            "ssap://system.launcher/launch",
            "ssap://com.webos.service.apiadapter/audio/changeSoundOutput",
        ]
    );

    // Keep the required package-startup test shape visible from the first
    // commit; full conformance/failure/timeout/spike fixtures are next.
    drop(testing::Package::new(adapter()));
    drop(session);
}

#[test]
fn every_pairing_scenario_passes_the_shared_admission_harness() {
    testing_v3::pairing(
        adapter(),
        PairingCase {
            device: |scenario| Some(Box::new(FakeTv::start(scenario)) as Box<dyn FakeDevice>),
            settings: |device, _| device.settings(),
            code: "unused",
            after: Request::status(),
        },
    );
}

#[test]
fn concurrent_package_startup_is_offline_and_race_free() {
    std::thread::scope(|scope| {
        for _ in 0..8 {
            scope.spawn(|| {
                let package = testing::Package::new(adapter());
                let mut host = package.host();
                host.configure(json!({"url": "wss://192.0.2.1:3001/"}))
                    .unwrap();
            });
        }
    });
}
