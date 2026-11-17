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
}
