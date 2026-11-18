//! Estado do sistema para a barra: rede, bateria, volume e brilho.
//!
//! - **Rede**: NetworkManager (D-Bus do sistema), com força do sinal quando Wi-Fi.
//! - **Bateria**: UPower `DisplayDevice`; some quando não há bateria.
//! - **Volume**: `wpctl` (PipeWire), com fallback para `pactl`.
//! - **Brilho**: `/sys/class/backlight`, escrita via `logind.SetBrightness`
//!   (sem root). Sem backlight, o módulo fica indisponível.

use std::{collections::HashMap, fs, process::Stdio, time::Duration};

use common::{BrightnessAction, IpcMessage, VolumeAction};
use tokio::{process::Command, sync::mpsc, time::interval};
use tracing::{debug, warn};
use zbus::{
    wire::OwnedObjectPath, zvariant::OwnedValue, Connection, Proxy,
};

const NM_NAME: &str = "org.freedesktop.NetworkManager";
const NM_PATH: &str = "/org/freedesktop/NetworkManager";
const NM_IFACE: &str = "org.freedesktop.NetworkManager";
const LOGIND_NAME: &str = "org.freedesktop.login1";
const LOGIND_PATH: &str = "/org/freedesktop/login1";
const LOGIND_MANAGER: &str = "org.freedesktop.login1.Manager";
const LOGIND_SESSION: &str = "org.freedesktop.login1.Session";
const UPOWER_DEVICE: &str = "org.freedesktop.UPower.Device";
const UPOWER_DISPLAY_PATH: &str = "/org/freedesktop/UPower/devices/DisplayDevice";
const POWER_PROFILES_NAME: &str = "org.freedesktop.UPower.PowerProfiles";
const POWER_PROFILES_PATH: &str = "/org/freedesktop/UPower/PowerProfiles";
const POWER_PROFILES_IFACE: &str = "org.freedesktop.UPower.PowerProfiles";
const BLUEZ_NAME: &str = "org.bluez";
const BLUEZ_ADAPTER: &str = "org.bluez.Adapter1";

pub enum SystemCommand {
    Volume { action: VolumeAction, value: u8 },
    Brightness { action: BrightnessAction, value: u8 },
    SetWifi { enabled: bool },
    SetBluetooth { enabled: bool },
    SetPowerProfile { profile: String },
    Refresh,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum AudioBackend {
    Wpctl,
    Pactl,
    None,
}

pub async fn run(ipc: mpsc::Sender<IpcMessage>, mut commands: mpsc::Receiver<SystemCommand>) {
    // Rede, UPower e logind vivem no barramento de sistema; volume/brilho são
    // locais (processos e sysfs).
    let system = zbus::Connection::system().await.ok();
    if system.is_none() {
        warn!("barramento de sistema indisponível; rede/bateria/brilho ficam ocultos");
    }
    let audio = detect_audio().await;
    let mut last_network = String::new();
    let mut last_battery = String::new();
    let mut last_volume = String::new();
    let mut last_brightness = String::new();
    let mut last_bluetooth = String::new();
    let mut last_profiles = String::new();
    // O primeiro sample de volume/brilho só popula o estado; o OSD aparece
    // a partir da primeira mudança real.
    let mut volume_seen = false;
    let mut brightness_seen = false;
    let mut bluetooth_next: u64 = 0;
    let mut ticker = interval(Duration::from_secs(1));
    let mut tick: u64 = 0;

    loop {
        tokio::select! {
            _ = ticker.tick() => {
                sample_volume(&audio, &ipc, &mut last_volume, volume_seen).await;
                sample_brightness(&ipc, &mut last_brightness, brightness_seen).await;
                volume_seen = true;
                brightness_seen = true;
                if tick % 3 == 0 {
                    sample_network(system.as_ref(), &ipc, &mut last_network).await;
                    sample_battery(system.as_ref(), &ipc, &mut last_battery).await;
                    sample_profiles(system.as_ref(), &ipc, &mut last_profiles).await;
                }
                if tick >= bluetooth_next {
                    let snapshot = read_bluetooth(system.as_ref()).await;
                    // Sem adaptador, tenta de novo só a cada minuto.
                    bluetooth_next = tick + if snapshot.0 { 3 } else { 60 };
                    let key = format!("{}|{}|{}", snapshot.0, snapshot.1, snapshot.2);
                    if key != last_bluetooth {
                        last_bluetooth = key;
                        let _ = ipc
                            .send(IpcMessage::BluetoothState {
                                available: snapshot.0,
                                enabled: snapshot.1,
                                name: snapshot.2,
                            })
                            .await;
                    }
                }
                tick = tick.wrapping_add(1);
            }
            command = commands.recv() => {
                let Some(command) = command else {
                    break;
                };
                match command {
                    SystemCommand::Refresh => {
                        last_network.clear();
                        last_battery.clear();
                        last_volume.clear();
                        last_brightness.clear();
                        last_bluetooth.clear();
                        last_profiles.clear();
                        sample_network(system.as_ref(), &ipc, &mut last_network).await;
                        sample_battery(system.as_ref(), &ipc, &mut last_battery).await;
                        sample_profiles(system.as_ref(), &ipc, &mut last_profiles).await;
                        sample_volume(&audio, &ipc, &mut last_volume, false).await;
                        sample_brightness(&ipc, &mut last_brightness, false).await;
                    }
                    SystemCommand::Volume { action, value } => {
                        apply_volume(audio, action, value).await;
                        last_volume.clear();
                        sample_volume(&audio, &ipc, &mut last_volume, volume_seen).await;
                    }
                    SystemCommand::Brightness { action, value } => {
                        apply_brightness(system.as_ref(), action, value).await;
                        last_brightness.clear();
                        sample_brightness(&ipc, &mut last_brightness, brightness_seen).await;
                    }
                    SystemCommand::SetWifi { enabled } => {
                        apply_wifi(system.as_ref(), enabled).await;
                        last_network.clear();
                        sample_network(system.as_ref(), &ipc, &mut last_network).await;
                    }
                    SystemCommand::SetBluetooth { enabled } => {
                        apply_bluetooth(system.as_ref(), enabled).await;
                        last_bluetooth.clear();
                        bluetooth_next = 0;
                    }
                    SystemCommand::SetPowerProfile { profile } => {
                        apply_power_profile(system.as_ref(), &profile).await;
                        last_profiles.clear();
                        sample_profiles(system.as_ref(), &ipc, &mut last_profiles).await;
                    }
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Volume
// ---------------------------------------------------------------------------

async fn detect_audio() -> AudioBackend {
    if command_ok("wpctl", &["get-volume", "@DEFAULT_AUDIO_SINK@"]).await {
        AudioBackend::Wpctl
    } else if command_ok("pactl", &["get-sink-volume", "@DEFAULT_SINK@"]).await {
        AudioBackend::Pactl
    } else {
        AudioBackend::None
    }
}

async fn command_ok(program: &str, args: &[&str]) -> bool {
    Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .await
        .map(|output| output.status.success())
        .unwrap_or(false)
}

async fn read_volume(backend: AudioBackend) -> (bool, u8, bool) {
    match backend {
        AudioBackend::Wpctl => {
            let Ok(output) = Command::new("wpctl")
                .args(["get-volume", "@DEFAULT_AUDIO_SINK@"])
                .output()
                .await
            else {
                return (false, 0, false);
            };
            if !output.status.success() {
                return (false, 0, false);
            }
            let (percent, muted) = parse_wpctl(&String::from_utf8_lossy(&output.stdout));
            (true, percent, muted)
        }
        AudioBackend::Pactl => {
            let Ok(output) = Command::new("pactl")
                .args(["get-sink-volume", "@DEFAULT_SINK@"])
                .output()
                .await
            else {
                return (false, 0, false);
            };
            if !output.status.success() {
                return (false, 0, false);
            }
            let percent = parse_pactl_volume(&String::from_utf8_lossy(&output.stdout)).unwrap_or(0);
            let muted = Command::new("pactl")
                .args(["get-sink-mute", "@DEFAULT_SINK@"])
                .output()
                .await
                .map(|output| String::from_utf8_lossy(&output.stdout).contains("yes"))
                .unwrap_or(false);
            (true, percent, muted)
        }
        AudioBackend::None => (false, 0, false),
    }
}

async fn apply_volume(backend: AudioBackend, action: VolumeAction, value: u8) {
    let value = value.min(100);
    let args: Vec<String> = match (backend, action) {
        (AudioBackend::Wpctl, VolumeAction::Up) => {
            vec!["set-volume".into(), "@DEFAULT_AUDIO_SINK@".into(), "5%+".into()]
        }
        (AudioBackend::Wpctl, VolumeAction::Down) => {
            vec!["set-volume".into(), "@DEFAULT_AUDIO_SINK@".into(), "5%-".into()]
        }
        (AudioBackend::Wpctl, VolumeAction::Mute) => {
            vec!["set-mute".into(), "@DEFAULT_AUDIO_SINK@".into(), "toggle".into()]
        }
        (AudioBackend::Wpctl, VolumeAction::Set) => vec![
            "set-volume".into(),
            "@DEFAULT_AUDIO_SINK@".into(),
            format!("{value}%"),
        ],
        (AudioBackend::Pactl, VolumeAction::Up) => {
            vec!["set-sink-volume".into(), "@DEFAULT_SINK@".into(), "+5%".into()]
        }
        (AudioBackend::Pactl, VolumeAction::Down) => {
            vec!["set-sink-volume".into(), "@DEFAULT_SINK@".into(), "-5%".into()]
        }
        (AudioBackend::Pactl, VolumeAction::Mute) => {
            vec!["set-sink-mute".into(), "@DEFAULT_SINK@".into(), "toggle".into()]
        }
        (AudioBackend::Pactl, VolumeAction::Set) => vec![
            "set-sink-volume".into(),
            "@DEFAULT_SINK@".into(),
            format!("{value}%"),
        ],
        (AudioBackend::None, _) => return,
    };

    let program = if backend == AudioBackend::Wpctl {
        "wpctl"
    } else {
        "pactl"
    };
    if let Err(error) = Command::new(program)
        .args(&args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .await
    {
        warn!(%error, program, "falha ao ajustar volume");
    }
}

async fn sample_volume(
    backend: &AudioBackend,
    ipc: &mpsc::Sender<IpcMessage>,
    last: &mut String,
    announce: bool,
) {
    let (available, percent, muted) = read_volume(*backend).await;
    let key = format!("{available}|{percent}|{muted}");
    if key != *last {
        *last = key;
        let _ = ipc
            .send(IpcMessage::VolumeState {
                available,
                percent,
                muted,
            })
            .await;
        if announce && available {
            let _ = ipc
                .send(IpcMessage::Osd {
                    kind: "volume".to_owned(),
                    percent,
                    muted,
                })
                .await;
        }
    }
}

/// `wpctl get-volume` imprime `Volume: 0.70` (ou com `[MUTED]`).
fn parse_wpctl(text: &str) -> (u8, bool) {
    let muted = text.contains("[MUTED]");
    let percent = text
        .split_whitespace()
        .find_map(|token| token.parse::<f64>().ok())
        .map(|value| (value * 100.0).round().clamp(0.0, 100.0) as u8)
        .unwrap_or(0);
    (percent, muted)
}

/// `pactl get-sink-volume` imprime `... / 70% / ...`; usa a primeira %.
fn parse_pactl_volume(text: &str) -> Option<u8> {
    let percent = text.find('%')?;
    let before = &text[..percent];
    let digits = before
        .rsplit(|character: char| !character.is_ascii_digit())
        .next()
        .unwrap_or("");
    digits.parse::<u8>().ok().map(|value| value.min(100))
}

// ---------------------------------------------------------------------------
// Brilho
// ---------------------------------------------------------------------------

fn backlight() -> Option<(String, u32, u32)> {
    let entry = fs::read_dir("/sys/class/backlight").ok()?.next()?.ok()?;
    let name = entry.file_name().to_string_lossy().to_string();
    let current = fs::read_to_string(entry.path().join("brightness"))
        .ok()?
        .trim()
        .parse::<u32>()
        .ok()?;
    let max = fs::read_to_string(entry.path().join("max_brightness"))
        .ok()?
        .trim()
        .parse::<u32>()
        .ok()?;
    Some((name, current, max))
}

async fn sample_brightness(ipc: &mpsc::Sender<IpcMessage>, last: &mut String, announce: bool) {
    let (available, percent) = match backlight() {
        Some((_, current, max)) if max > 0 => {
            (true, ((current * 100) / max).min(100) as u8)
        }
        _ => (false, 0),
    };
    let key = format!("{available}|{percent}");
    if key != *last {
        *last = key;
        let _ = ipc
            .send(IpcMessage::BrightnessState {
                available,
                percent,
            })
            .await;
        if announce && available {
            let _ = ipc
                .send(IpcMessage::Osd {
                    kind: "brightness".to_owned(),
                    percent,
                    muted: false,
                })
                .await;
        }
    }
}

async fn apply_brightness(
    connection: Option<&Connection>,
    action: BrightnessAction,
    value: u8,
) {
    let Some(connection) = connection else {
        return;
    };
    let Some((name, current, max)) = backlight() else {
        return;
    };
    let current_percent = ((current * 100) / max.max(1)) as i32;
    let target = match action {
        BrightnessAction::Up => current_percent + 5,
        BrightnessAction::Down => current_percent - 5,
        BrightnessAction::Set => value as i32,
    }
    .clamp(5, 100) as u32;
    let raw = (target * max / 100).max(1);

    let session = async {
        let manager = Proxy::new_owned(connection.clone(), LOGIND_NAME, LOGIND_PATH, LOGIND_MANAGER)
            .await
            .ok()?;
        let session: OwnedObjectPath = manager
            .call("GetSessionByPID", &(std::process::id(),))
            .await
            .ok()?;
        Some(session)
    }
    .await;
    let Some(session) = session else {
        debug!("logind não encontrou a sessão para SetBrightness");
        return;
    };

    let Ok(proxy) = Proxy::new_owned(
        connection.clone(),
        LOGIND_NAME,
        session.as_str().to_owned(),
        LOGIND_SESSION,
    )
    .await
    else {
        return;
    };
    if let Err(error) = proxy
        .call::<_, _, ()>("SetBrightness", &("backlight", name.as_str(), raw))
        .await
    {
        debug!(%error, "SetBrightness falhou");
    }
}

// ---------------------------------------------------------------------------
// Rede (NetworkManager)
// ---------------------------------------------------------------------------

struct NetworkSnapshot {
    connected: bool,
    kind: String,
    ssid: String,
    strength: u8,
    wifi_enabled: bool,
    networking_enabled: bool,
}

async fn read_network(connection: Option<&Connection>) -> NetworkSnapshot {
    let offline = NetworkSnapshot {
        connected: false,
        kind: "offline".to_owned(),
        ssid: String::new(),
        strength: 0,
        wifi_enabled: false,
        networking_enabled: false,
    };
    let Some(connection) = connection else {
        return offline;
    };
    let Ok(proxy) = Proxy::new_owned(connection.clone(), NM_NAME, NM_PATH, NM_IFACE).await else {
        return offline;
    };
    let wifi_enabled = proxy
        .get_property::<bool>("WirelessEnabled")
        .await
        .unwrap_or(false);
    let networking_enabled = proxy
        .get_property::<bool>("NetworkingEnabled")
        .await
        .unwrap_or(false);
    let state: u32 = proxy.get_property("State").await.unwrap_or(0);
    if state < 40 {
        return NetworkSnapshot {
            wifi_enabled,
            networking_enabled,
            ..offline
        };
    }

    let active: Vec<OwnedObjectPath> = proxy
        .get_property("ActiveConnections")
        .await
        .unwrap_or_default();
    let mut chosen: Option<(u8, String, String, String)> = None;
    for path in active {
        let Ok(active_proxy) = Proxy::new_owned(
            connection.clone(),
            NM_NAME,
            path.as_str().to_owned(),
            "org.freedesktop.NetworkManager.Connection.Active",
        )
        .await
        else {
            continue;
        };
        let id: String = active_proxy.get_property("Id").await.unwrap_or_default();
        let kind_raw: String = active_proxy.get_property("Type").await.unwrap_or_default();
        if kind_raw.contains("loopback") {
            continue;
        }
        let kind = classify(&kind_raw);
        let priority = match kind.as_str() {
            "wifi" => 3,
            "ethernet" | "vpn" => 2,
            "bridge" => 1,
            _ => 0,
        };
        if chosen
            .as_ref()
            .map(|(best, _, _, _)| priority > *best)
            .unwrap_or(true)
        {
            chosen = Some((priority, kind, id, path.as_str().to_owned()));
        }
    }

    let Some((_, kind, ssid, path)) = chosen else {
        return NetworkSnapshot {
            connected: true,
            wifi_enabled,
            networking_enabled,
            ..offline
        };
    };
    let strength = if kind == "wifi" {
        wifi_strength(connection, &path).await.unwrap_or(0)
    } else {
        100
    };
    NetworkSnapshot {
        connected: true,
        kind,
        ssid,
        strength,
        wifi_enabled,
        networking_enabled,
    }
}

fn classify(kind: &str) -> String {
    if kind.contains("wireless") {
        "wifi".to_owned()
    } else if kind.contains("ethernet") {
        "ethernet".to_owned()
    } else if kind.contains("vpn") || kind.contains("wireguard") {
        "vpn".to_owned()
    } else if kind.contains("bridge") {
        "bridge".to_owned()
    } else {
        "other".to_owned()
    }
}

async fn wifi_strength(connection: &Connection, active_path: &str) -> Option<u8> {
    let active = Proxy::new_owned(
        connection.clone(),
        NM_NAME,
        active_path.to_owned(),
        "org.freedesktop.NetworkManager.Connection.Active",
    )
    .await
    .ok()?;
    let devices: Vec<OwnedObjectPath> = active.get_property("Devices").await.ok()?;
    for device in devices {
        let Ok(wireless) = Proxy::new_owned(
            connection.clone(),
            NM_NAME,
            device.as_str().to_owned(),
            "org.freedesktop.NetworkManager.Device.Wireless",
        )
        .await
        else {
            continue;
        };
        let Ok(ap) = wireless
            .get_property::<OwnedObjectPath>("ActiveAccessPoint")
            .await
        else {
            continue;
        };
        if ap.as_str().len() <= 1 {
            continue;
        }
        let Ok(ap_proxy) = Proxy::new_owned(
            connection.clone(),
            NM_NAME,
            ap.as_str().to_owned(),
            "org.freedesktop.NetworkManager.AccessPoint",
        )
        .await
        else {
            continue;
        };
        if let Ok(strength) = ap_proxy.get_property::<u8>("Strength").await {
            return Some(strength);
        }
    }
    None
}

async fn sample_network(
    connection: Option<&Connection>,
    ipc: &mpsc::Sender<IpcMessage>,
    last: &mut String,
) {
    let snapshot = read_network(connection).await;
    let key = format!(
        "{}|{}|{}|{}|{}|{}",
        snapshot.connected,
        snapshot.kind,
        snapshot.ssid,
        snapshot.strength,
        snapshot.wifi_enabled,
        snapshot.networking_enabled
    );
    if key != *last {
        *last = key;
        let _ = ipc
            .send(IpcMessage::NetworkState {
                connected: snapshot.connected,
                kind: snapshot.kind,
                ssid: snapshot.ssid,
                strength: snapshot.strength,
                wifi_enabled: snapshot.wifi_enabled,
                networking_enabled: snapshot.networking_enabled,
            })
            .await;
    }
}

async fn apply_wifi(connection: Option<&Connection>, enabled: bool) {
    let Some(connection) = connection else {
        return;
    };
    let Ok(proxy) = Proxy::new_owned(connection.clone(), NM_NAME, NM_PATH, NM_IFACE).await else {
        return;
    };
    if enabled {
        // Ligar o rádio com o NetworkManager desligado não faria efeito.
        if let Err(error) = proxy.set_property("NetworkingEnabled", true).await {
            debug!(%error, "NetworkingEnabled recusado");
        }
    }
    if let Err(error) = proxy.set_property("WirelessEnabled", enabled).await {
        warn!(%error, enabled, "falha ao alternar o Wi-Fi");
    }
}

// ---------------------------------------------------------------------------
// Bluetooth (BlueZ)
// ---------------------------------------------------------------------------

fn adapter_path(index: u8) -> String {
    format!("/org/bluez/hci{index}")
}

/// Evita acordar/ativar o BlueZ (e o spam de avisos do zbus) quando o
/// serviço não está rodando.
async fn bluez_available(connection: &Connection) -> bool {
    let Ok(bus) = zbus::fdo::DBusProxy::new(connection).await else {
        return false;
    };
    let Ok(name) = zbus::names::BusName::try_from(BLUEZ_NAME) else {
        return false;
    };
    bus.name_has_owner(name).await.unwrap_or(false)
}

async fn find_adapter(connection: &Connection) -> Option<Proxy<'static>> {
    if !bluez_available(connection).await {
        return None;
    }
    for index in 0..4 {
        let Ok(proxy) = Proxy::new_owned(
            connection.clone(),
            BLUEZ_NAME,
            adapter_path(index),
            BLUEZ_ADAPTER,
        )
        .await
        else {
            continue;
        };
        if proxy.get_property::<bool>("Powered").await.is_ok() {
            return Some(proxy);
        }
    }
    None
}

async fn read_bluetooth(connection: Option<&Connection>) -> (bool, bool, String) {
    let Some(connection) = connection else {
        return (false, false, String::new());
    };
    let Some(adapter) = find_adapter(connection).await else {
        return (false, false, String::new());
    };
    let enabled = adapter.get_property::<bool>("Powered").await.unwrap_or(false);
    let name = match adapter.get_property::<String>("Alias").await {
        Ok(name) => name,
        Err(_) => adapter
            .get_property::<String>("Address")
            .await
            .unwrap_or_default(),
    };
    (true, enabled, name)
}

async fn apply_bluetooth(connection: Option<&Connection>, enabled: bool) {
    let Some(connection) = connection else {
        return;
    };
    let Some(adapter) = find_adapter(connection).await else {
        warn!("sem adaptador Bluetooth para alternar");
        return;
    };
    if let Err(error) = adapter.set_property("Powered", enabled).await {
        warn!(%error, enabled, "falha ao alternar o Bluetooth");
    }
}

// ---------------------------------------------------------------------------
// Perfil de energia (power-profiles-daemon)
// ---------------------------------------------------------------------------

async fn read_profiles(connection: Option<&Connection>) -> (bool, String, Vec<String>) {
    let Some(connection) = connection else {
        return (false, String::new(), Vec::new());
    };
    let Ok(proxy) = Proxy::new_owned(
        connection.clone(),
        POWER_PROFILES_NAME,
        POWER_PROFILES_PATH,
        POWER_PROFILES_IFACE,
    )
    .await
    else {
        return (false, String::new(), Vec::new());
    };
    let Ok(active) = proxy.get_property::<String>("ActiveProfile").await else {
        return (false, String::new(), Vec::new());
    };
    let entries: Vec<HashMap<String, OwnedValue>> = proxy
        .get_property("Profiles")
        .await
        .unwrap_or_default();
    let profiles = entries
        .iter()
        .filter_map(|entry| {
            entry
                .get("Profile")
                .and_then(|value| <&str>::try_from(value).ok())
                .map(str::to_owned)
        })
        .collect();
    (true, active, profiles)
}

async fn sample_profiles(
    connection: Option<&Connection>,
    ipc: &mpsc::Sender<IpcMessage>,
    last: &mut String,
) {
    let (available, active, profiles) = read_profiles(connection).await;
    let key = format!("{available}|{active}|{}", profiles.join(","));
    if key != *last {
        *last = key;
        let _ = ipc
            .send(IpcMessage::PowerProfileState {
                available,
                active,
                profiles,
            })
            .await;
    }
}

async fn apply_power_profile(connection: Option<&Connection>, profile: &str) {
    let Some(connection) = connection else {
        return;
    };
    let Ok(proxy) = Proxy::new_owned(
        connection.clone(),
        POWER_PROFILES_NAME,
        POWER_PROFILES_PATH,
        POWER_PROFILES_IFACE,
    )
    .await
    else {
        return;
    };
    if let Err(error) = proxy.set_property("ActiveProfile", profile).await {
        warn!(%error, profile, "falha ao trocar o perfil de energia");
    }
}

// ---------------------------------------------------------------------------
// Bateria (UPower)
// ---------------------------------------------------------------------------

async fn read_battery(connection: Option<&Connection>) -> (bool, u8, bool, u32) {
    let Some(connection) = connection else {
        return (false, 0, false, 0);
    };
    let Ok(proxy) = Proxy::new_owned(
        connection.clone(),
        "org.freedesktop.UPower",
        UPOWER_DISPLAY_PATH,
        UPOWER_DEVICE,
    )
    .await
    else {
        return (false, 0, false, 0);
    };
    let present = proxy.get_property::<bool>("IsPresent").await.unwrap_or(false);
    if !present {
        return (false, 0, false, 0);
    }
    let percentage = proxy
        .get_property::<f64>("Percentage")
        .await
        .unwrap_or(0.0);
    let state = proxy.get_property::<u32>("State").await.unwrap_or(0);
    let time_to_empty = proxy.get_property::<i64>("TimeToEmpty").await.unwrap_or(0);
    let charging = state == 1 || state == 4;
    let percent = percentage.round().clamp(0.0, 100.0) as u8;
    let time_left = if charging {
        0
    } else {
        (time_to_empty.max(0) / 60) as u32
    };
    (true, percent, charging, time_left)
}

async fn sample_battery(
    connection: Option<&Connection>,
    ipc: &mpsc::Sender<IpcMessage>,
    last: &mut String,
) {
    let (present, percent, charging, time_left) = read_battery(connection).await;
    let key = format!("{present}|{percent}|{charging}|{time_left}");
    if key != *last {
        *last = key;
        let _ = ipc
            .send(IpcMessage::BatteryState {
                present,
                percent,
                charging,
                time_left,
            })
            .await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_wpctl_volume() {
        assert_eq!(parse_wpctl("Volume: 0.70\n"), (70, false));
        assert_eq!(parse_wpctl("Volume: 0.34 [MUTED]\n"), (34, true));
        assert_eq!(parse_wpctl("Volume: 1.00\n"), (100, false));
    }

    #[test]
    fn parses_pactl_volume() {
        assert_eq!(
            parse_pactl_volume("Volume: front-left: 45875 /  70% / -9.29 dB, front-right: 45875 /  70% / -9.29 dB\n"),
            Some(70)
        );
        assert_eq!(parse_pactl_volume("sem volume"), None);
    }

    #[test]
    fn classifies_network_types() {
        assert_eq!(classify("802-11-wireless"), "wifi");
        assert_eq!(classify("802-3-ethernet"), "ethernet");
        assert_eq!(classify("bridge"), "bridge");
        assert_eq!(classify("vpn"), "vpn");
        assert_eq!(classify("tun"), "other");
    }
}
