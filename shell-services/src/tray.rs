//! Bandeja do sistema via `StatusNotifierItem`.
//!
//! O serviço funciona em dois modos:
//! - **watcher próprio** quando `org.kde.StatusNotifierWatcher` está livre
//!   (sessão isolada, por exemplo): implementa o protocolo e recebe os itens;
//! - **host** quando já existe um watcher (KDE/GNOME): registra-se como host e
//!   lê a lista e as propriedades dos itens existentes, sem disputar o nome.
//!
//! Os itens são consultados a cada 2 s (título, ícone, status e menu) e o
//! frontend recebe `tray_update` apenas quando algo muda. O menu dbusmenu é
//! buscado sob demanda (`GetLayout`) e devolvido em `tray_menu`.

use std::{collections::HashMap, sync::Arc, time::Duration};

use common::{IpcMessage, TrayItem, TrayMenuEntry};
use tokio::sync::{mpsc, Mutex};
use tracing::{debug, info, warn};
use zbus::{object_server::SignalEmitter, Connection, OwnedVariant, Proxy, Variant};

pub const WATCHER_NAME: &str = "org.kde.StatusNotifierWatcher";
const WATCHER_PATH: &str = "/StatusNotifierWatcher";
const WATCHER_IFACE: &str = "org.kde.StatusNotifierWatcher";
const ITEM_IFACE: &str = "org.kde.StatusNotifierItem";
const MENU_IFACE: &str = "com.canonical.dbusmenu";
const ITEM_DEFAULT_PATH: &str = "/StatusNotifierItem";

#[derive(Clone, Default)]
struct WatcherRegistry {
    /// Strings no formato `serviço/caminho` (convenção do KDE).
    items: Arc<Mutex<Vec<String>>>,
}

struct StatusNotifierWatcher {
    registry: WatcherRegistry,
}

#[zbus::interface(name = "org.kde.StatusNotifierWatcher")]
impl StatusNotifierWatcher {
    async fn register_status_notifier_item(
        &self,
        service: String,
        #[zbus(header)] header: zbus::message::Header<'_>,
        #[zbus(signal_emitter)] emitter: SignalEmitter<'_>,
    ) {
        // Registro por caminho usa o nome único do remetente.
        let raw = if service.starts_with('/') {
            match header.sender() {
                Some(sender) => format!("{}{}", sender.as_str(), service),
                None => service,
            }
        } else {
            service
        };
        {
            let mut items = self.registry.items.lock().await;
            if items.iter().any(|item| item == &raw) {
                return;
            }
            items.push(raw.clone());
        }
        info!(item = %raw, "item da bandeja registrado");
        let _ = emitter
            .emit(
                WATCHER_IFACE,
                "StatusNotifierItemRegistered",
                &(raw,),
            )
            .await;
    }

    async fn register_status_notifier_host(&self, _service: String) {
        // O próprio desktop já é o host que desenha os itens.
    }

    #[zbus(property)]
    async fn registered_status_notifier_items(&self) -> Vec<String> {
        self.registry.items.lock().await.clone()
    }

    #[zbus(property)]
    fn is_status_notifier_host_registered(&self) -> bool {
        true
    }

    #[zbus(property)]
    fn protocol_version(&self) -> i32 {
        0
    }

    #[zbus(signal)]
    async fn status_notifier_item_registered(
        emitter: &SignalEmitter<'_>,
        service: String,
    ) -> zbus::Result<()>;

    #[zbus(signal)]
    async fn status_notifier_item_unregistered(
        emitter: &SignalEmitter<'_>,
        service: String,
    ) -> zbus::Result<()>;
}

enum WatcherMode {
    Own(WatcherRegistry),
    Remote(Proxy<'static>),
}

impl WatcherMode {
    async fn list(&self) -> Vec<String> {
        match self {
            Self::Own(registry) => registry.items.lock().await.clone(),
            Self::Remote(proxy) => proxy
                .get_property::<Vec<String>>("RegisteredStatusNotifierItems")
                .await
                .unwrap_or_default(),
        }
    }
}

/// Sobe o watcher (se possível) ou se conecta ao watcher existente.
async fn watcher_mode(connection: &Connection) -> Option<WatcherMode> {
    let registry = WatcherRegistry::default();
    if connection.request_name(WATCHER_NAME).await.is_ok() {
        let watcher = StatusNotifierWatcher {
            registry: registry.clone(),
        };
        match connection
            .object_server()
            .at(WATCHER_PATH, watcher)
            .await
        {
            Ok(_) => {
                info!(name = WATCHER_NAME, "watcher da bandeja registrado");
                return Some(WatcherMode::Own(registry));
            }
            Err(error) => warn!(%error, "falha ao servir o watcher da bandeja"),
        }
    }

    let proxy = Proxy::new_owned(
        connection.clone(),
        WATCHER_NAME,
        WATCHER_PATH,
        WATCHER_IFACE,
    )
    .await
    .ok()?;
    let unique = connection
        .unique_name()
        .map(|name| name.to_string())
        .unwrap_or_default();
    if let Err(error) = proxy
        .call::<_, _, ()>("RegisterStatusNotifierHost", &(unique,))
        .await
    {
        debug!(%error, "watcher existente recusou o host");
    }
    info!(name = WATCHER_NAME, "usando o watcher de bandeja da sessão");
    Some(WatcherMode::Remote(proxy))
}

pub enum TrayCommand {
    Activate(String),
    Refresh,
    SecondaryActivate(String),
    Menu(String),
    MenuEvent { id: String, event_id: i32 },
}

struct TrayEntry {
    item: TrayItem,
    destination: String,
    path: String,
    menu_path: Option<String>,
}

/// Inicia o watcher/host e o laço de consulta dos itens.
pub async fn start(
    connection: Connection,
    ipc: mpsc::Sender<IpcMessage>,
    commands: mpsc::Receiver<TrayCommand>,
) {
    let Some(mode) = watcher_mode(&connection).await else {
        warn!("nenhum watcher de bandeja disponível; módulo fica vazio");
        return;
    };
    info!("bandeja: iniciando monitoramento");
    run(connection, mode, ipc, commands).await;
}

async fn run(
    connection: Connection,
    mode: WatcherMode,
    ipc: mpsc::Sender<IpcMessage>,
    mut commands: mpsc::Receiver<TrayCommand>,
) {
    let mut entries: HashMap<String, TrayEntry> = HashMap::new();
    let mut last_json = String::new();
    let mut ticker = tokio::time::interval(Duration::from_secs(2));

    loop {
        tokio::select! {
            _ = ticker.tick() => {
                refresh(&connection, &mode, &ipc, &mut entries, &mut last_json).await;
            }
            command = commands.recv() => {
                let Some(command) = command else {
                    break;
                };
                handle_command(&connection, &entries, command, &ipc).await;
            }
        }
    }
}

async fn refresh(
    connection: &Connection,
    mode: &WatcherMode,
    ipc: &mpsc::Sender<IpcMessage>,
    entries: &mut HashMap<String, TrayEntry>,
    last_json: &mut String,
) {
    let registered = mode.list().await;
    debug!(count = registered.len(), "bandeja: consultando itens");
    let mut next = HashMap::new();
    for raw in registered {
        // Um item morto no barramento não pode travar a lista inteira.
        let entry = tokio::time::timeout(
            Duration::from_millis(900),
            fetch_entry(connection, &raw),
        )
        .await
        .ok()
        .flatten();
        if let Some(entry) = entry {
            next.insert(raw, entry);
        }
    }

    debug!(fetched = next.len(), "bandeja: propriedades obtidas");
    let mut items: Vec<TrayItem> = next.values().map(|entry| entry.item.clone()).collect();
    items.sort_by(|a, b| a.title.cmp(&b.title).then(a.id.cmp(&b.id)));
    *entries = next;

    let json = serde_json::to_string(&items).unwrap_or_default();
    if json != *last_json {
        *last_json = json;
        debug!(count = items.len(), "bandeja: enviando tray_update");
        if ipc.send(IpcMessage::TrayUpdate { items }).await.is_err() {
            warn!("bandeja: canal IPC fechado");
        }
    }
}

async fn fetch_entry(connection: &Connection, raw: &str) -> Option<TrayEntry> {
    let (destination, path) = resolve_item(raw);
    let proxy = Proxy::new_owned(
        connection.clone(),
        destination.clone(),
        path.clone(),
        ITEM_IFACE,
    )
    .await
    .ok()?;

    let title = property_string(&proxy, "Title").await.unwrap_or_default();
    let id = property_string(&proxy, "Id").await.unwrap_or_default();
    let icon_name = property_string(&proxy, "IconName").await.unwrap_or_default();
    let status = property_string(&proxy, "Status").await.unwrap_or_default();
    let item_is_menu = proxy
        .get_property::<bool>("ItemIsMenu")
        .await
        .unwrap_or(false);
    let menu_path = proxy
        .get_property::<zbus::wire::OwnedObjectPath>("Menu")
        .await
        .ok()
        .map(|path| path.as_str().to_owned())
        .filter(|path| !path.is_empty() && path != "/NO_DBUSMENU");

    let title = if title.is_empty() { id } else { title };
    Some(TrayEntry {
        item: TrayItem {
            id: raw.to_owned(),
            title,
            icon_name,
            status,
            has_menu: menu_path.is_some(),
            item_is_menu,
        },
        destination,
        path,
        menu_path,
    })
}

async fn handle_command(
    connection: &Connection,
    entries: &HashMap<String, TrayEntry>,
    command: TrayCommand,
    ipc: &mpsc::Sender<IpcMessage>,
) {
    match command {
        TrayCommand::Activate(id) => {
            if let Some(entry) = entries.get(&id) {
                if entry.item.item_is_menu {
                    show_menu(connection, entry, ipc).await;
                } else {
                    call_item(connection, entry, "Activate").await;
                }
            }
        }
        TrayCommand::Refresh => {
            let mut items: Vec<TrayItem> = entries.values().map(|entry| entry.item.clone()).collect();
            items.sort_by(|a, b| a.title.cmp(&b.title).then(a.id.cmp(&b.id)));
            let _ = ipc.send(IpcMessage::TrayUpdate { items }).await;
        }
        TrayCommand::SecondaryActivate(id) => {
            if let Some(entry) = entries.get(&id) {
                call_item(connection, entry, "SecondaryActivate").await;
            }
        }
        TrayCommand::Menu(id) => {
            if let Some(entry) = entries.get(&id) {
                show_menu(connection, entry, ipc).await;
            }
        }
        TrayCommand::MenuEvent { id, event_id } => {
            let Some(entry) = entries.get(&id) else {
                return;
            };
            let Some(menu_path) = &entry.menu_path else {
                return;
            };
            let Ok(proxy) = Proxy::new_owned(
                connection.clone(),
                entry.destination.clone(),
                menu_path.clone(),
                MENU_IFACE,
            )
            .await
            else {
                return;
            };
            let body = (event_id, "clicked", Variant::from(0i32), 0u32);
            let call = proxy.call::<_, _, ()>("Event", &body);
            if tokio::time::timeout(Duration::from_secs(1), call)
                .await
                .is_err()
            {
                warn!(event_id, "item do menu da bandeja não respondeu");
            }
        }
    }
}

async fn call_item(connection: &Connection, entry: &TrayEntry, method: &str) {
    let call = async {
        let proxy = Proxy::new_owned(
            connection.clone(),
            entry.destination.clone(),
            entry.path.clone(),
            ITEM_IFACE,
        )
        .await
        .ok()?;
        proxy.call::<_, _, ()>(method, &(0i32, 0i32)).await.ok()
    };
    if tokio::time::timeout(Duration::from_secs(1), call)
        .await
        .is_err()
    {
        debug!(method, "item da bandeja não respondeu");
    }
}

async fn show_menu(
    connection: &Connection,
    entry: &TrayEntry,
    ipc: &mpsc::Sender<IpcMessage>,
) {
    let items = tokio::time::timeout(Duration::from_millis(1_500), fetch_menu(connection, entry))
        .await
        .unwrap_or_default();
    let _ = ipc
        .send(IpcMessage::TrayMenu {
            id: entry.item.id.clone(),
            items,
        })
        .await;
}

async fn fetch_menu(connection: &Connection, entry: &TrayEntry) -> Vec<TrayMenuEntry> {
    let Some(menu_path) = &entry.menu_path else {
        return Vec::new();
    };
    let Ok(proxy) = Proxy::new_owned(
        connection.clone(),
        entry.destination.clone(),
        menu_path.clone(),
        MENU_IFACE,
    )
    .await
    else {
        return Vec::new();
    };

    if let Err(error) = proxy.call::<_, _, bool>("AboutToShow", &(0i32,)).await {
        debug!(%error, "menu: AboutToShow falhou");
    }
    let reply = match proxy
        .call_method("GetLayout", &(0i32, -1i32, Vec::<String>::new()))
        .await
    {
        Ok(reply) => reply,
        Err(error) => {
            debug!(%error, "menu: GetLayout falhou");
            return Vec::new();
        }
    };
    let body = reply.body();
    let parsed: Result<(u32, MenuLayout), _> = body.deserialize();
    let Ok((_revision, layout)) = parsed else {
        debug!("menu: layout não desserializou");
        return Vec::new();
    };
    let root = layout_to_entry(layout);
    debug!(
        children = root.children.len(),
        "menu: layout recebido"
    );
    root.children
}

/// Layout devolvido por `GetLayout`: `(ia{sv}av)` — id, propriedades e
/// filhos (cada filho é uma variante com outro layout).
#[derive(Debug, serde::Deserialize, zbus::Type)]
struct MenuLayout(i32, HashMap<String, OwnedVariant>, Vec<OwnedVariant>);

fn layout_to_entry(layout: MenuLayout) -> TrayMenuEntry {
    let mut label = String::new();
    let mut enabled = true;
    let mut visible = true;
    let mut is_separator = false;
    for (key, value) in &layout.1 {
        match key.as_str() {
            "label" => label = owned_string(value).unwrap_or_default(),
            "enabled" => enabled = owned_bool(value).unwrap_or(true),
            "visible" => visible = owned_bool(value).unwrap_or(true),
            "type" => is_separator = owned_string(value).as_deref() == Some("separator"),
            _ => {}
        }
    }

    let mut children = Vec::new();
    for child in layout.2 {
        if let Ok(structure) = zbus::wire::Structure::try_from(child) {
            if let Some(entry) = entry_from_structure(&structure) {
                if entry.visible {
                    children.push(entry);
                }
            }
        }
    }

    TrayMenuEntry {
        id: layout.0,
        label,
        enabled,
        visible,
        is_separator,
        children,
    }
}

fn owned_string(value: &OwnedVariant) -> Option<String> {
    String::try_from(value.try_clone().ok()?).ok()
}

fn owned_bool(value: &OwnedVariant) -> Option<bool> {
    bool::try_from(value.try_clone().ok()?).ok()
}

fn entry_from_structure(structure: &zbus::wire::Structure<'_>) -> Option<TrayMenuEntry> {
    let fields = structure.fields();
    let id = match fields.first()? {
        Variant::I32(id) => *id,
        Variant::U32(id) => *id as i32,
        _ => return None,
    };

    let mut label = String::new();
    let mut enabled = true;
    let mut visible = true;
    let mut is_separator = false;
    if let Some(field) = fields.get(1) {
        if let Variant::Dict(dict) = unwrap_variant(field) {
        for (key, value) in dict.iter() {
            let Some(key) = as_string(key) else {
                continue;
            };
            match key.as_str() {
                "label" => label = as_string(value).unwrap_or_default(),
                "enabled" => enabled = as_bool(value).unwrap_or(true),
                "visible" => visible = as_bool(value).unwrap_or(true),
                "type" => is_separator = as_string(value).as_deref() == Some("separator"),
                _ => {}
            }
        }
        }
    }

    let mut children = Vec::new();
    if let Some(field) = fields.get(2) {
        if let Variant::Array(array) = unwrap_variant(field) {
            for child in array.iter() {
                if let Variant::Structure(structure) = unwrap_variant(child) {
                    if let Some(entry) = entry_from_structure(structure) {
                        if entry.visible {
                            children.push(entry);
                        }
                    }
                }
            }
        }
    }

    Some(TrayMenuEntry {
        id,
        label,
        enabled,
        visible,
        is_separator,
        children,
    })
}

fn unwrap_variant<'a>(value: &'a Variant<'a>) -> &'a Variant<'a> {
    match value {
        Variant::Variant(inner) => unwrap_variant(inner),
        other => other,
    }
}

fn as_string(value: &Variant<'_>) -> Option<String> {
    match unwrap_variant(value) {
        Variant::Str(text) => Some(text.as_str().to_owned()),
        Variant::ObjectPath(path) => Some(path.as_str().to_owned()),
        _ => None,
    }
}

fn as_bool(value: &Variant<'_>) -> Option<bool> {
    match unwrap_variant(value) {
        Variant::Bool(value) => Some(*value),
        _ => None,
    }
}

async fn property_string(proxy: &Proxy<'_>, name: &str) -> Option<String> {
    proxy.get_property::<String>(name).await.ok()
}

/// `serviço/caminho` → (destino, caminho). Sem barra, usa o caminho padrão do
/// protocolo.
fn resolve_item(raw: &str) -> (String, String) {
    match raw.find('/') {
        Some(index) => (raw[..index].to_owned(), raw[index..].to_owned()),
        None => (raw.to_owned(), ITEM_DEFAULT_PATH.to_owned()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_service_and_path() {
        assert_eq!(
            resolve_item(":1.89/StatusNotifierItem"),
            (":1.89".to_owned(), "/StatusNotifierItem".to_owned())
        );
        assert_eq!(
            resolve_item("org.kde.StatusNotifierItem-1-1"),
            (
                "org.kde.StatusNotifierItem-1-1".to_owned(),
                "/StatusNotifierItem".to_owned()
            )
        );
    }

}
