//! Estado do sistema para a barra: rede, bateria, volume e brilho.
//!
//! - **Rede**: NetworkManager (D-Bus do sistema), com força do sinal quando Wi-Fi.
//! - **Bateria**: UPower `DisplayDevice`; some quando não há bateria.
//! - **Volume**: `wpctl` (PipeWire), com fallback para `pactl`.
//! - **Brilho**: `/sys/class/backlight`, escrita via `logind.SetBrightness`
//!   (sem root). Sem backlight, o módulo fica indisponível.

use std::{fs, process::Stdio, time::Duration};

use common::{BrightnessAction, IpcMessage, VolumeAction};
use tokio::{process::Command, sync::mpsc, time::interval};
use tracing::{debug, warn};
use zbus::{wire::OwnedObjectPath, Connection, Proxy};

const NM_NAME: &str = "org.freedesktop.NetworkManager";
const NM_PATH: &str = "/org/freedesktop/NetworkManager";
const NM_IFACE: &str = "org.freedesktop.NetworkManager";
const LOGIND_NAME: &str = "org.freedesktop.login1";
const LOGIND_PATH: &str = "/org/freedesktop/login1";
const LOGIND_MANAGER: &str = "org.freedesktop.login1.Manager";
const LOGIND_SESSION: &str = "org.freedesktop.login1.Session";
const UPOWER_DEVICE: &str = "org.freedesktop.UPower.Device";
const UPOWER_DISPLAY_PATH: &str = "/org/freedesktop/UPower/devices/DisplayDevice";

pub enum SystemCommand {
    Volume { action: VolumeAction, value: u8 },
    Brightness { action: BrightnessAction, value: u8 },
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
    let mut ticker = interval(Duration::from_secs(1));
    let mut tick: u64 = 0;

    loop {
        tokio::select! {
            _ = ticker.tick() => {
                sample_volume(&audio, &ipc, &mut last_volume).await;
                sample_brightness(&ipc, &mut last_brightness).await;
                if tick % 3 == 0 {
                    sample_network(system.as_ref(), &ipc, &mut last_network).await;
                    sample_battery(system.as_ref(), &ipc, &mut last_battery).await;
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
                        sample_network(system.as_ref(), &ipc, &mut last_network).await;
                        sample_battery(system.as_ref(), &ipc, &mut last_battery).await;
                        sample_volume(&audio, &ipc, &mut last_volume).await;
                        sample_brightness(&ipc, &mut last_brightness).await;
                    }
                    SystemCommand::Volume { action, value } => {
                        apply_volume(audio, action, value).await;
                        last_volume.clear();
                        sample_volume(&audio, &ipc, &mut last_volume).await;
                    }
                    SystemCommand::Brightness { action, value } => {
                        apply_brightness(system.as_ref(), action, value).await;
                        last_brightness.clear();
                        sample_brightness(&ipc, &mut last_brightness).await;
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

async fn sample_brightness(ipc: &mpsc::Sender<IpcMessage>, last: &mut String) {
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

async fn read_network(connection: Option<&Connection>) -> (bool, String, String, u8) {
    let Some(connection) = connection else {
        return (false, "offline".to_owned(), String::new(), 0);
    };
    let Ok(proxy) = Proxy::new_owned(connection.clone(), NM_NAME, NM_PATH, NM_IFACE).await else {
        return (false, "offline".to_owned(), String::new(), 0);
    };
    let state: u32 = proxy.get_property("State").await.unwrap_or(0);
    if state < 40 {
        return (false, "offline".to_owned(), String::new(), 0);
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
        return (true, "offline".to_owned(), String::new(), 0);
    };
    let strength = if kind == "wifi" {
        wifi_strength(connection, &path).await.unwrap_or(0)
    } else {
        100
    };
    (true, kind, ssid, strength)
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
    let (connected, kind, ssid, strength) = read_network(connection).await;
    let key = format!("{connected}|{kind}|{ssid}|{strength}");
    if key != *last {
        *last = key;
        let _ = ipc
            .send(IpcMessage::NetworkState {
                connected,
                kind,
                ssid,
                strength,
            })
            .await;
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
