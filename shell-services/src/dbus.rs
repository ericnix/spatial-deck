//! Servidor D-Bus `org.freedesktop.Notifications`.

use std::{
    collections::HashMap,
    sync::Arc,
    time::{Duration, Instant},
};

use common::{
    IpcMessage, NotificationButton, NotificationItem, NOTIFICATION_CLOSED_BY_CALL,
};
use tokio::sync::{mpsc, Mutex};
use tracing::{info, warn};
use zbus::{fdo::RequestNameFlags, object_server::SignalEmitter, Connection, OwnedVariant};

use crate::store::Store;

pub const INTERFACE: &str = "org.freedesktop.Notifications";
pub const NOTIFY_PATH: &str = "/org/freedesktop/Notifications";
/// Nome alternativo: permite testar o daemon sem disputar o nome padrão com
/// o daemon da sessão hospedeira (KDE/GNOME/dunst).
pub const FALLBACK_NAME: &str = "org.freedesktop.Notifications.SpatialDeck";

pub struct Notifier {
    pub store: Arc<Mutex<Store>>,
    pub ipc: mpsc::Sender<IpcMessage>,
}

/// Converte a lista plana `[id, label, id, label, …]` do protocolo.
fn buttons_from(actions: &[String]) -> Vec<NotificationButton> {
    actions
        .chunks_exact(2)
        .map(|pair| NotificationButton {
            key: pair[0].clone(),
            label: pair[1].clone(),
        })
        .collect()
}

/// `expire_timeout`: `0` nunca expira, `> 0` milissegundos, `-1` padrão do
/// servidor (5 s; críticas ficam até serem fechadas).
fn resolve_timeout(expire_timeout: i32, urgency: u8) -> Option<u64> {
    if expire_timeout > 0 {
        Some(expire_timeout as u64)
    } else if expire_timeout == 0 {
        None
    } else if urgency >= 2 {
        None
    } else {
        Some(5_000)
    }
}

#[zbus::interface(
    name = "org.freedesktop.Notifications",
    proxy(assume_defaults = true, visibility = "pub")
)]
impl Notifier {
    #[allow(clippy::too_many_arguments)]
    async fn notify(
        &self,
        app_name: String,
        replaces_id: u32,
        app_icon: String,
        summary: String,
        body: String,
        actions: Vec<String>,
        hints: HashMap<String, OwnedVariant>,
        expire_timeout: i32,
    ) -> u32 {
        let urgency = hints
            .get("urgency")
            .and_then(|value| u8::try_from(value).ok())
            .unwrap_or(1);
        let buttons = buttons_from(&actions);
        let timeout = resolve_timeout(expire_timeout, urgency);
        let deadline = timeout.map(|ms| Instant::now() + Duration::from_millis(ms));

        let item = NotificationItem {
            id: 0,
            app_name: app_name.clone(),
            summary: summary.clone(),
            body: body.clone(),
            icon: app_icon.clone(),
            urgency,
            actions: buttons.clone(),
            timestamp_ms: 0,
        };
        let id = self.store.lock().await.push(item, replaces_id, deadline);

        let _ = self
            .ipc
            .send(IpcMessage::Notification {
                id,
                app_name,
                summary,
                body,
                icon: app_icon,
                urgency,
                actions: buttons,
                timeout: expire_timeout,
                replaces_id,
            })
            .await;

        if let Some(ms) = timeout {
            tracing::debug!(%ms, id, "notificação com prazo");
        }

        id
    }

    async fn close_notification(&self, id: u32, #[zbus(signal_emitter)] emitter: SignalEmitter<'_>) {
        if self.store.lock().await.close(id) {
            let _ = self
                .ipc
                .send(IpcMessage::NotificationClosed {
                    id,
                    reason: NOTIFICATION_CLOSED_BY_CALL,
                })
                .await;
            let _ = emitter
                .emit(
                    INTERFACE,
                    "NotificationClosed",
                    &(id, NOTIFICATION_CLOSED_BY_CALL),
                )
                .await;
        }
    }

    fn get_capabilities(&self) -> Vec<String> {
        vec![
            "actions".into(),
            "body".into(),
            "body-markup".into(),
            "icon-static".into(),
            "persistence".into(),
        ]
    }

    fn get_server_information(&self) -> (String, String, String, String) {
        (
            "Spatial Deck".into(),
            "spatial-deck".into(),
            "0.1".into(),
            "1.2".into(),
        )
    }

    #[zbus(signal)]
    async fn notification_closed(
        emitter: &SignalEmitter<'_>,
        id: u32,
        reason: u32,
    ) -> zbus::Result<()>;

    #[zbus(signal)]
    async fn action_invoked(
        emitter: &SignalEmitter<'_>,
        id: u32,
        action: String,
    ) -> zbus::Result<()>;
}

/// Sobe o servidor no barramento da sessão. O nome padrão pode estar ocupado
/// pelo daemon da sessão hospedeira: nesse caso o serviço continua de pé no
/// nome alternativo (e tenta novamente não é necessário — o teste usa o
/// alternativo).
pub async fn serve(notifier: Notifier) -> anyhow::Result<Connection> {
    let connection = zbus::connection::Builder::session()
        .name(FALLBACK_NAME)
        .serve_at(NOTIFY_PATH, notifier)
        .build()
        .await?;

    let replace = std::env::var("SPATIAL_DECK_NOTIFY_REPLACE")
        .map(|value| value == "1")
        .unwrap_or(false);

    let acquired = if replace {
        connection
            .request_name_with_flags(
                INTERFACE,
                RequestNameFlags::REPLACE_EXISTING | RequestNameFlags::DO_NOT_QUEUE,
            )
            .await
            .is_ok()
    } else {
        connection.request_name(INTERFACE).await.is_ok()
    };

    if acquired {
        info!(name = INTERFACE, "daemon de notificações registrado");
    } else {
        warn!(
            name = INTERFACE,
            fallback = FALLBACK_NAME,
            "nome padrão de notificações ocupado por outro daemon; \
             o desktop continua no nome alternativo (use \
             SPATIAL_DECK_NOTIFY_REPLACE=1 para substituir ou pare o outro daemon)"
        );
    }

    Ok(connection)
}

#[cfg(test)]
mod tests {
    use super::*;
    use common::NOTIFICATION_CLOSED_DISMISSED;
    use tokio::sync::mpsc;

    async fn session_or_skip() -> Option<Connection> {
        match Connection::session().await {
            Ok(connection) => Some(connection),
            Err(error) => {
                eprintln!("sem barramento de sessão ({error}); teste D-Bus ignorado");
                None
            }
        }
    }

    #[tokio::test]
    async fn notify_over_dbus_reaches_ipc() {
        let Some(connection) = session_or_skip().await else {
            return;
        };
        let (tx, mut rx) = mpsc::channel(8);
        let notifier = Notifier {
            store: Arc::new(Mutex::new(Store::in_memory())),
            ipc: tx,
        };
        connection
            .object_server()
            .at("/org/freedesktop/Notifications", notifier)
            .await
            .expect("registrar objeto de teste");

        let destination = connection.unique_name().unwrap().to_string();
        let proxy = NotifierProxy::builder(&connection)
            .destination(destination)
            .path(NOTIFY_PATH)
            .build()
            .await
            .expect("criar proxy");

        let id = proxy
            .notify(
                "Teste".to_string(),
                0,
                "dialog-information".to_string(),
                "Resumo".to_string(),
                "Corpo".to_string(),
                Vec::new(),
                HashMap::new(),
                0,
            )
            .await
            .expect("chamar Notify");
        assert!(id >= 1);

        match rx.recv().await.expect("mensagem IPC") {
            IpcMessage::Notification {
                id: ipc_id,
                summary,
                body,
                urgency,
                ..
            } => {
                assert_eq!(ipc_id, id);
                assert_eq!(summary, "Resumo");
                assert_eq!(body, "Corpo");
                assert_eq!(urgency, 1);
            }
            other => panic!("mensagem inesperada: {other:?}"),
        }

        // CloseNotification fecha e emite NotificationClosed.
        proxy.close_notification(id).await.expect("fechar");
        match rx.recv().await.expect("fechamento") {
            IpcMessage::NotificationClosed { id: closed, reason } => {
                assert_eq!(closed, id);
                assert_eq!(reason, NOTIFICATION_CLOSED_BY_CALL);
            }
            other => panic!("mensagem inesperada: {other:?}"),
        }
        assert_ne!(NOTIFICATION_CLOSED_DISMISSED, NOTIFICATION_CLOSED_BY_CALL);
    }

    #[tokio::test]
    async fn capabilities_and_information() {
        let Some(connection) = session_or_skip().await else {
            return;
        };
        let (tx, _rx) = mpsc::channel(1);
        let notifier = Notifier {
            store: Arc::new(Mutex::new(Store::in_memory())),
            ipc: tx,
        };
        connection
            .object_server()
            .at("/org/freedesktop/Notifications", notifier)
            .await
            .expect("registrar objeto de teste");
        let destination = connection.unique_name().unwrap().to_string();
        let proxy = NotifierProxy::builder(&connection)
            .destination(destination)
            .path(NOTIFY_PATH)
            .build()
            .await
            .expect("criar proxy");

        let capabilities = proxy.get_capabilities().await.expect("capabilities");
        assert!(capabilities.iter().any(|item| item == "actions"));
        let (name, vendor, _, _) = proxy.get_server_information().await.expect("info");
        assert_eq!(name, "Spatial Deck");
        assert_eq!(vendor, "spatial-deck");
    }

    #[test]
    fn timeout_defaults() {
        assert_eq!(resolve_timeout(0, 1), None);
        assert_eq!(resolve_timeout(-1, 1), Some(5_000));
        assert_eq!(resolve_timeout(-1, 2), None);
        assert_eq!(resolve_timeout(1_500, 1), Some(1_500));
    }

    #[test]
    fn action_pairs() {
        let buttons = buttons_from(&["default".into(), "Abrir".into(), "x".into(), "Fechar".into()]);
        assert_eq!(buttons.len(), 2);
        assert_eq!(buttons[1].key, "x");
        assert_eq!(buttons[1].label, "Fechar");
    }
}
