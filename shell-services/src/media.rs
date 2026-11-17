//! Player MPRIS ativo: metadados, estado e comandos.

use std::{collections::HashMap, time::Duration};

use common::{IpcMessage, MediaAction};
use tokio::sync::mpsc;
use tracing::{debug, warn};
use zbus::{fdo::DBusProxy, Connection, OwnedVariant, Proxy};

const PLAYER_PATH: &str = "/org/mpris/MediaPlayer2";
const PLAYER_IFACE: &str = "org.mpris.MediaPlayer2.Player";
const NAME_PREFIX: &str = "org.mpris.MediaPlayer2.";

struct PlayerState {
    name: String,
    title: String,
    artist: String,
    status: String,
    can_next: bool,
    can_prev: bool,
}

pub enum MediaCommand {
    Action(MediaAction),
    Refresh,
}

pub async fn run(
    connection: Connection,
    ipc: mpsc::Sender<IpcMessage>,
    mut commands: mpsc::Receiver<MediaCommand>,
) {
    let mut last_json = String::new();
    let mut last_player = String::new();
    let mut ticker = tokio::time::interval(Duration::from_millis(1_000));

    loop {
        tokio::select! {
            _ = ticker.tick() => {
                match active_player(&connection).await {
                    Some(state) => {
                        let json = format!(
                            "{}|{}|{}|{}|{}|{}",
                            state.name, state.title, state.artist, state.status,
                            state.can_next, state.can_prev
                        );
                        if json != last_json {
                            last_json = json;
                            last_player = state.name.clone();
                            let _ = ipc
                                .send(IpcMessage::MediaState {
                                    player: state.name,
                                    title: state.title,
                                    artist: state.artist,
                                    status: state.status,
                                    can_next: state.can_next,
                                    can_prev: state.can_prev,
                                })
                                .await;
                        }
                    }
                    None => {
                        if !last_player.is_empty() {
                            last_player.clear();
                            last_json.clear();
                            let _ = ipc
                                .send(IpcMessage::MediaState {
                                    player: String::new(),
                                    title: String::new(),
                                    artist: String::new(),
                                    status: String::new(),
                                    can_next: false,
                                    can_prev: false,
                                })
                                .await;
                        }
                    }
                }
            }
            command = commands.recv() => {
                let Some(command) = command else {
                    break;
                };
                let action = match command {
                    MediaCommand::Refresh => {
                        // Força o próximo tick a reenviar o estado atual.
                        last_json.clear();
                        continue;
                    }
                    MediaCommand::Action(action) => action,
                };
                if last_player.is_empty() {
                    continue;
                }
                let method = match action {
                    MediaAction::PlayPause => "PlayPause",
                    MediaAction::Next => "Next",
                    MediaAction::Previous => "Previous",
                };
                match Proxy::new_owned(
                    connection.clone(),
                    last_player.clone(),
                    PLAYER_PATH,
                    PLAYER_IFACE,
                )
                .await
                {
                    Ok(proxy) => {
                        if let Err(error) = proxy.call::<_, _, ()>(method, &()).await {
                            warn!(%error, method, "falha no comando MPRIS");
                        }
                    }
                    Err(error) => debug!(%error, "player MPRIS indisponível"),
                }
            }
        }
    }
}

/// Escolhe o player com `PlaybackStatus == Playing`; na falta, o primeiro com
/// metadados. `playerctld` sem faixa é ignorado para não ocupar a barra.
async fn active_player(connection: &Connection) -> Option<PlayerState> {
    let dbus = DBusProxy::new(connection).await.ok()?;
    let names = dbus.list_names().await.ok()?;
    let mut fallback: Option<PlayerState> = None;

    for name in names {
        let name = name.to_string();
        if !name.starts_with(NAME_PREFIX) {
            continue;
        }
        let Ok(proxy) = Proxy::new_owned(
            connection.clone(),
            name.clone(),
            PLAYER_PATH,
            PLAYER_IFACE,
        )
        .await
        else {
            continue;
        };

        let status = proxy
            .get_property::<String>("PlaybackStatus")
            .await
            .unwrap_or_default();
        let metadata = proxy
            .get_property::<HashMap<String, OwnedVariant>>("Metadata")
            .await
            .unwrap_or_default();
        let title = metadata
            .get("xesam:title")
            .and_then(|value| value.try_clone().ok())
            .and_then(|value| String::try_from(value).ok())
            .filter(|title| !title.is_empty())
            .or_else(|| {
                metadata
                    .get("xesam:url")
                    .and_then(|value| value.try_clone().ok())
                    .and_then(|value| String::try_from(value).ok())
                    .map(|url| title_from_url(&url))
            })
            .unwrap_or_default();
        let artist = metadata
            .get("xesam:artist")
            .and_then(|value| value.try_clone().ok())
            .and_then(|value| Vec::<String>::try_from(value).ok())
            .map(|values| values.join(", "))
            .unwrap_or_default();

        if title.is_empty() && name.ends_with(".playerctld") {
            continue;
        }

        let state = PlayerState {
            can_next: proxy.get_property::<bool>("CanGoNext").await.unwrap_or(false),
            can_prev: proxy
                .get_property::<bool>("CanGoPrevious")
                .await
                .unwrap_or(false),
            name,
            title,
            artist,
            status: status.clone(),
        };

        if status == "Playing" {
            return Some(state);
        }
        if fallback.is_none() {
            fallback = Some(state);
        }
    }

    fallback
}

/// VLC e alguns players só publicam `xesam:url`; usa o nome do arquivo.
fn title_from_url(url: &str) -> String {
    let name = url.rsplit('/').next().unwrap_or(url).replace("%20", " ");
    name.rsplit_once('.')
        .map(|(stem, _)| stem.to_owned())
        .unwrap_or(name)
}

#[cfg(test)]
mod tests {
    use super::*;
    use zbus::Variant;

    #[test]
    fn reads_title() {
        let metadata: HashMap<String, OwnedVariant> = [(
            "xesam:title".to_string(),
            OwnedVariant::try_from(Variant::from("Faixa")).unwrap(),
        )]
        .into_iter()
        .collect();
        let title = metadata
            .get("xesam:title")
            .and_then(|value| value.try_clone().ok())
            .and_then(|value| String::try_from(value).ok());
        assert_eq!(title.as_deref(), Some("Faixa"));
    }

    #[test]
    fn title_falls_back_to_url() {
        assert_eq!(title_from_url("file:///home/userx/my%20video.mkv"), "my video");
        assert_eq!(title_from_url("file:///tmp/faixa.mp3"), "faixa");
    }

    #[test]
    fn joins_artist_list() {
        let array = Variant::Array(
            vec![Variant::from("A"), Variant::from("B")]
                .into_iter()
                .collect::<Vec<_>>()
                .into(),
        );
        let artists = OwnedVariant::try_from(array).unwrap();
        let joined = Vec::<String>::try_from(artists)
            .map(|values| values.join(", "))
            .unwrap();
        assert_eq!(joined, "A, B");
    }
}
