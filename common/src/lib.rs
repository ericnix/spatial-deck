//! Contrato IPC compartilhado entre o compositor nativo e o frontend WASM.
//!
//! O protocolo é JSON com um campo `type` que funciona como discriminante.
//! Exemplos:
//!
//! ```json
//! {"type":"canvas_transform","scale":1.25,"offset_x":120.0,"offset_y":-40.0}
//! {"type":"auto_maximize","window_id":3,"state":true}
//! {"type":"set_theme","theme":"oxidized-iron"}
//! ```
//!
//! A escolha de um enum internamente marcado mantém o contrato simples de
//! consumir tanto em Rust (com `serde_json`) quanto em JavaScript puro.

use serde::{Deserialize, Serialize};

pub mod theme;

pub use theme::CanvasTheme;

/// Faixa superior ocupada pelo HUD, em px lógicos. A visão geral não desenha
/// miniaturas sob ela.
pub const OVERVIEW_TOP: i32 = 78;
/// Faixa inferior ocupada pelo deck.
pub const OVERVIEW_BOTTOM: i32 = 96;
/// Margem lateral da grade da visão geral.
pub const OVERVIEW_MARGIN_X: i32 = 32;
/// Espaço entre as células da grade.
pub const OVERVIEW_GAP: i32 = 16;

/// Mensagens trocadas pelo compositor, pelo socket Unix e pelo WebSocket.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum IpcMessage {
    /// Nova transformação do canvas: `screen = world * scale + offset`.
    CanvasTransform {
        scale: f64,
        offset_x: f64,
        offset_y: f64,
    },

    /// Posições dos nós (janelas) no espaço de mundo.
    ClusterUpdate { placements: Vec<ClusterPlacement> },

    /// Liga/desliga a ferramenta de redimensionamento do deck. Com ela ativa,
    /// arrastar sobre uma janela redimensiona em vez de entregar o clique.
    WindowResizeTool { active: bool },

    /// Áreas do chrome do shell que devem ficar acima das janelas reais.
    ChromeRects {
        rects: Vec<ChromeRect>,
        /// Borda inferior da barra superior: as janelas são cortadas nessa linha.
        hud_bottom: f64,
    },

    /// Pedido para maximizar/restaurar uma janela pelo id numérico.
    AutoMaximize { window_id: u32, state: bool },

    /// Troca do tema visual do frontend/compositor.
    SetTheme { theme: CanvasTheme },

    /// Abre um aplicativo `.desktop` usando o comando `Exec`.
    LaunchApp { exec: String },

    /// Fecha a janela interna correspondente ao id numérico.
    CloseWindow { window_id: u32 },

    /// Traz a janela para o topo da pilha do canvas.
    RaiseWindow { window_id: u32 },

    /// Envia a janela para o fundo da pilha do canvas.
    LowerWindow { window_id: u32 },

    /// Notifica o frontend de que a janela interna foi fechada/destruída.
    WindowClosed { window_id: u32 },

    /// Lista de aplicativos `.desktop` enviada assim que um cliente conecta.
    AppList { apps: Vec<DesktopApp> },

    /// Pedido do compositor para ajustar o zoom do canvas (atalhos de
    /// teclado). O frontend aplica em torno do centro da área visível.
    ZoomCanvas { action: CanvasZoom },

    /// Alterna o modo de visão geral (miniaturas de todas as janelas).
    ToggleOverview,

    /// Estado atual da visão geral, enviado pelo compositor para o HUD.
    /// `windows` é quantas janelas Wayland reais entraram na grade; com zero
    /// o frontend mostra os cartões de demonstração do shell.
    OverviewState { active: bool, windows: usize },

    /// Aviso curto do compositor para o shell (toast do HUD). Usado quando
    /// uma ação não tem efeito, como abrir a visão geral sem janelas reais.
    Toast { message: String },

    /// Notificação entregue pelo `shell-services` ao shell (popup + histórico).
    Notification {
        id: u32,
        app_name: String,
        summary: String,
        body: String,
        /// Nome de ícone freedesktop ou caminho absoluto.
        icon: String,
        /// 0 = baixa, 1 = normal, 2 = crítica.
        urgency: u8,
        #[serde(default)]
        actions: Vec<NotificationButton>,
        /// Milissegundos até expirar: `0` nunca, `-1` usa o padrão do servidor.
        timeout: i32,
        /// Id da notificação substituída; `0` quando é nova.
        replaces_id: u32,
    },

    /// Notificação fechada (expirou, foi descartada ou fechada por chamada).
    NotificationClosed { id: u32, reason: u32 },

    /// Ação de uma notificação acionada no shell.
    NotificationAction { id: u32, action: String },

    /// Pausa/retoma o prazo de uma notificação (hover no popup). Sem isso o
    /// daemon fecharia a notificação por baixo do ponteiro.
    NotificationHold { id: u32, held: bool },

    /// "Não perturbe" ligado/desligado. O estado é o resultado, não um toggle,
    /// para que a mensagem possa ser reentregue sem efeitos colaterais.
    DndState { active: bool },

    /// Histórico completo de notificações, enviado pelo `shell-services`.
    NotificationHistory { items: Vec<NotificationItem> },

    /// Pedido do shell para receber histórico e DND ao abrir a página.
    NotificationHistoryRequest,

    /// Regiões do canvas (viewpoints nomeados), enviadas pelos serviços.
    RegionList { regions: Vec<CanvasRegion> },

    /// Pedido do shell para receber as regiões persistidas.
    RegionListRequest,

    /// Salva a transformação atual como uma região com nome.
    SaveRegion {
        name: String,
        scale: f64,
        offset_x: f64,
        offset_y: f64,
    },

    /// Região criada/atualizada pelos serviços.
    RegionSaved { region: CanvasRegion },

    /// Pede para o shell animar até a região. Também pode vir de um cliente
    /// externo (barra, atalho) e é atendido pelo frontend.
    GotoRegion { id: u32 },

    /// Remove uma região persistida.
    DeleteRegion { id: u32 },

    /// Itens da bandeja (StatusNotifierItem) após uma mudança.
    TrayUpdate { items: Vec<TrayItem> },

    /// Clique esquerdo num item da bandeja.
    TrayActivate { id: String },

    /// Clique do meio num item (SecondaryActivate).
    TraySecondaryActivate { id: String },

    /// Pedido do menu de contexto de um item.
    TrayMenuRequest { id: String },

    /// Pedido do shell para receber o estado atual da bandeja.
    TrayRequest,

    /// Layout do menu de um item (dbusmenu GetLayout).
    TrayMenu { id: String, items: Vec<TrayMenuEntry> },

    /// Clique num item do menu (dbusmenu Event).
    TrayMenuEvent { id: String, event_id: i32 },

    /// Estado do player MPRIS ativo (vazio quando não há player).
    MediaState {
        player: String,
        title: String,
        artist: String,
        status: String,
        can_next: bool,
        can_prev: bool,
    },

    /// Comando para o player MPRIS ativo.
    MediaCommand { command: MediaAction },

    /// Pedido do shell para receber o estado atual do player.
    MediaRequest,

    /// Pedido único do shell para receber rede, bateria, volume e brilho.
    StatusRequest,

    /// Estado da rede (NetworkManager).
    NetworkState {
        connected: bool,
        /// `wifi`, `ethernet`, `vpn`, `bridge`, `other` ou `offline`.
        kind: String,
        ssid: String,
        /// 0–100 (0 quando não se aplica).
        strength: u8,
        /// `WirelessEnabled` do NetworkManager.
        wifi_enabled: bool,
        /// `NetworkingEnabled` do NetworkManager.
        networking_enabled: bool,
    },

    /// Liga/desliga o rádio Wi-Fi.
    NetworkSetWifi { enabled: bool },

    /// Estado da bateria (UPower `DisplayDevice`).
    BatteryState {
        present: bool,
        percent: u8,
        charging: bool,
        /// Minutos restantes (0 quando desconhecido/carregando).
        time_left: u32,
    },

    /// Volume do sink padrão.
    VolumeState {
        available: bool,
        percent: u8,
        muted: bool,
    },

    /// Brilho do backlight.
    BrightnessState {
        available: bool,
        percent: u8,
    },

    /// Comando de volume (mudo, +/-5% ou valor absoluto).
    VolumeCommand {
        action: VolumeAction,
        value: u8,
    },

    /// Comando de brilho.
    BrightnessCommand {
        action: BrightnessAction,
        value: u8,
    },

    /// Estado do adaptador Bluetooth (BlueZ).
    BluetoothState {
        available: bool,
        enabled: bool,
        name: String,
    },

    /// Liga/desliga o adaptador Bluetooth.
    BluetoothSetEnabled { enabled: bool },

    /// Perfis de energia (power-profiles-daemon).
    PowerProfileState {
        available: bool,
        active: String,
        profiles: Vec<String>,
    },

    /// Troca o perfil de energia ativo.
    PowerProfileSet { profile: String },

    /// OSD transitório de volume/brilho.
    Osd {
        /// `volume` ou `brightness`.
        kind: String,
        percent: u8,
        muted: bool,
    },

    /// Pedido do shell para receber as configurações persistidas.
    SettingsRequest,

    /// Configurações persistidas (o `shell-services` é a fonte única).
    SettingsState {
        theme: Option<String>,
        /// JSON do layout da barra, opaco para os serviços.
        bar: Option<serde_json::Value>,
        night_light: bool,
    },

    /// Alteração de uma configuração (`theme`, `bar`, `night-light`).
    SetSetting {
        key: String,
        value: serde_json::Value,
    },
}

/// Ações de volume aceitas pelo `shell-services`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VolumeAction {
    Up,
    Down,
    Mute,
    Set,
}

/// Ações de brilho aceitas pelo `shell-services`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BrightnessAction {
    Up,
    Down,
    Set,
}

/// Item da bandeja do sistema (StatusNotifierItem).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TrayItem {
    /// Identificador estável: `serviço/caminho` do item no barramento.
    pub id: String,
    pub title: String,
    pub icon_name: String,
    /// `Passive`, `Active` ou `NeedsAttention`.
    pub status: String,
    pub has_menu: bool,
    /// `true` quando o clique esquerdo deve abrir o menu.
    pub item_is_menu: bool,
}

/// Entrada do menu de contexto de um item (dbusmenu).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TrayMenuEntry {
    pub id: i32,
    pub label: String,
    pub enabled: bool,
    pub visible: bool,
    pub is_separator: bool,
    #[serde(default)]
    pub children: Vec<TrayMenuEntry>,
}

/// Comandos aceitos pelo player MPRIS ativo.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MediaAction {
    PlayPause,
    Next,
    Previous,
}

/// Ações de zoom pedidas pelo compositor ao frontend.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CanvasZoom {
    In,
    Out,
    Reset,
    Fit,
}

/// Entrada de aplicativo encontrada em `/usr/share/applications` ou
/// `~/.local/share/applications`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DesktopApp {
    pub name: String,
    pub exec: String,
    pub icon: String,
}

/// Botão de ação de uma notificação, na ordem em que o cliente o enviou
/// (pares `id`/`label` do protocolo `org.freedesktop.Notifications`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NotificationButton {
    pub key: String,
    pub label: String,
}

/// Notificação guardada no histórico do shell.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NotificationItem {
    pub id: u32,
    pub app_name: String,
    pub summary: String,
    pub body: String,
    pub icon: String,
    pub urgency: u8,
    #[serde(default)]
    pub actions: Vec<NotificationButton>,
    /// Instante de chegada, em milissegundos desde a epoch Unix.
    pub timestamp_ms: u64,
}

/// Motivos de fechamento do protocolo de notificações.
pub const NOTIFICATION_CLOSED_EXPIRED: u32 = 1;
pub const NOTIFICATION_CLOSED_DISMISSED: u32 = 2;
pub const NOTIFICATION_CLOSED_BY_CALL: u32 = 3;

/// Região do canvas: um enquadramento nomeado (`screen = world * scale +
/// offset`). Substitui o conceito de workspaces no modelo espacial.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CanvasRegion {
    pub id: u32,
    pub name: String,
    pub scale: f64,
    pub offset_x: f64,
    pub offset_y: f64,
}

impl CanvasRegion {
    /// Distância aproximada entre a região e uma transformação atual, em
    /// pixels de tela. Serve para destacar a região ativa na barra.
    pub fn distance(&self, scale: f64, offset_x: f64, offset_y: f64) -> f64 {
        let scale_penalty = ((self.scale - scale).abs() * 1_000.0).min(400.0);
        scale_penalty
            + ((self.offset_x - offset_x).powi(2) + (self.offset_y - offset_y).powi(2)).sqrt()
    }
}

/// Caminho do socket IPC nativo do compositor (JSON por linha).
pub const IPC_SOCKET_PATH: &str = "/tmp/spatial-deck.sock";

/// Calcula a pontuação de busca do launcher. Quanto maior, melhor.
///
/// A ordem de preferência é: nome exato, prefixo do nome, trecho do nome e,
/// por fim, subsequência no nome/comando. Retorna `None` quando não casa.
pub fn fuzzy_score(query: &str, name: &str, exec: &str) -> Option<i32> {
    let query = query.trim().to_lowercase();
    if query.is_empty() {
        return Some(0);
    }

    let name_lower = name.to_lowercase();
    let exec_lower = exec.to_lowercase();
    let mut best: Option<i32> = None;

    if name_lower == query {
        best = Some(1000);
    } else if name_lower.starts_with(&query) {
        best = Some(800 - (name_lower.len() as i32 - query.len() as i32).min(100));
    } else if let Some(position) = name_lower.find(&query) {
        best = Some(650 - position.min(150) as i32);
    } else if let Some(score) = subsequence_score(&query, &name_lower) {
        best = Some(500 + score);
    }

    if best.is_none() {
        if exec_lower.starts_with(&query) {
            best = Some(350);
        } else if let Some(position) = exec_lower.find(&query) {
            best = Some(280 - position.min(120) as i32);
        } else if let Some(score) = subsequence_score(&query, &exec_lower) {
            best = Some(180 + score);
        }
    }

    best
}

/// Pontua uma subsequência contígua possível, premiando caracteres
/// consecutivos e início de palavra. Retorna `None` se algum caractere faltar.
fn subsequence_score(query: &str, haystack: &str) -> Option<i32> {
    let haystack_chars: Vec<char> = haystack.chars().collect();
    let mut cursor = 0usize;
    let mut score = 0i32;
    let mut previous: Option<usize> = None;

    for wanted in query.chars() {
        let position = haystack_chars[cursor..]
            .iter()
            .position(|candidate| *candidate == wanted)?
            + cursor;
        score -= (position - cursor) as i32 * 2;
        if previous.is_some_and(|last| last + 1 == position) {
            score += 25;
        }
        if position == 0
            || haystack_chars
                .get(position.saturating_sub(1))
                .is_some_and(|character| !character.is_alphanumeric())
        {
            score += 20;
        }
        previous = Some(position);
        cursor = position + 1;
    }

    Some(score)
}

/// Liga/desliga um comando dos favoritos. Devolve o novo estado.
pub fn toggle_favorite(favorites: &mut Vec<String>, exec: &str) -> bool {
    let exec = exec.trim();
    if exec.is_empty() {
        return false;
    }
    if let Some(position) = favorites.iter().position(|item| item == exec) {
        favorites.remove(position);
        false
    } else {
        favorites.push(exec.to_owned());
        true
    }
}

/// Move o comando para o topo dos recentes, sem duplicar e com limite.
pub fn push_recent(recent: &mut Vec<String>, exec: &str, limit: usize) {
    let exec = exec.trim();
    if exec.is_empty() || limit == 0 {
        return;
    }
    recent.retain(|item| item != exec);
    recent.insert(0, exec.to_owned());
    recent.truncate(limit);
}

/// Retângulo ocupado pelo chrome do shell (HUD, deck, gaveta), em pixels
/// lógicos de tela. Esses pontos ficam sempre por cima das janelas reais.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct ChromeRect {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

/// Posição de um nó no espaço de mundo do canvas.
///
/// Para janelas reais, `x`/`y` são o canto superior esquerdo do *conteúdo* do
/// cliente. O cartão do shell desenha a barra de título logo acima desse
/// ponto, então a barra fica sempre visível e clicável — a superfície da
/// janela é desenhada pelo compositor por cima do corpo do cartão.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ClusterPlacement {
    pub id: String,
    pub x: f64,
    pub y: f64,
    /// Tamanho do conteúdo da janela real; zero em cartões de demonstração.
    #[serde(default)]
    pub w: f64,
    #[serde(default)]
    pub h: f64,
    /// Título informado pelo cliente (vazio quando ainda não há título).
    #[serde(default)]
    pub title: String,
}

impl IpcMessage {
    /// Serializa a mensagem no formato JSON de uma linha.
    pub fn to_json(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string(self)
    }

    /// Lê uma mensagem JSON recebida do frontend ou de um cliente IPC.
    pub fn from_json(input: &str) -> Result<Self, serde_json::Error> {
        serde_json::from_str(input)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canvas_transform_round_trip() {
        let msg = IpcMessage::CanvasTransform {
            scale: 1.5,
            offset_x: 12.0,
            offset_y: -8.0,
        };
        let json = msg.to_json().unwrap();
        assert!(json.contains("\"type\":\"canvas_transform\""));
        assert_eq!(IpcMessage::from_json(&json).unwrap(), msg);
    }

    #[test]
    fn zoom_canvas_round_trip() {
        for (action, name) in [
            (CanvasZoom::In, "in"),
            (CanvasZoom::Out, "out"),
            (CanvasZoom::Reset, "reset"),
            (CanvasZoom::Fit, "fit"),
        ] {
            let msg = IpcMessage::ZoomCanvas { action };
            let json = msg.to_json().unwrap();
            assert!(json.contains("\"type\":\"zoom_canvas\""));
            assert!(json.contains(&format!("\"action\":\"{name}\"")));
            assert_eq!(IpcMessage::from_json(&json).unwrap(), msg);
        }
    }

    #[test]
    fn set_theme_round_trip() {
        let msg = IpcMessage::SetTheme {
            theme: CanvasTheme::Minimalist,
        };
        let json = msg.to_json().unwrap();
        assert!(json.contains("\"theme\":\"minimalist\""));
        assert_eq!(IpcMessage::from_json(&json).unwrap(), msg);
    }

    #[test]
    fn set_dark_minimalist_theme_round_trip() {
        let msg = IpcMessage::SetTheme {
            theme: CanvasTheme::MinimalistDark,
        };
        let json = msg.to_json().unwrap();
        assert!(json.contains("\"theme\":\"minimalist-dark\""));
        assert_eq!(IpcMessage::from_json(&json).unwrap(), msg);
    }

    #[test]
    fn launch_app_round_trip() {
        let msg = IpcMessage::LaunchApp {
            exec: "foot".into(),
        };
        let json = msg.to_json().unwrap();
        assert!(json.contains("\"type\":\"launch_app\""));
        assert_eq!(IpcMessage::from_json(&json).unwrap(), msg);
    }

    #[test]
    fn close_window_round_trip() {
        let msg = IpcMessage::CloseWindow { window_id: 3 };
        let json = msg.to_json().unwrap();
        assert!(json.contains("\"type\":\"close_window\""));
        assert_eq!(IpcMessage::from_json(&json).unwrap(), msg);

        let closed = IpcMessage::WindowClosed { window_id: 3 };
        let json = closed.to_json().unwrap();
        assert!(json.contains("\"type\":\"window_closed\""));
        assert_eq!(IpcMessage::from_json(&json).unwrap(), closed);

        let raise = IpcMessage::RaiseWindow { window_id: 3 };
        let json = raise.to_json().unwrap();
        assert!(json.contains("\"type\":\"raise_window\""));
        assert_eq!(IpcMessage::from_json(&json).unwrap(), raise);

        let lower = IpcMessage::LowerWindow { window_id: 3 };
        let json = lower.to_json().unwrap();
        assert!(json.contains("\"type\":\"lower_window\""));
        assert_eq!(IpcMessage::from_json(&json).unwrap(), lower);
    }

    #[test]
    fn overview_round_trip() {
        let toggle = IpcMessage::ToggleOverview;
        let json = toggle.to_json().unwrap();
        assert!(json.contains("\"type\":\"toggle_overview\""));
        assert_eq!(IpcMessage::from_json(&json).unwrap(), toggle);

        let state = IpcMessage::OverviewState {
            active: true,
            windows: 2,
        };
        let json = state.to_json().unwrap();
        assert!(json.contains("\"type\":\"overview_state\""));
        assert!(json.contains("\"active\":true"));
        assert!(json.contains("\"windows\":2"));
        assert_eq!(IpcMessage::from_json(&json).unwrap(), state);
    }

    #[test]
    fn resize_tool_round_trip() {
        let msg = IpcMessage::WindowResizeTool { active: true };
        let json = msg.to_json().unwrap();
        assert!(json.contains("\"type\":\"window_resize_tool\""));
        assert!(json.contains("\"active\":true"));
        assert_eq!(IpcMessage::from_json(&json).unwrap(), msg);
    }

    #[test]
    fn toast_round_trip() {
        let toast = IpcMessage::Toast {
            message: "Nenhuma janela real aberta".into(),
        };
        let json = toast.to_json().unwrap();
        assert!(json.contains("\"type\":\"toast\""));
        assert!(json.contains("Nenhuma janela real aberta"));
        assert_eq!(IpcMessage::from_json(&json).unwrap(), toast);
    }

    #[test]
    fn fuzzy_search_prefers_exact_prefix_and_finds_typos() {
        let exact = fuzzy_score("firefox", "Firefox", "firefox %u").unwrap();
        let prefix = fuzzy_score("fire", "Firefox", "firefox %u").unwrap();
        let subsequence = fuzzy_score("frfx", "Firefox", "firefox %u").unwrap();
        let exec_only = fuzzy_score("ffx", "Navegador", "firefox %u").unwrap();
        assert!(exact > prefix);
        assert!(prefix > subsequence);
        assert!(subsequence > exec_only);
        assert!(fuzzy_score("zzz", "Firefox", "firefox %u").is_none());
    }

    #[test]
    fn empty_search_matches_everything() {
        assert_eq!(fuzzy_score("  ", "qualquer", "comando"), Some(0));
    }

    #[test]
    fn favorites_toggle_and_do_not_duplicate() {
        let mut favorites = Vec::new();
        assert!(toggle_favorite(&mut favorites, "alacritty"));
        assert_eq!(favorites, vec!["alacritty".to_owned()]);
        assert!(!toggle_favorite(&mut favorites, "alacritty"));
        assert!(favorites.is_empty());
    }

    #[test]
    fn recent_moves_to_front_with_limit() {
        let mut recent = Vec::new();
        push_recent(&mut recent, "a", 2);
        push_recent(&mut recent, "b", 2);
        push_recent(&mut recent, "a", 2);
        assert_eq!(recent, vec!["a".to_owned(), "b".to_owned()]);
        push_recent(&mut recent, "c", 2);
        assert_eq!(recent, vec!["c".to_owned(), "a".to_owned()]);
    }

    #[test]
    fn app_list_round_trip() {
        let msg = IpcMessage::AppList {
            apps: vec![DesktopApp {
                name: "Terminal".into(),
                exec: "foot".into(),
                icon: "utilities-terminal".into(),
            }],
        };
        assert_eq!(
            IpcMessage::from_json(&msg.to_json().unwrap()).unwrap(),
            msg
        );
    }

    #[test]
    fn notification_round_trip() {
        let msg = IpcMessage::Notification {
            id: 7,
            app_name: "Firefox".into(),
            summary: "Download concluído".into(),
            body: "<b>arquivo.iso</b>".into(),
            icon: "firefox".into(),
            urgency: 2,
            actions: vec![
                NotificationButton {
                    key: "default".into(),
                    label: "Abrir".into(),
                },
                NotificationButton {
                    key: "dismiss".into(),
                    label: "Dispensar".into(),
                },
            ],
            timeout: -1,
            replaces_id: 0,
        };
        let json = msg.to_json().unwrap();
        assert!(json.contains("\"type\":\"notification\""));
        assert!(json.contains("\"actions\""));
        assert_eq!(IpcMessage::from_json(&json).unwrap(), msg);
    }

    #[test]
    fn notification_events_round_trip() {
        let closed = IpcMessage::NotificationClosed {
            id: 7,
            reason: NOTIFICATION_CLOSED_EXPIRED,
        };
        let json = closed.to_json().unwrap();
        assert!(json.contains("\"type\":\"notification_closed\""));
        assert_eq!(IpcMessage::from_json(&json).unwrap(), closed);

        let action = IpcMessage::NotificationAction {
            id: 7,
            action: "default".into(),
        };
        let json = action.to_json().unwrap();
        assert!(json.contains("\"type\":\"notification_action\""));
        assert_eq!(IpcMessage::from_json(&json).unwrap(), action);

        let hold = IpcMessage::NotificationHold {
            id: 7,
            held: true,
        };
        let json = hold.to_json().unwrap();
        assert!(json.contains("\"type\":\"notification_hold\""));
        assert_eq!(IpcMessage::from_json(&json).unwrap(), hold);

        let dnd = IpcMessage::DndState { active: true };
        let json = dnd.to_json().unwrap();
        assert!(json.contains("\"type\":\"dnd_state\""));
        assert_eq!(IpcMessage::from_json(&json).unwrap(), dnd);
    }

    #[test]
    fn notification_history_round_trip() {
        let msg = IpcMessage::NotificationHistory {
            items: vec![NotificationItem {
                id: 1,
                app_name: "Terminal".into(),
                summary: "Comando terminou".into(),
                body: "make".into(),
                icon: "utilities-terminal".into(),
                urgency: 1,
                actions: vec![NotificationButton {
                    key: "default".into(),
                    label: "Ver".into(),
                }],
                timestamp_ms: 1_760_000_000_000,
            }],
        };
        let json = msg.to_json().unwrap();
        assert!(json.contains("\"type\":\"notification_history\""));
        assert_eq!(IpcMessage::from_json(&json).unwrap(), msg);

        let request = IpcMessage::NotificationHistoryRequest;
        let json = request.to_json().unwrap();
        assert!(json.contains("\"type\":\"notification_history_request\""));
        assert_eq!(IpcMessage::from_json(&json).unwrap(), request);
    }

    #[test]
    fn region_messages_round_trip() {
        let region = CanvasRegion {
            id: 2,
            name: "Trabalho".into(),
            scale: 1.25,
            offset_x: -340.0,
            offset_y: 120.5,
        };

        let list = IpcMessage::RegionList {
            regions: vec![region.clone()],
        };
        let json = list.to_json().unwrap();
        assert!(json.contains("\"type\":\"region_list\""));
        assert_eq!(IpcMessage::from_json(&json).unwrap(), list);

        let request = IpcMessage::RegionListRequest;
        let json = request.to_json().unwrap();
        assert!(json.contains("\"type\":\"region_list_request\""));
        assert_eq!(IpcMessage::from_json(&json).unwrap(), request);

        let save = IpcMessage::SaveRegion {
            name: "Trabalho".into(),
            scale: 1.25,
            offset_x: -340.0,
            offset_y: 120.5,
        };
        let json = save.to_json().unwrap();
        assert!(json.contains("\"type\":\"save_region\""));
        assert_eq!(IpcMessage::from_json(&json).unwrap(), save);

        let saved = IpcMessage::RegionSaved {
            region: region.clone(),
        };
        let json = saved.to_json().unwrap();
        assert!(json.contains("\"type\":\"region_saved\""));
        assert_eq!(IpcMessage::from_json(&json).unwrap(), saved);

        let goto = IpcMessage::GotoRegion { id: 2 };
        let json = goto.to_json().unwrap();
        assert!(json.contains("\"type\":\"goto_region\""));
        assert_eq!(IpcMessage::from_json(&json).unwrap(), goto);

        let delete = IpcMessage::DeleteRegion { id: 2 };
        let json = delete.to_json().unwrap();
        assert!(json.contains("\"type\":\"delete_region\""));
        assert_eq!(IpcMessage::from_json(&json).unwrap(), delete);
    }

    #[test]
    fn tray_messages_round_trip() {
        let item = TrayItem {
            id: ":1.89/StatusNotifierItem".into(),
            title: "Ponte de vídeo".into(),
            icon_name: "org.kde.xwaylandvideobridge".into(),
            status: "Active".into(),
            has_menu: true,
            item_is_menu: false,
        };
        let update = IpcMessage::TrayUpdate {
            items: vec![item.clone()],
        };
        let json = update.to_json().unwrap();
        assert!(json.contains("\"type\":\"tray_update\""));
        assert_eq!(IpcMessage::from_json(&json).unwrap(), update);

        let activate = IpcMessage::TrayActivate {
            id: item.id.clone(),
        };
        let json = activate.to_json().unwrap();
        assert!(json.contains("\"type\":\"tray_activate\""));
        assert_eq!(IpcMessage::from_json(&json).unwrap(), activate);

        let menu = IpcMessage::TrayMenu {
            id: item.id.clone(),
            items: vec![TrayMenuEntry {
                id: 3,
                label: "Abrir".into(),
                enabled: true,
                visible: true,
                is_separator: false,
                children: vec![TrayMenuEntry {
                    id: 4,
                    label: "Preferências".into(),
                    enabled: false,
                    visible: true,
                    is_separator: false,
                    children: Vec::new(),
                }],
            }],
        };
        let json = menu.to_json().unwrap();
        assert!(json.contains("\"type\":\"tray_menu\""));
        assert_eq!(IpcMessage::from_json(&json).unwrap(), menu);

        let event = IpcMessage::TrayMenuEvent {
            id: item.id,
            event_id: 3,
        };
        let json = event.to_json().unwrap();
        assert!(json.contains("\"type\":\"tray_menu_event\""));
        assert_eq!(IpcMessage::from_json(&json).unwrap(), event);

        let request = IpcMessage::TrayMenuRequest {
            id: "x".into(),
        };
        assert_eq!(
            IpcMessage::from_json(&request.to_json().unwrap()).unwrap(),
            request
        );

        let refresh = IpcMessage::TrayRequest;
        let json = refresh.to_json().unwrap();
        assert!(json.contains("\"type\":\"tray_request\""));
        assert_eq!(IpcMessage::from_json(&json).unwrap(), refresh);
    }

    #[test]
    fn media_messages_round_trip() {
        let state = IpcMessage::MediaState {
            player: "org.mpris.MediaPlayer2.vlc".into(),
            title: "Faixa".into(),
            artist: "Artista".into(),
            status: "Playing".into(),
            can_next: true,
            can_prev: false,
        };
        let json = state.to_json().unwrap();
        assert!(json.contains("\"type\":\"media_state\""));
        assert_eq!(IpcMessage::from_json(&json).unwrap(), state);

        for (action, name) in [
            (MediaAction::PlayPause, "play_pause"),
            (MediaAction::Next, "next"),
            (MediaAction::Previous, "previous"),
        ] {
            let command = IpcMessage::MediaCommand { command: action };
            let json = command.to_json().unwrap();
            assert!(json.contains(&format!("\"command\":\"{name}\"")));
            assert_eq!(IpcMessage::from_json(&json).unwrap(), command);
        }

        let refresh = IpcMessage::MediaRequest;
        let json = refresh.to_json().unwrap();
        assert!(json.contains("\"type\":\"media_request\""));
        assert_eq!(IpcMessage::from_json(&json).unwrap(), refresh);
    }

    #[test]
    fn system_messages_round_trip() {
        let network = IpcMessage::NetworkState {
            connected: true,
            kind: "wifi".into(),
            ssid: "Casa".into(),
            strength: 78,
            wifi_enabled: true,
            networking_enabled: true,
        };
        let json = network.to_json().unwrap();
        assert!(json.contains("\"type\":\"network_state\""));
        assert_eq!(IpcMessage::from_json(&json).unwrap(), network);

        let battery = IpcMessage::BatteryState {
            present: true,
            percent: 85,
            charging: false,
            time_left: 120,
        };
        let json = battery.to_json().unwrap();
        assert!(json.contains("\"type\":\"battery_state\""));
        assert_eq!(IpcMessage::from_json(&json).unwrap(), battery);

        let volume = IpcMessage::VolumeState {
            available: true,
            percent: 70,
            muted: false,
        };
        let json = volume.to_json().unwrap();
        assert!(json.contains("\"type\":\"volume_state\""));
        assert_eq!(IpcMessage::from_json(&json).unwrap(), volume);

        let brightness = IpcMessage::BrightnessState {
            available: false,
            percent: 0,
        };
        let json = brightness.to_json().unwrap();
        assert!(json.contains("\"type\":\"brightness_state\""));
        assert_eq!(IpcMessage::from_json(&json).unwrap(), brightness);

        let command = IpcMessage::VolumeCommand {
            action: VolumeAction::Set,
            value: 40,
        };
        let json = command.to_json().unwrap();
        assert!(json.contains("\"action\":\"set\""));
        assert_eq!(IpcMessage::from_json(&json).unwrap(), command);

        let command = IpcMessage::BrightnessCommand {
            action: BrightnessAction::Down,
            value: 0,
        };
        let json = command.to_json().unwrap();
        assert!(json.contains("\"type\":\"brightness_command\""));
        assert_eq!(IpcMessage::from_json(&json).unwrap(), command);

        let request = IpcMessage::StatusRequest;
        let json = request.to_json().unwrap();
        assert!(json.contains("\"type\":\"status_request\""));
        assert_eq!(IpcMessage::from_json(&json).unwrap(), request);
    }

    #[test]
    fn control_center_messages_round_trip() {
        let wifi = IpcMessage::NetworkSetWifi { enabled: false };
        let json = wifi.to_json().unwrap();
        assert!(json.contains("\"type\":\"network_set_wifi\""));
        assert_eq!(IpcMessage::from_json(&json).unwrap(), wifi);

        let bluetooth = IpcMessage::BluetoothState {
            available: true,
            enabled: false,
            name: "hci0".into(),
        };
        let json = bluetooth.to_json().unwrap();
        assert!(json.contains("\"type\":\"bluetooth_state\""));
        assert_eq!(IpcMessage::from_json(&json).unwrap(), bluetooth);

        let set = IpcMessage::BluetoothSetEnabled { enabled: true };
        let json = set.to_json().unwrap();
        assert!(json.contains("\"type\":\"bluetooth_set_enabled\""));
        assert_eq!(IpcMessage::from_json(&json).unwrap(), set);

        let profiles = IpcMessage::PowerProfileState {
            available: true,
            active: "balanced".into(),
            profiles: vec!["power-saver".into(), "balanced".into(), "performance".into()],
        };
        let json = profiles.to_json().unwrap();
        assert!(json.contains("\"type\":\"power_profile_state\""));
        assert_eq!(IpcMessage::from_json(&json).unwrap(), profiles);

        let set = IpcMessage::PowerProfileSet {
            profile: "performance".into(),
        };
        let json = set.to_json().unwrap();
        assert!(json.contains("\"type\":\"power_profile_set\""));
        assert_eq!(IpcMessage::from_json(&json).unwrap(), set);

        let osd = IpcMessage::Osd {
            kind: "volume".into(),
            percent: 65,
            muted: false,
        };
        let json = osd.to_json().unwrap();
        assert!(json.contains("\"type\":\"osd\""));
        assert_eq!(IpcMessage::from_json(&json).unwrap(), osd);

        let settings = IpcMessage::SettingsState {
            theme: Some("nord".into()),
            bar: Some(serde_json::json!({ "hour24": true })),
            night_light: false,
        };
        let json = settings.to_json().unwrap();
        assert!(json.contains("\"type\":\"settings_state\""));
        assert_eq!(IpcMessage::from_json(&json).unwrap(), settings);

        let set = IpcMessage::SetSetting {
            key: "theme".into(),
            value: serde_json::json!("nord"),
        };
        let json = set.to_json().unwrap();
        assert!(json.contains("\"type\":\"set_setting\""));
        assert_eq!(IpcMessage::from_json(&json).unwrap(), set);

        let request = IpcMessage::SettingsRequest;
        let json = request.to_json().unwrap();
        assert!(json.contains("\"type\":\"settings_request\""));
        assert_eq!(IpcMessage::from_json(&json).unwrap(), request);
    }

    #[test]
    fn region_distance_prefers_near_transform() {
        let region = CanvasRegion {
            id: 1,
            name: "A".into(),
            scale: 1.0,
            offset_x: 0.0,
            offset_y: 0.0,
        };
        assert_eq!(region.distance(1.0, 0.0, 0.0), 0.0);
        assert!(region.distance(1.0, 30.0, 40.0) > 49.0);
        assert!(region.distance(2.0, 0.0, 0.0) >= 400.0);
    }
}
