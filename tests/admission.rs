//! First package-boundary admission case: a real adapter subprocess, a fake
//! loopback SSAP television, protocol-3 pairing, then commands, status,
//! inputs, apps and television selectors.

use couch_plugin::{
    testing::{self, Adapter, FakeDevice, Package},
    testing_v3::{self, PairingCase, PairingScenario},
    Credential, PairStep, Request,
};
use serde_json::{json, Value};
use std::{
    net::{TcpListener, TcpStream},
    path::Path,
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
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

#[derive(Clone, Copy, Default)]
struct FakeTvOptions {
    reject_audio_subscriptions: bool,
    cec: CecScenario,
}

#[derive(Clone, Copy, Default, PartialEq, Eq)]
enum CecScenario {
    #[default]
    None,
    DisappearsBeforeSelection,
    WrongSelectionAfterLaunch,
}

struct FakeTv {
    address: std::net::SocketAddr,
    log: Arc<Mutex<Vec<String>>>,
    stop: Arc<AtomicBool>,
    thread: Option<thread::JoinHandle<()>>,
}

impl FakeTv {
    fn start(scenario: PairingScenario) -> Self {
        Self::start_with_options(scenario, FakeTvOptions::default())
    }

    fn start_with_options(scenario: PairingScenario, options: FakeTvOptions) -> Self {
        Self::start_configured(scenario, None, options)
    }

    fn start_disconnect_after_subscription(scenario: PairingScenario, uri: &'static str) -> Self {
        Self::start_configured(scenario, Some(uri), FakeTvOptions::default())
    }

    fn start_configured(
        scenario: PairingScenario,
        disconnect_after_subscription: Option<&'static str>,
        options: FakeTvOptions,
    ) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let address = listener.local_addr().unwrap();
        let log = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let shared_log = log.clone();
        let shared_stop = stop.clone();
        let eim_calls = Arc::new(AtomicUsize::new(0));
        let shared_eim_calls = eim_calls.clone();
        let thread = thread::spawn(move || {
            let mut clients = Vec::new();
            while !shared_stop.load(Ordering::SeqCst) {
                match listener.accept() {
                    Ok((stream, _)) => {
                        stream.set_nonblocking(false).unwrap();
                        let log = shared_log.clone();
                        let eim_calls = shared_eim_calls.clone();
                        clients.push(thread::spawn(move || {
                            serve(
                                stream,
                                log,
                                scenario,
                                address,
                                disconnect_after_subscription,
                                options,
                                eim_calls,
                            )
                        }));
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
    disconnect_after_subscription: Option<&'static str>,
    options: FakeTvOptions,
    eim_calls: Arc<AtomicUsize>,
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
    let mut subscription_ids = std::collections::HashMap::<String, String>::new();
    let mut last_unique_id = "3".to_owned();
    while let Some(request) = receive(&mut socket) {
        let Some(uri) = request["uri"].as_str() else {
            continue;
        };
        log.lock().unwrap().push(uri.into());
        let is_subscription = request["type"] == "subscribe";
        if is_subscription && disconnect_after_subscription == Some(uri) {
            let _ = socket.get_mut().shutdown(std::net::Shutdown::Both);
            return;
        }
        let payload = match uri {
            "ssap://audio/getStatus" if request["type"] == "subscribe" => {
                if options.reject_audio_subscriptions {
                    json!({"returnValue": false})
                } else {
                    json!({"returnValue": true, "mute": false})
                }
            }
            "ssap://audio/getVolume" if request["type"] == "subscribe" => {
                if options.reject_audio_subscriptions {
                    json!({"returnValue": false})
                } else {
                    json!({"returnValue": true, "volumeStatus": {"volume": 37}})
                }
            }
            "ssap://audio/volumeUp" => json!({"returnValue": true}),
            "ssap://com.webos.service.networkinput/getPointerInputSocket" => json!({
                "returnValue": true,
                "socketPath": format!("ws://{address}/resources/pointer?token=fixture")
            }),
            "ssap://com.webos.service.tvpower/power/getPowerState" => {
                json!({"returnValue": true, "state": "Active"})
            }
            "ssap://audio/getVolume" if options.reject_audio_subscriptions => {
                json!({"returnValue": true, "volume": 37})
            }
            "ssap://audio/getVolume" => json!({"returnValue": true}),
            "ssap://audio/setMute" => json!({"returnValue": true}),
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
            "ssap://tv/getExternalInputList" => {
                let parent_label = match options.cec {
                    CecScenario::None => "Blu-ray",
                    _ => "Soundbar/Receiver",
                };
                json!({
                    "returnValue": true,
                    "devices": [
                        {"id": "HDMI_1", "label": "Game console"},
                        {"id": "HDMI_2", "label": parent_label}
                    ]
                })
            }
            "ssap://com.webos.service.eim/getAllInputStatus" => {
                let call = eim_calls.fetch_add(1, Ordering::SeqCst) + 1;
                let child_present = match options.cec {
                    CecScenario::None => true,
                    CecScenario::DisappearsBeforeSelection => call == 1,
                    CecScenario::WrongSelectionAfterLaunch => call <= 2,
                };
                let reported_last_unique_id = match options.cec {
                    CecScenario::WrongSelectionAfterLaunch if call >= 3 => "5",
                    _ => last_unique_id.as_str(),
                };
                let (parent_label, child_name) = match options.cec {
                    CecScenario::None => ("Blu-ray", "p3i"),
                    _ => ("Soundbar/Receiver", "Apple TV"),
                };
                let sub_list = if child_present {
                    json!([{"osdName": child_name, "uniqueId": 4, "depth": 2}])
                } else {
                    json!([])
                };
                json!({
                    "returnValue": true,
                    "devices": [{
                        "id": "HDMI_2",
                        "appId": "com.webos.app.hdmi2",
                        "label": parent_label,
                        "lastUniqueId": reported_last_unique_id,
                        "subList": sub_list
                    }]
                })
            }
            "ssap://com.webos.applicationManager/listLaunchPoints" => json!({
                "returnValue": true,
                "launchPoints": [
                    {"id": "netflix", "title": "Netflix"},
                    {"id": "com.webos.app.settings", "title": "Settings"}
                ]
            }),
            "ssap://com.webos.applicationManager/launch" => {
                last_unique_id = request["payload"]["params"]["value"]
                    .as_str()
                    .unwrap_or_default()
                    .to_owned();
                json!({"returnValue": true})
            }
            "ssap://system.launcher/launch"
            | "ssap://com.webos.service.apiadapter/audio/changeSoundOutput" => {
                json!({"returnValue": true})
            }
            _ => json!({"returnValue": false}),
        };

        if is_subscription {
            subscription_ids.insert(uri.to_owned(), request["id"].as_str().unwrap().to_owned());
        } else if uri == "ssap://audio/volumeUp" {
            // Deliver unsolicited audio updates before the ordinary command
            // response. The client must route both subscription IDs without
            // allowing either notification to satisfy the pending request.
            for (subscription_uri, notification) in [
                ("ssap://audio/getStatus", json!({"mute": true})),
                (
                    "ssap://audio/getVolume",
                    json!({"volumeStatus": {"volume": 62}}),
                ),
            ] {
                if let Some(id) = subscription_ids.get(subscription_uri) {
                    socket
                        .send(Message::Text(
                            json!({"id": id, "type": "response", "payload": notification})
                                .to_string()
                                .into(),
                        ))
                        .unwrap();
                }
            }
        } else if uri == "ssap://audio/setMute" {
            log.lock()
                .unwrap()
                .push(format!("mute:{}", request["payload"]["mute"]));
        }
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
    host.command("mute").unwrap();
    tv.wait_for("pointer:UP");
    let status = host.status().unwrap();
    assert_eq!(status.on, Some(true));
    assert_eq!(status.volume, Some(62));
    assert_eq!(status.muted, Some(false));
    assert_eq!(status.input.as_deref(), Some("HDMI_1"));
    assert_eq!(status.sound_output.as_deref(), Some("external_arc"));
    assert_eq!(status.picture_mode.as_deref(), Some("cinema"));
    let inputs = host.inputs().unwrap();
    assert_eq!(inputs[0].id, "HDMI_1");
    assert_eq!(inputs[0].name, "Game console");
    let cec_input = inputs
        .iter()
        .find(|input| input.name == "Blu-ray - p3i")
        .expect("CEC descendant is exposed as an input selector");
    host.command(&format!("input:{}", cec_input.id)).unwrap();
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
            "ssap://audio/getStatus",
            "ssap://audio/getVolume",
            "ssap://audio/volumeUp",
            "ssap://com.webos.service.networkinput/getPointerInputSocket",
            "pointer:UP",
            "ssap://audio/getVolume",
            "ssap://audio/setMute",
            "mute:false",
            "ssap://com.webos.service.tvpower/power/getPowerState",
            "ssap://audio/getVolume",
            "ssap://com.webos.applicationManager/getForegroundAppInfo",
            "ssap://com.webos.service.apiadapter/audio/getSoundOutput",
            "ssap://settings/getSystemSettings",
            "ssap://tv/getExternalInputList",
            "ssap://com.webos.service.eim/getAllInputStatus",
            "ssap://com.webos.service.eim/getAllInputStatus",
            "ssap://com.webos.applicationManager/launch",
            "ssap://com.webos.service.eim/getAllInputStatus",
            "ssap://com.webos.applicationManager/listLaunchPoints",
            "ssap://system.launcher/launch",
            "ssap://com.webos.service.apiadapter/audio/changeSoundOutput",
        ]
    );

    drop(session);
}

#[test]
fn transport_disconnect_during_audio_subscription_fails_first_operation() {
    let tv = FakeTv::start_disconnect_after_subscription(
        PairingScenario::Paired,
        "ssap://audio/getStatus",
    );
    let package = Package::new(adapter());
    let mut pairing = package.host();
    let (session, first) = pairing.pair_start(tv.settings(), None).unwrap();
    assert!(matches!(first, PairStep::Waiting { .. }));
    let paired = pairing.pair_continue(None).unwrap();
    let PairStep::Done { credential, .. } = paired else {
        panic!("pairing did not finish: {paired:?}");
    };

    let mut host = package.host();
    host.configure_with(tv.settings(), Some(&credential))
        .unwrap();
    assert!(host.command("volume-up").is_err());
    assert!(tv
        .requests()
        .iter()
        .any(|request| request == "ssap://audio/getStatus"));

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

fn pair_tv(tv: &FakeTv) -> (Package, Credential) {
    let package = Package::new(adapter());
    let mut pairing = package.host();
    let (_session, first) = pairing.pair_start(tv.settings(), None).unwrap();
    assert!(matches!(first, PairStep::Waiting { .. }));
    let paired = pairing.pair_continue(None).unwrap();
    let PairStep::Done { credential, .. } = paired else {
        panic!("pairing did not finish: {paired:?}");
    };
    (package, credential)
}

#[test]
fn rejected_audio_subscriptions_do_not_prevent_normal_tv_operation() {
    let tv = FakeTv::start_with_options(
        PairingScenario::Paired,
        FakeTvOptions {
            reject_audio_subscriptions: true,
            ..FakeTvOptions::default()
        },
    );
    let (package, credential) = pair_tv(&tv);
    let mut host = package.host();
    host.configure_with(tv.settings(), Some(&credential))
        .unwrap();
    host.command("volume-up").unwrap();
    let status = host.status().unwrap();

    assert_eq!(status.on, Some(true));
    assert_eq!(status.volume, Some(37));
    assert!(status.muted.is_none());
    let requests = tv.requests();
    assert_eq!(
        requests
            .iter()
            .filter(|uri| *uri == "ssap://audio/getStatus")
            .count(),
        1
    );
    assert_eq!(
        requests
            .iter()
            .filter(|uri| *uri == "ssap://audio/getVolume")
            .count(),
        2
    );
    assert!(requests.iter().any(|uri| uri == "ssap://audio/volumeUp"));
}

#[test]
fn stale_cec_child_is_not_launched_after_disappearing_from_tv_probe() {
    let tv = FakeTv::start_with_options(
        PairingScenario::Paired,
        FakeTvOptions {
            cec: CecScenario::DisappearsBeforeSelection,
            ..FakeTvOptions::default()
        },
    );
    let (package, credential) = pair_tv(&tv);
    let mut host = package.host();
    host.configure_with(tv.settings(), Some(&credential))
        .unwrap();
    let inputs = host.inputs().unwrap();
    let child = inputs
        .iter()
        .find(|input| input.name == "Soundbar/Receiver - Apple TV")
        .unwrap();

    assert!(host.command(&format!("input:{}", child.id)).is_err());
    let requests = tv.requests();
    assert_eq!(
        requests
            .iter()
            .filter(|uri| *uri == "ssap://com.webos.applicationManager/launch")
            .count(),
        0
    );
    assert_eq!(
        requests
            .iter()
            .filter(|uri| *uri == "ssap://com.webos.service.eim/getAllInputStatus")
            .count(),
        2
    );
}

#[test]
fn cec_selection_is_rejected_when_tv_reports_a_different_device_after_launch() {
    let tv = FakeTv::start_with_options(
        PairingScenario::Paired,
        FakeTvOptions {
            cec: CecScenario::WrongSelectionAfterLaunch,
            ..FakeTvOptions::default()
        },
    );
    let (package, credential) = pair_tv(&tv);
    let mut host = package.host();
    host.configure_with(tv.settings(), Some(&credential))
        .unwrap();
    let inputs = host.inputs().unwrap();
    let child = inputs
        .iter()
        .find(|input| input.name == "Soundbar/Receiver - Apple TV")
        .unwrap();

    assert!(host.command(&format!("input:{}", child.id)).is_err());
    let requests = tv.requests();
    assert_eq!(
        requests
            .iter()
            .filter(|uri| *uri == "ssap://com.webos.applicationManager/launch")
            .count(),
        1
    );
    assert_eq!(
        requests
            .iter()
            .filter(|uri| *uri == "ssap://com.webos.service.eim/getAllInputStatus")
            .count(),
        3
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
