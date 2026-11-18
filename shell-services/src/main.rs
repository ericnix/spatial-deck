//! `spatial-deck-services`: serviços D-Bus do shell.
//!
//! Primeira função: daemon `org.freedesktop.Notifications`. Conecta no socket
//! IPC do compositor e encaminha notificações para o frontend (WebSocket),
//! recebendo de volta DND, ações e fechamentos.
//!
//! O nome padrão D-Bus pode estar ocupado pelo daemon da sessão hospedeira.
//! Nesse caso o serviço continua no nome alternativo (ver `dbus.rs`) e não
//! interfere no desktop real.

mod dbus;
mod ipc;
mod media;
mod regions;
mod system;
mod store;
mod tray;

use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use common::{IpcMessage, NOTIFICATION_CLOSED_EXPIRED};
use tokio::sync::{mpsc, Mutex};
use tracing::{info, warn};
use tracing_subscriber::EnvFilter;
use zbus::object_server::SignalEmitter;

use crate::{
    dbus::{Notifier, INTERFACE, NOTIFY_PATH},
    regions::RegionStore,
    store::Store,
};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    tracing_subscriber::fmt().with_env_filter(filter).init();

    let store = Arc::new(Mutex::new(Store::load()));
    let regions = Arc::new(Mutex::new(RegionStore::load()));
    let (ipc_tx, ipc_rx) = mpsc::channel(64);
    let (incoming_tx, mut incoming_rx) = mpsc::channel(256);

    tokio::spawn(ipc::run(ipc_rx, incoming_tx));

    let connection = dbus::serve(Notifier {
        store: store.clone(),
        ipc: ipc_tx.clone(),
    })
    .await?;

    let emitter = {
        let interface = connection
            .object_server()
            .interface::<_, Notifier>(NOTIFY_PATH)
            .await?;
        interface.signal_emitter().clone().into_owned()
    };

    let (tray_tx, tray_rx) = mpsc::channel(16);
    let (media_tx, media_rx) = mpsc::channel(16);
    let (system_tx, system_rx) = mpsc::channel(16);
    {
        let connection = connection.clone();
        let ipc = ipc_tx.clone();
        tokio::spawn(async move {
            tray::start(connection, ipc, tray_rx).await;
        });
    }
    {
        let connection = connection.clone();
        let ipc = ipc_tx.clone();
        tokio::spawn(async move {
            media::run(connection, ipc, media_rx).await;
        });
    }
    {
        let ipc = ipc_tx.clone();
        tokio::spawn(async move {
            system::run(ipc, system_rx).await;
        });
    }

    info!("shell-services pronto");

    // Notificações com prazo são fechadas num tick único, sem uma task por
    // notificação; o mesmo caminho emite o sinal D-Bus e avisa o frontend.
    let mut ticker = tokio::time::interval(Duration::from_millis(250));
    loop {
        tokio::select! {
            message = incoming_rx.recv() => {
                let Some(message) = message else {
                    break;
                };
                handle(
                    message,
                    &store,
                    &regions,
                    &ipc_tx,
                    &emitter,
                    &tray_tx,
                    &media_tx,
                    &system_tx,
                )
                .await;
            }
            _ = ticker.tick() => {
                let expired = store.lock().await.expire_due(Instant::now());
                for id in expired {
                    let _ = ipc_tx
                        .send(IpcMessage::NotificationClosed {
                            id,
                            reason: NOTIFICATION_CLOSED_EXPIRED,
                        })
                        .await;
                    if let Err(error) = emitter
                        .emit(INTERFACE, "NotificationClosed", &(id, NOTIFICATION_CLOSED_EXPIRED))
                        .await
                    {
                        warn!(%error, id, "falha ao emitir NotificationClosed");
                    }
                }
            }
        }
    }

    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn handle(
    message: IpcMessage,
    store: &Arc<Mutex<Store>>,
    regions: &Arc<Mutex<RegionStore>>,
    ipc: &mpsc::Sender<IpcMessage>,
    emitter: &SignalEmitter<'static>,
    tray: &mpsc::Sender<tray::TrayCommand>,
    media: &mpsc::Sender<media::MediaCommand>,
    system: &mpsc::Sender<system::SystemCommand>,
) {
    match message {
        // O compositor manda AppList ao aceitar a conexão; o pedido explícito
        // vem do frontend quando a página abre ou recarrega.
        IpcMessage::AppList { .. } => {
            announce(store, ipc).await;
            send_regions(regions, ipc).await;
        }
        IpcMessage::NotificationHistoryRequest => {
            announce(store, ipc).await;
        }
        IpcMessage::RegionListRequest => {
            send_regions(regions, ipc).await;
        }
        IpcMessage::SaveRegion {
            name,
            scale,
            offset_x,
            offset_y,
        } => {
            let region = regions
                .lock()
                .await
                .save(&name, scale, offset_x, offset_y);
            let _ = ipc.send(IpcMessage::RegionSaved { region }).await;
            send_regions(regions, ipc).await;
        }
        IpcMessage::DeleteRegion { id } => {
            if regions.lock().await.delete(id) {
                send_regions(regions, ipc).await;
            }
        }
        IpcMessage::GotoRegion { .. } => {
            // O frontend anima a transformação; o daemon só mantém a lista.
        }
        IpcMessage::TrayActivate { id } => {
            let _ = tray.send(tray::TrayCommand::Activate(id)).await;
        }
        IpcMessage::TraySecondaryActivate { id } => {
            let _ = tray.send(tray::TrayCommand::SecondaryActivate(id)).await;
        }
        IpcMessage::TrayMenuRequest { id } => {
            let _ = tray.send(tray::TrayCommand::Menu(id)).await;
        }
        IpcMessage::TrayRequest => {
            let _ = tray.send(tray::TrayCommand::Refresh).await;
        }
        IpcMessage::TrayMenuEvent { id, event_id } => {
            let _ = tray
                .send(tray::TrayCommand::MenuEvent { id, event_id })
                .await;
        }
        IpcMessage::MediaCommand { command } => {
            let _ = media.send(media::MediaCommand::Action(command)).await;
        }
        IpcMessage::MediaRequest => {
            let _ = media.send(media::MediaCommand::Refresh).await;
        }
        IpcMessage::StatusRequest => {
            let _ = system.send(system::SystemCommand::Refresh).await;
        }
        IpcMessage::VolumeCommand { action, value } => {
            let _ = system
                .send(system::SystemCommand::Volume { action, value })
                .await;
        }
        IpcMessage::BrightnessCommand { action, value } => {
            let _ = system
                .send(system::SystemCommand::Brightness { action, value })
                .await;
        }
        IpcMessage::NetworkState { .. }
        | IpcMessage::BatteryState { .. }
        | IpcMessage::VolumeState { .. }
        | IpcMessage::BrightnessState { .. } => {
            // Estados produzidos pelos próprios serviços.
        }
        IpcMessage::TrayUpdate { .. }
        | IpcMessage::TrayMenu { .. }
        | IpcMessage::MediaState { .. } => {
            // Mensagens produzidas pelos próprios serviços.
        }
        IpcMessage::DndState { active } => {
            store.lock().await.set_dnd(active);
        }
        IpcMessage::NotificationHold { id, held } => {
            store.lock().await.hold(id, held);
        }
        IpcMessage::NotificationAction { id, action } => {
            if let Err(error) = emitter.emit(INTERFACE, "ActionInvoked", &(id, action)).await {
                warn!(%error, id, "falha ao emitir ActionInvoked");
            }
        }
        IpcMessage::NotificationClosed { id, reason } => {
            // Fechamento pedido pela interface: propaga ao aplicativo.
            if store.lock().await.close(id) {
                if let Err(error) = emitter
                    .emit(INTERFACE, "NotificationClosed", &(id, reason))
                    .await
                {
                    warn!(%error, id, "falha ao emitir NotificationClosed");
                }
            }
        }
        _ => {}
    }
}

/// Envia a lista de regiões para todos os clientes.
async fn send_regions(regions: &Arc<Mutex<RegionStore>>, ipc: &mpsc::Sender<IpcMessage>) {
    let regions = regions.lock().await.list();
    let _ = ipc.send(IpcMessage::RegionList { regions }).await;
}

/// Envia o histórico e o estado de DND para quem estiver escutando.
async fn announce(store: &Arc<Mutex<Store>>, ipc: &mpsc::Sender<IpcMessage>) {
    let (items, dnd) = {
        let store = store.lock().await;
        (store.history(), store.dnd())
    };
    let _ = ipc.send(IpcMessage::NotificationHistory { items }).await;
    let _ = ipc.send(IpcMessage::DndState { active: dnd }).await;
}
