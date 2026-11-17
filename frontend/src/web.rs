//! Lógica de navegador do frontend WASM.
//!
//! Responsabilidades:
//! - pan/zoom do canvas (roda do mouse + arrasto);
//! - grade desenhada num `<canvas>` HTML5;
//! - moldura `#active-window-glow` acompanhando o cartão ativo;
//! - cinco temas (`default`, `minimalist`, `minimalist-dark`, `oxidized-iron`
//!   e `nord`);
//! - visão geral das janelas reais (botão do HUD + `Ctrl+Alt+O`);
//! - IPC WebSocket com o compositor;
//! - `AutoMaximize` quando um cartão ocupa >= 90% do viewport.

use std::{
    cell::{Cell, RefCell},
    collections::HashSet,
};

use common::{
    fuzzy_score, push_recent, toggle_favorite, CanvasRegion, CanvasTheme, CanvasZoom, ChromeRect,
    ClusterPlacement, DesktopApp, IpcMessage, MediaAction, NotificationButton, NotificationItem,
    TrayItem, TrayMenuEntry, NOTIFICATION_CLOSED_DISMISSED, NOTIFICATION_CLOSED_EXPIRED,
};
use serde::{Deserialize, Serialize};
use wasm_bindgen::{closure::Closure, prelude::*, JsCast};
use web_sys::{
    CanvasRenderingContext2d, CloseEvent, Document, DomRect, Element, Event, HtmlCanvasElement,
    HtmlInputElement, HtmlSelectElement, KeyboardEvent, MessageEvent, PointerEvent, WebSocket,
    WheelEvent, Window,
};

use crate::{drawing, minimalist_dark_theme, minimalist_theme};

const WS_URL: &str = "ws://127.0.0.1:7337";
const THEME_KEY: &str = "spatial-deck.theme";
const FAVORITES_KEY: &str = "spatial-deck.favorites";
const RECENT_KEY: &str = "spatial-deck.recent";
const RECENT_LIMIT: usize = 10;
/// Histórico de notificações mantido na interface.
const HISTORY_LIMIT: usize = 100;
/// Configuração da barra (ordem/slots e formato do relógio).
const BAR_KEY: &str = "spatial-deck.bar";

/// Módulos da barra: id, rótulo e slot padrão.
const BAR_MODULES: [(&str, &str, &str); 9] = [
    ("regions", "Regiões", "left"),
    ("clock", "Relógio", "center"),
    ("media", "Mídia", "center"),
    ("status", "Status", "left"),
    ("themes", "Temas", "right"),
    ("zoom", "Zoom", "right"),
    ("tray", "Bandeja", "right"),
    ("notifications", "Notificações", "right"),
    ("overview", "Visão geral", "right"),
];

const WEEKDAYS: [&str; 7] = ["dom", "seg", "ter", "qua", "qui", "sex", "sáb"];
const MONTHS: [&str; 12] = [
    "jan", "fev", "mar", "abr", "mai", "jun", "jul", "ago", "set", "out", "nov", "dez",
];
const MONTHS_FULL: [&str; 12] = [
    "Janeiro",
    "Fevereiro",
    "Março",
    "Abril",
    "Maio",
    "Junho",
    "Julho",
    "Agosto",
    "Setembro",
    "Outubro",
    "Novembro",
    "Dezembro",
];
const THEMES: [CanvasTheme; 5] = CanvasTheme::ALL;

/// Altura da barra de título de uma janela real: 1px de borda + 38px da barra.
const CARD_BAR: f64 = 39.0;

#[derive(Debug, Clone, Copy)]
struct Card {
    id: u32,
    x: f64,
    y: f64,
    w: f64,
    h: f64,
    /// `true` quando uma janela real ocupa este id: o cartão vira só um
    /// placeholder invisível, porque o compositor desenha a janela por cima.
    has_window: bool,
}

// Posições de mundo dos cartões de demonstração. Os mesmos ids são usados
// pelo compositor para as janelas xdg_toplevel reais.
const CARDS: [Card; 4] = [
    Card { id: 1, x: 90.0, y: 90.0, w: 720.0, h: 480.0, has_window: false },
    Card { id: 2, x: 880.0, y: 130.0, w: 520.0, h: 360.0, has_window: false },
    Card { id: 3, x: 180.0, y: 640.0, w: 620.0, h: 420.0, has_window: false },
    Card { id: 4, x: 920.0, y: 600.0, w: 460.0, h: 320.0, has_window: false },
];

/// Módulo da barra com o slot onde ele aparece.
#[derive(Clone, Debug, Serialize, Deserialize)]
struct BarModule {
    id: String,
    slot: String,
}

/// Configuração persistida em `localStorage` (`spatial-deck.bar`).
#[derive(Clone, Debug, Serialize, Deserialize)]
struct BarConfig {
    #[serde(default = "default_hour24")]
    hour24: bool,
    #[serde(default)]
    modules: Vec<BarModule>,
}

fn default_hour24() -> bool {
    true
}

impl Default for BarConfig {
    fn default() -> Self {
        Self {
            hour24: true,
            modules: BAR_MODULES
                .iter()
                .map(|(id, _, slot)| BarModule {
                    id: (*id).to_owned(),
                    slot: (*slot).to_owned(),
                })
                .collect(),
        }
    }
}

/// Animação de transformação ao trocar de região (250 ms, smoothstep).
struct TransformAnim {
    start_ms: f64,
    from: (f64, f64, f64),
    to: (f64, f64, f64),
}

/// Popup visível de uma notificação, com o prazo individual.
struct PopupState {
    item: NotificationItem,
    /// Instante (`Date::now`, em ms) em que o popup some; `None` não expira.
    deadline: Option<f64>,
    /// Tempo restante congelado enquanto o ponteiro está sobre o popup.
    remaining: Option<f64>,
    paused: bool,
}

struct State {
    scale: f64,
    offset_x: f64,
    offset_y: f64,
    dragging: bool,
    /// Pan temporário com o botão do meio (funciona em qualquer ferramenta).
    middle_pan: bool,
    /// Cartão sendo arrastado (None quando o gesto é pan do canvas).
    drag_card: Option<u32>,
    last_x: f64,
    last_y: f64,
    active: Option<u32>,
    theme: CanvasTheme,
    maximized: HashSet<u32>,
    viewport_w: f64,
    viewport_h: f64,
    apps: Vec<DesktopApp>,
    app_query: String,
    /// Comandos fixados pelo usuário, na ordem em que foram marcados.
    favorites: Vec<String>,
    /// Comandos usados recentemente, do mais recente para o mais antigo.
    recent: Vec<String>,
    /// Posições atuais dos cartões no espaço de mundo.
    cards: Vec<Card>,
    /// Visão geral sem janelas reais: os cartões do shell formam a grade.
    card_overview: bool,
    /// Ferramenta do deck que redimensiona janelas reais ao arrastar.
    resize_tool: bool,
    /// Histórico de notificações (mais recente primeiro).
    notifications: Vec<NotificationItem>,
    /// Popups empilhados no canto superior direito.
    popups: Vec<PopupState>,
    /// "Não perturbe": silencia popups de urgência normal/baixa.
    dnd: bool,
    /// Painel de histórico aberto.
    notif_panel_open: bool,
    /// Configuração da barra (slots/ordem e relógio 24h).
    bar: BarConfig,
    /// Regiões do canvas recebidas dos serviços.
    regions: Vec<CanvasRegion>,
    /// Animação de transformação em andamento.
    anim: Option<TransformAnim>,
    /// Mês exibido no calendário (0-11) e ano.
    calendar_year: i32,
    calendar_month: u32,
    /// Itens da bandeja (StatusNotifierItem).
    tray_items: Vec<TrayItem>,
    /// Item cujo menu de contexto está aberto.
    tray_menu_owner: Option<String>,
    /// Estado do player MPRIS ativo (vazio quando não há player).
    media_player: String,
    media_title: String,
    media_artist: String,
    media_status: String,
    media_can_next: bool,
    media_can_prev: bool,
}

impl Default for State {
    fn default() -> Self {
        Self {
            scale: 1.0,
            offset_x: 0.0,
            offset_y: 0.0,
            dragging: false,
            middle_pan: false,
            drag_card: None,
            last_x: 0.0,
            last_y: 0.0,
            active: None,
            theme: CanvasTheme::Default,
            maximized: HashSet::new(),
            viewport_w: 1280.0,
            viewport_h: 720.0,
            apps: Vec::new(),
            app_query: String::new(),
            favorites: Vec::new(),
            recent: Vec::new(),
            cards: CARDS.to_vec(),
            card_overview: false,
            resize_tool: false,
            notifications: Vec::new(),
            popups: Vec::new(),
            dnd: false,
            notif_panel_open: false,
            bar: BarConfig::default(),
            regions: Vec::new(),
            anim: None,
            calendar_year: 0,
            calendar_month: 0,
            tray_items: Vec::new(),
            tray_menu_owner: None,
            media_player: String::new(),
            media_title: String::new(),
            media_artist: String::new(),
            media_status: String::new(),
            media_can_next: false,
            media_can_prev: false,
        }
    }
}

thread_local! {
    static STATE: RefCell<State> = RefCell::new(State::default());
    static SOCKET: RefCell<Option<WebSocket>> = const { RefCell::new(None) };
    static TOAST_GENERATION: Cell<u32> = const { Cell::new(0) };
}

#[wasm_bindgen]
pub fn start() -> Result<(), JsValue> {
    let window = web_sys::window().ok_or_else(|| JsValue::from_str("window ausente"))?;
    let document = window
        .document()
        .ok_or_else(|| JsValue::from_str("document ausente"))?;

    apply_initial_theme(&window, &document)?;
    load_launcher_state(&window);
    setup_theme_buttons(&window, &document)?;
    setup_bar(&window, &document)?;
    setup_canvas(&window, &document)?;
    setup_pan_zoom(&window, &document)?;
    setup_zoom_controls(&window, &document)?;
    setup_resize_tool(&document)?;
    setup_overview_control(&document)?;
    setup_cards(&document)?;
    setup_deck_apps(&window, &document)?;
    setup_notifications(&window, &document)?;
    setup_tray_media(&document)?;
    drawing::init(&document)?;
    connect_websocket();

    apply_transform(&document)?;
    draw_grid(&window, &document)?;
    Ok(())
}

fn document_element(document: &Document) -> Result<Element, JsValue> {
    document
        .document_element()
        .ok_or_else(|| JsValue::from_str("documentElement ausente"))
}

fn by_id(document: &Document, id: &str) -> Result<Element, JsValue> {
    document
        .get_element_by_id(id)
        .ok_or_else(|| JsValue::from_str(&format!("elemento #{id} ausente")))
}

fn set_text(document: &Document, id: &str, text: &str) -> Result<(), JsValue> {
    by_id(document, id)?.set_inner_html(&escape_html(text));
    Ok(())
}

fn escape_html(input: &str) -> String {
    input
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

// ---------------------------------------------------------------------------
// Temas
// ---------------------------------------------------------------------------

fn apply_initial_theme(window: &Window, document: &Document) -> Result<(), JsValue> {
    let mut theme = CanvasTheme::Default;

    if let Ok(Some(storage)) = window.local_storage() {
        if let Ok(Some(saved)) = storage.get_item(THEME_KEY) {
            if let Some(candidate) = CanvasTheme::from_name(&saved) {
                theme = candidate;
            }
        }
    }

    if let Ok(search) = window.location().search() {
        for part in search.trim_start_matches('?').split('&') {
            if let Some(value) = part.strip_prefix("theme=") {
                if let Some(candidate) = CanvasTheme::from_name(value) {
                    theme = candidate;
                }
            }
        }
    }

    apply_theme_to_document(document, theme)
}

fn apply_theme_to_document(document: &Document, theme: CanvasTheme) -> Result<(), JsValue> {
    STATE.with(|state| state.borrow_mut().theme = theme);

    let root = document_element(document)?;
    root.set_attribute("data-canvas-theme", theme.as_str())?;
    minimalist_theme::set_active(document, theme == CanvasTheme::Minimalist)?;
    minimalist_dark_theme::set_active(document, theme == CanvasTheme::MinimalistDark)?;
    sync_theme_buttons(document, theme);
    Ok(())
}

fn setup_theme_buttons(window: &Window, document: &Document) -> Result<(), JsValue> {
    for theme in THEMES {
        let button = by_id(document, &format!("theme-{}", theme.as_str()))?;
        let window = window.clone();
        let document = document.clone();

        let callback = Closure::<dyn FnMut()>::new(move || {
            let _ = set_theme(&window, &document, theme);
        });
        button.add_event_listener_with_callback("click", callback.as_ref().unchecked_ref())?;
        callback.forget();
    }
    Ok(())
}

fn sync_theme_buttons(document: &Document, active: CanvasTheme) {
    for theme in THEMES {
        if let Some(button) = document.get_element_by_id(&format!("theme-{}", theme.as_str())) {
            let pressed = if theme == active { "true" } else { "false" };
            let _ = button.set_attribute("aria-pressed", pressed);
        }
    }
    if let Some(label) = document.get_element_by_id("theme-label") {
        let text = match active {
            CanvasTheme::Default => "Padrão",
            CanvasTheme::Minimalist => "Minimalista",
            CanvasTheme::MinimalistDark => "Minimalista Negro",
            CanvasTheme::OxidizedIron => "Ferro Oxidado",
            CanvasTheme::Nord => "Nord",
        };
        let _ = label.set_text_content(Some(text));
    }
}

fn set_theme(window: &Window, document: &Document, theme: CanvasTheme) -> Result<(), JsValue> {
    apply_theme_to_document(document, theme)?;

    if let Ok(Some(storage)) = window.local_storage() {
        let _ = storage.set_item(THEME_KEY, theme.as_str());
    }

    draw_grid(window, document)?;
    send_message(&IpcMessage::SetTheme { theme });
    Ok(())
}

// ---------------------------------------------------------------------------
// Canvas / grade
// ---------------------------------------------------------------------------

fn setup_canvas(window: &Window, document: &Document) -> Result<(), JsValue> {
    resize_canvas(window, document)?;

    let window_for_closure = window.clone();
    let document_for_closure = document.clone();
    let callback = Closure::<dyn FnMut()>::new(move || {
        let _ = resize_canvas(&window_for_closure, &document_for_closure);
        let _ = draw_grid(&window_for_closure, &document_for_closure);
        let _ = apply_transform(&document_for_closure);
        let card_overview = STATE.with(|state| state.borrow().card_overview);
        if card_overview {
            let _ = apply_card_overview(&document_for_closure, true);
        }
        send_chrome_rects(&document_for_closure);
    });
    window.add_event_listener_with_callback(
        "resize",
        callback.as_ref().unchecked_ref(),
    )?;
    callback.forget();
    Ok(())
}

fn resize_canvas(window: &Window, document: &Document) -> Result<(), JsValue> {
    let width = window
        .inner_width()
        .ok()
        .and_then(|value| value.as_f64())
        .unwrap_or(1280.0);
    let height = window
        .inner_height()
        .ok()
        .and_then(|value| value.as_f64())
        .unwrap_or(720.0);

    let canvas: HtmlCanvasElement = by_id(document, "grid")?.dyn_into()?;
    canvas.set_width(width.max(1.0) as u32);
    canvas.set_height(height.max(1.0) as u32);

    STATE.with(|state| {
        let mut state = state.borrow_mut();
        state.viewport_w = width;
        state.viewport_h = height;
    });
    Ok(())
}

fn draw_grid(window: &Window, document: &Document) -> Result<(), JsValue> {
    let canvas: HtmlCanvasElement = by_id(document, "grid")?.dyn_into()?;
    let context = canvas
        .get_context("2d")?
        .ok_or_else(|| JsValue::from_str("contexto 2d indisponível"))?
        .dyn_into::<CanvasRenderingContext2d>()?;

    let (width, height, scale, offset_x, offset_y, theme) = STATE.with(|state| {
        let state = state.borrow();
        (
            state.viewport_w,
            state.viewport_h,
            state.scale,
            state.offset_x,
            state.offset_y,
            state.theme,
        )
    });

    if theme == CanvasTheme::Minimalist {
        minimalist_theme::draw_graph_paper(
            &context, width, height, offset_x, offset_y, scale,
        )?;
        let _ = window;
        return Ok(());
    }

    if theme == CanvasTheme::MinimalistDark {
        minimalist_dark_theme::draw_graph_paper(
            &context, width, height, offset_x, offset_y, scale,
        )?;
        let _ = window;
        return Ok(());
    }

    context.clear_rect(0.0, 0.0, width, height);

    let step = adaptive_step(40.0 * scale);
    let (minor, major) = grid_colors(theme);

    context.set_line_width(1.0);
    context.set_stroke_style_str(minor);
    context.begin_path();
    add_grid_lines(&context, width, height, offset_x, offset_y, step);
    context.stroke();

    context.set_stroke_style_str(major);
    context.begin_path();
    add_grid_lines(&context, width, height, offset_x, offset_y, step * 5.0);
    context.stroke();

    let _ = window;
    Ok(())
}

fn adaptive_step(mut step: f64) -> f64 {
    if !step.is_finite() || step <= 0.0 {
        return 40.0;
    }
    while step < 18.0 {
        step *= 2.0;
    }
    while step > 180.0 {
        step /= 2.0;
    }
    step
}

fn grid_colors(theme: CanvasTheme) -> (&'static str, &'static str) {
    match theme {
        CanvasTheme::OxidizedIron => ("rgba(198,140,92,.10)", "rgba(198,140,92,.22)"),
        CanvasTheme::Nord => ("rgba(136,192,208,.10)", "rgba(136,192,208,.22)"),
        _ => ("rgba(130,170,235,.10)", "rgba(130,170,235,.22)"),
    }
}

fn add_grid_lines(
    context: &CanvasRenderingContext2d,
    width: f64,
    height: f64,
    offset_x: f64,
    offset_y: f64,
    step: f64,
) {
    if !step.is_finite() || step <= 0.0 {
        return;
    }

    let mut x = offset_x.rem_euclid(step);
    while x < width {
        context.move_to(x, 0.0);
        context.line_to(x, height);
        x += step;
    }

    let mut y = offset_y.rem_euclid(step);
    while y < height {
        context.move_to(0.0, y);
        context.line_to(width, y);
        y += step;
    }
}

// ---------------------------------------------------------------------------
// Pan / zoom / cartões
// ---------------------------------------------------------------------------

fn setup_pan_zoom(window: &Window, document: &Document) -> Result<(), JsValue> {
    let viewport = by_id(document, "viewport")?;

    // Roda do mouse: pan por padrão, zoom com Ctrl/Meta.
    let window_for_wheel = window.clone();
    let document_for_wheel = document.clone();
    let wheel = Closure::<dyn FnMut(WheelEvent)>::new(move |event: WheelEvent| {
        event.prevent_default();
        let cursor_x = event.client_x() as f64;
        let cursor_y = event.client_y() as f64;

        STATE.with(|state| {
            let mut state = state.borrow_mut();
            if event.ctrl_key() || event.meta_key() {
                // O delta chega em pixels, linhas ou páginas dependendo do
                // dispositivo. Normaliza para um fator perceptível (~12% por
                // entalhe) em vez do passo quase invisível anterior.
                let delta = event.delta_y();
                let exponent = match event.delta_mode() {
                    WheelEvent::DOM_DELTA_LINE => delta * 0.12,
                    WheelEvent::DOM_DELTA_PAGE => delta * 0.8,
                    _ => delta * 0.0025,
                };
                let factor = (-exponent).exp().clamp(0.5, 2.0);
                let new_scale = (state.scale * factor).clamp(0.15, 6.0);
                let world_x = (cursor_x - state.offset_x) / state.scale;
                let world_y = (cursor_y - state.offset_y) / state.scale;
                state.scale = new_scale;
                state.offset_x = cursor_x - world_x * new_scale;
                state.offset_y = cursor_y - world_y * new_scale;
            } else {
                state.offset_x -= event.delta_x();
                state.offset_y -= event.delta_y();
            }
        });

        let _ = apply_transform(&document_for_wheel);
        let _ = draw_grid(&window_for_wheel, &document_for_wheel);
        send_transform();
    });
    let wheel_options = web_sys::AddEventListenerOptions::new();
    wheel_options.set_passive(false);
    viewport.add_event_listener_with_callback_and_add_event_listener_options(
        "wheel",
        wheel.as_ref().unchecked_ref(),
        &wheel_options,
    )?;
    wheel.forget();

    // Botão do meio: pan temporário, como a ferramenta mão, sem trocar a
    // ferramenta atual. O listener é de captura para chegar antes da camada
    // de desenho, que também escuta `pointerdown`.
    let viewport_for_middle = viewport.clone();
    let document_for_middle = document.clone();
    let middle_down = Closure::<dyn FnMut(PointerEvent)>::new(move |event: PointerEvent| {
        if event.button() != 1 {
            return;
        }
        // Sem o prevent_default o WebKit tenta colar a seleção primária e
        // inicia o autoscroll do botão do meio.
        event.prevent_default();
        event.stop_propagation();
        STATE.with(|state| {
            let mut state = state.borrow_mut();
            state.dragging = true;
            state.middle_pan = true;
            state.last_x = event.client_x() as f64;
            state.last_y = event.client_y() as f64;
        });
        if let Ok(root) = document_element(&document_for_middle) {
            let _ = root.class_list().add_1("panning");
        }
        let _ = viewport_for_middle.set_pointer_capture(event.pointer_id());
    });
    let capture = web_sys::AddEventListenerOptions::new();
    capture.set_capture(true);
    viewport.add_event_listener_with_callback_and_add_event_listener_options(
        "pointerdown",
        middle_down.as_ref().unchecked_ref(),
        &capture,
    )?;
    middle_down.forget();

    // `auxclick` é o clique "extra" do botão do meio; bloquear evita o
    // comportamento nativo do navegador.
    let auxclick = Closure::<dyn FnMut(PointerEvent)>::new(|event: PointerEvent| {
        if event.button() == 1 {
            event.prevent_default();
        }
    });
    viewport.add_event_listener_with_callback("auxclick", auxclick.as_ref().unchecked_ref())?;
    auxclick.forget();

    // Arrasto com o botão esquerdo.
    let viewport_for_down = viewport.clone();
    let pointer_down = Closure::<dyn FnMut(PointerEvent)>::new(move |event: PointerEvent| {
        if event.button() != 0 {
            return;
        }
        STATE.with(|state| {
            let mut state = state.borrow_mut();
            state.dragging = true;
            state.last_x = event.client_x() as f64;
            state.last_y = event.client_y() as f64;
        });
        let _ = viewport_for_down.set_pointer_capture(event.pointer_id());
    });
    viewport.add_event_listener_with_callback("pointerdown", pointer_down.as_ref().unchecked_ref())?;
    pointer_down.forget();

    let window_for_move = window.clone();
    let document_for_move = document.clone();
    let pointer_move = Closure::<dyn FnMut(PointerEvent)>::new(move |event: PointerEvent| {
        let moved = STATE.with(|state| {
            let mut state = state.borrow_mut();
            if !state.dragging {
                return false;
            }
            let dx = event.client_x() as f64 - state.last_x;
            let dy = event.client_y() as f64 - state.last_y;
            state.last_x = event.client_x() as f64;
            state.last_y = event.client_y() as f64;
            state.offset_x += dx;
            state.offset_y += dy;
            true
        });

        if moved {
            let _ = apply_transform(&document_for_move);
            let _ = draw_grid(&window_for_move, &document_for_move);
            send_transform();
        }
    });
    viewport.add_event_listener_with_callback("pointermove", pointer_move.as_ref().unchecked_ref())?;
    pointer_move.forget();

    let viewport_for_up = viewport.clone();
    let document_for_up = document.clone();
    let pointer_up = Closure::<dyn FnMut(PointerEvent)>::new(move |event: PointerEvent| {
        let (was_middle, still_dragging) = STATE.with(|state| {
            let mut state = state.borrow_mut();
            let was_middle = state.middle_pan;
            // Soltar outro botão no meio de um pan do meio não encerra o
            // gesto; só o botão do meio (ou um cancelamento) encerra.
            if was_middle && event.button() != 1 {
                return (was_middle, true);
            }
            state.dragging = false;
            state.middle_pan = false;
            (was_middle, false)
        });
        if !still_dragging {
            let _ = viewport_for_up.release_pointer_capture(event.pointer_id());
            if was_middle {
                if let Ok(root) = document_element(&document_for_up) {
                    let _ = root.class_list().remove_1("panning");
                }
            }
        }
    });
    viewport.add_event_listener_with_callback("pointerup", pointer_up.as_ref().unchecked_ref())?;
    pointer_up.forget();

    let viewport_for_cancel = viewport.clone();
    let document_for_cancel = document.clone();
    let pointer_cancel = Closure::<dyn FnMut(PointerEvent)>::new(move |event: PointerEvent| {
        STATE.with(|state| {
            let mut state = state.borrow_mut();
            state.dragging = false;
            state.middle_pan = false;
        });
        let _ = viewport_for_cancel.release_pointer_capture(event.pointer_id());
        if let Ok(root) = document_element(&document_for_cancel) {
            let _ = root.class_list().remove_1("panning");
        }
    });
    viewport.add_event_listener_with_callback(
        "pointercancel",
        pointer_cancel.as_ref().unchecked_ref(),
    )?;
    pointer_cancel.forget();

    // O botão direito é do desktop, não do navegador: nada de menu de
    // contexto (voltar/recarregar/inspecionar). O shell já suprime o menu
    // nativo do WebKit; este listener cobre também a versão web do frontend.
    // Sem ele, o popup nativo abre um grab de ponteiro e o desktop aninhado
    // fica sem resposta a cliques e teclas.
    let context_menu = Closure::<dyn FnMut(Event)>::new(|event: Event| {
        event.prevent_default();
    });
    document.add_event_listener_with_callback(
        "contextmenu",
        context_menu.as_ref().unchecked_ref(),
    )?;
    context_menu.forget();

    Ok(())
}

// ---------------------------------------------------------------------------
// Controles de zoom do HUD
// ---------------------------------------------------------------------------

fn zoom_at(window: &Window, document: &Document, factor: f64, cx: f64, cy: f64) {
    STATE.with(|state| {
        let mut state = state.borrow_mut();
        let new_scale = (state.scale * factor).clamp(0.15, 6.0);
        let world_x = (cx - state.offset_x) / state.scale;
        let world_y = (cy - state.offset_y) / state.scale;
        state.scale = new_scale;
        state.offset_x = cx - world_x * new_scale;
        state.offset_y = cy - world_y * new_scale;
    });
    let _ = apply_transform(document);
    let _ = draw_grid(window, document);
    send_transform();
}

fn zoom_by(window: &Window, document: &Document, factor: f64) {
    let (cx, cy) = STATE.with(|state| {
        let state = state.borrow();
        (state.viewport_w / 2.0, state.viewport_h / 2.0)
    });
    zoom_at(window, document, factor, cx, cy);
}

fn reset_zoom(window: &Window, document: &Document) {
    STATE.with(|state| {
        let mut state = state.borrow_mut();
        state.scale = 1.0;
        state.offset_x = 0.0;
        state.offset_y = 0.0;
    });
    let _ = apply_transform(document);
    let _ = draw_grid(window, document);
    send_transform();
}

fn fit_zoom(window: &Window, document: &Document) {
    let bounds = STATE.with(|state| {
        let state = state.borrow();
        let mut min_x = f64::MAX;
        let mut min_y = f64::MAX;
        let mut max_x = f64::MIN;
        let mut max_y = f64::MIN;
        for card in state.cards.iter().copied() {
            min_x = min_x.min(card.x);
            min_y = min_y.min(card.y);
            max_x = max_x.max(card.x + card.w);
            max_y = max_y.max(card.y + card.h);
        }
        (min_x, min_y, max_x, max_y, state.viewport_w, state.viewport_h)
    });
    let (min_x, min_y, max_x, max_y, viewport_w, viewport_h) = bounds;
    if !min_x.is_finite() || !min_y.is_finite() || max_x <= min_x || max_y <= min_y {
        reset_zoom(window, document);
        return;
    }

    let margin = 90.0;
    let world_w = (max_x - min_x).max(1.0);
    let world_h = (max_y - min_y).max(1.0);
    let scale = ((viewport_w - 2.0 * margin) / world_w)
        .min((viewport_h - 2.0 * margin) / world_h)
        .clamp(0.15, 6.0);
    let center_x = (min_x + max_x) / 2.0;
    let center_y = (min_y + max_y) / 2.0;

    STATE.with(|state| {
        let mut state = state.borrow_mut();
        state.scale = scale;
        state.offset_x = viewport_w / 2.0 - center_x * scale;
        state.offset_y = viewport_h / 2.0 - center_y * scale;
    });
    let _ = apply_transform(document);
    let _ = draw_grid(window, document);
    send_transform();
}

fn setup_zoom_controls(window: &Window, document: &Document) -> Result<(), JsValue> {
    for (id, factor) in [("zoom-in", 1.2), ("zoom-out", 1.0 / 1.2)] {
        let Ok(button) = by_id(document, id) else {
            continue;
        };
        let win = window.clone();
        let doc = document.clone();
        let callback = Closure::<dyn FnMut()>::new(move || {
            zoom_by(&win, &doc, factor);
        });
        button.add_event_listener_with_callback("click", callback.as_ref().unchecked_ref())?;
        callback.forget();
    }

    if let Ok(button) = by_id(document, "zoom-reset") {
        let win = window.clone();
        let doc = document.clone();
        let callback = Closure::<dyn FnMut()>::new(move || {
            reset_zoom(&win, &doc);
        });
        button.add_event_listener_with_callback("click", callback.as_ref().unchecked_ref())?;
        callback.forget();
    }

    if let Ok(button) = by_id(document, "zoom-fit") {
        let win = window.clone();
        let doc = document.clone();
        let callback = Closure::<dyn FnMut()>::new(move || {
            fit_zoom(&win, &doc);
        });
        button.add_event_listener_with_callback("click", callback.as_ref().unchecked_ref())?;
        callback.forget();
    }

    // Atalhos de zoom dentro do WebView (não conflitam com o WM aninhado).
    let win = window.clone();
    let doc = document.clone();
    let on_key = Closure::<dyn FnMut(KeyboardEvent)>::new(move |event: KeyboardEvent| {
        if !(event.ctrl_key() || event.meta_key()) {
            return;
        }
        // Ctrl+1..9 vai para as primeiras regiões do canvas.
        let editing = doc
            .active_element()
            .map(|element| matches!(element.tag_name().as_str(), "INPUT" | "TEXTAREA"))
            .unwrap_or(false);
        if !editing && !event.shift_key() && !event.alt_key() {
            if let Ok(index) = event.key().parse::<usize>() {
                if (1..=9).contains(&index) {
                    goto_region_by_index(index - 1);
                    event.prevent_default();
                    return;
                }
            }
        }
        if event.shift_key() && event.key().eq_ignore_ascii_case("f") {
            fit_zoom(&win, &doc);
            event.prevent_default();
            return;
        }
        match event.key().as_str() {
            "+" | "=" => {
                zoom_by(&win, &doc, 1.2);
                event.prevent_default();
            }
            "-" | "_" => {
                zoom_by(&win, &doc, 1.0 / 1.2);
                event.prevent_default();
            }
            "0" => {
                reset_zoom(&win, &doc);
                event.prevent_default();
            }
            _ => {}
        }
    });
    document.add_event_listener_with_callback("keydown", on_key.as_ref().unchecked_ref())?;
    on_key.forget();

    Ok(())
}

fn setup_overview_control(document: &Document) -> Result<(), JsValue> {
    let Ok(button) = by_id(document, "overview-btn") else {
        return Ok(());
    };
    let callback = Closure::<dyn FnMut()>::new(move || {
        send_message(&IpcMessage::ToggleOverview);
    });
    button.add_event_listener_with_callback("click", callback.as_ref().unchecked_ref())?;
    callback.forget();
    Ok(())
}

fn setup_resize_tool(document: &Document) -> Result<(), JsValue> {
    let Ok(button) = by_id(document, "tool-resize") else {
        return Ok(());
    };

    let doc = document.clone();
    let button_for_click = button.clone();
    let toggle = Closure::<dyn FnMut()>::new(move || {
        let active = STATE.with(|state| {
            let mut state = state.borrow_mut();
            state.resize_tool = !state.resize_tool;
            state.resize_tool
        });
        let _ = button_for_click.set_attribute(
            "aria-pressed",
            if active { "true" } else { "false" },
        );
        send_message(&IpcMessage::WindowResizeTool { active });
        if active {
            show_toast(
                &doc,
                "Redimensionar: arraste uma janela para ajustar pelo canto mais próximo",
            );
        }
    });
    button.add_event_listener_with_callback("click", toggle.as_ref().unchecked_ref())?;
    toggle.forget();

    // Qualquer ferramenta de desenho desliga o modo de redimensionamento.
    let button_for_tools = button.clone();
    let deactivate = Closure::<dyn FnMut(Event)>::new(move |_event: Event| {
        let was_active = STATE.with(|state| {
            let mut state = state.borrow_mut();
            let was_active = state.resize_tool;
            state.resize_tool = false;
            was_active
        });
        if was_active {
            let _ = button_for_tools.set_attribute("aria-pressed", "false");
            send_message(&IpcMessage::WindowResizeTool { active: false });
        }
    });
    if let Ok(tools) = document.query_selector_all(".tool:not(#tool-resize)") {
        for index in 0..tools.length() {
            if let Some(tool) = tools.get(index) {
                let _ = tool.add_event_listener_with_callback(
                    "click",
                    deactivate.as_ref().unchecked_ref(),
                );
            }
        }
    }
    deactivate.forget();
    Ok(())
}

fn setup_cards(document: &Document) -> Result<(), JsValue> {
    for card in CARDS {
        if document
            .get_element_by_id(&format!("card-{}", card.id))
            .is_some()
        {
            bind_card(document, card.id)?;
        }
    }
    Ok(())
}

/// Liga os gestos de um cartão: foco, arrasto e botão de fechar.
///
/// Os cartões de demonstração já existem no HTML; os cartões de janelas reais
/// novas são criados em `ensure_card` e passam por aqui também.
fn bind_card(document: &Document, card_id: u32) -> Result<(), JsValue> {
    let Some(element) = document.get_element_by_id(&format!("card-{card_id}")) else {
        return Ok(());
    };

    // Clique: ativa o cartão (destaque de foco).
    let document_for_click = document.clone();
    let click = Closure::<dyn FnMut()>::new(move || {
        STATE.with(|state| state.borrow_mut().active = Some(card_id));
        let _ = apply_transform(&document_for_click);
    });
    element.add_event_listener_with_callback("click", click.as_ref().unchecked_ref())?;
    click.forget();

    // Arrasto: move o cartão no espaço de mundo. O `stop_propagation`
    // evita que o mesmo gesto vire pan do canvas; a posição é enviada
    // ao compositor para mover a janela real correspondente.
    let element_for_down = element.clone();
    let document_for_down = document.clone();
    let pointer_down = Closure::<dyn FnMut(PointerEvent)>::new(move |event: PointerEvent| {
        if event.button() != 0 {
            return;
        }
        event.stop_propagation();

        // Na grade da visão geral o cartão não é arrastável: o clique só
        // o destaca e quem fecha o modo é o compositor, ao ver o clique
        // fora das miniaturas.
        let card_overview = STATE.with(|state| state.borrow().card_overview);
        if card_overview {
            STATE.with(|state| state.borrow_mut().active = Some(card_id));
            return;
        }

        STATE.with(|state| {
            let mut state = state.borrow_mut();
            state.drag_card = Some(card_id);
            state.active = Some(card_id);
            state.last_x = event.client_x() as f64;
            state.last_y = event.client_y() as f64;
        });
        let _ = element_for_down.set_pointer_capture(event.pointer_id());
        let _ = element_for_down.class_list().add_1("dragging");
        let _ = apply_transform(&document_for_down);
    });
    element.add_event_listener_with_callback("pointerdown", pointer_down.as_ref().unchecked_ref())?;
    pointer_down.forget();

    let document_for_move = document.clone();
    let pointer_move = Closure::<dyn FnMut(PointerEvent)>::new(move |event: PointerEvent| {
        let position = STATE.with(|state| {
            let mut state = state.borrow_mut();
            if state.drag_card != Some(card_id) {
                return None;
            }
            let scale = state.scale.max(0.01);
            let dx = (event.client_x() as f64 - state.last_x) / scale;
            let dy = (event.client_y() as f64 - state.last_y) / scale;
            state.last_x = event.client_x() as f64;
            state.last_y = event.client_y() as f64;
            let entry = state.cards.iter_mut().find(|entry| entry.id == card_id)?;
            entry.x += dx;
            entry.y += dy;
            Some((entry.x, entry.y))
        });

        if let Some((x, y)) = position {
            set_card_position(&document_for_move, card_id, x, y);
            let _ = update_glow(&document_for_move);
            send_cluster();
        }
    });
    element.add_event_listener_with_callback("pointermove", pointer_move.as_ref().unchecked_ref())?;
    pointer_move.forget();

    let element_for_up = element.clone();
    let pointer_up = Closure::<dyn FnMut(PointerEvent)>::new(move |event: PointerEvent| {
        let was_dragging = STATE.with(|state| {
            let mut state = state.borrow_mut();
            if state.drag_card == Some(card_id) {
                state.drag_card = None;
                true
            } else {
                false
            }
        });
        let _ = element_for_up.release_pointer_capture(event.pointer_id());
        if was_dragging {
            let _ = element_for_up.class_list().remove_1("dragging");
            send_cluster();
        }
    });
    element.add_event_listener_with_callback("pointerup", pointer_up.as_ref().unchecked_ref())?;
    pointer_up.forget();

    let element_for_cancel = element.clone();
    let pointer_cancel = Closure::<dyn FnMut(PointerEvent)>::new(move |event: PointerEvent| {
        STATE.with(|state| state.borrow_mut().drag_card = None);
        let _ = element_for_cancel.release_pointer_capture(event.pointer_id());
        let _ = element_for_cancel.class_list().remove_1("dragging");
        send_cluster();
    });
    element.add_event_listener_with_callback(
        "pointercancel",
        pointer_cancel.as_ref().unchecked_ref(),
    )?;
    pointer_cancel.forget();

    // Botão de fechar: encerra a janela real correspondente (e o cartão
    // some quando o compositor confirmar com `WindowClosed`).
    if let Ok(Some(close)) = element.query_selector(".window-card__close") {
        let close_button = close.clone();
        let pointer = Closure::<dyn FnMut(PointerEvent)>::new(move |event: PointerEvent| {
            event.stop_propagation();
            event.prevent_default();
            let _ = close_button.class_list().add_1("is-closing");
            send_close_window(card_id);
        });
        close.add_event_listener_with_callback("pointerdown", pointer.as_ref().unchecked_ref())?;
        pointer.forget();
    }

    Ok(())
}

/// Garante que existe um cartão no DOM para a janela e mantém o título em dia.
fn ensure_card(document: &Document, id: u32, title: &str) {
    if document.get_element_by_id(&format!("card-{id}")).is_none() {
        let Ok(world) = by_id(document, "world") else {
            return;
        };
        let Ok(element) = document.create_element("article") else {
            return;
        };
        let _ = element.set_id(&format!("card-{id}"));
        let _ = element.set_class_name("window-card");
        let _ = element.set_attribute("data-window-id", &id.to_string());
        element.set_inner_html(&format!(
            "<div class=\"window-card__bar\"><span class=\"dots\"><i></i><i></i><i></i></span>\
             <b>{}</b><span class=\"window-card__id\">{id}</span>\
             <button class=\"window-card__close\" type=\"button\" aria-label=\"Fechar janela\">×</button>\
             </div><div class=\"window-card__body\"></div>",
            escape_html(title)
        ));
        let _ = world.append_child(&element);
        let _ = bind_card(document, id);
    }

    if title.trim().is_empty() {
        return;
    }
    let Some(element) = document.get_element_by_id(&format!("card-{id}")) else {
        return;
    };
    if let Ok(Some(label)) = element.query_selector(".window-card__bar b") {
        label.set_text_content(Some(title));
    }
}

/// Aplica posição e tamanho guardados no estado. Com uma janela real o
/// cartão fica exatamente sob a superfície do cliente; ele serve de
/// placeholder para a visão geral e para o arrasto do cartão.
fn apply_card_geometry(document: &Document, card: Card) {
    let Some(element) = document.get_element_by_id(&format!("card-{}", card.id)) else {
        return;
    };
    let Ok(element) = element.dyn_into::<web_sys::HtmlElement>() else {
        return;
    };
    let style = element.style();
    let _ = style.set_property("display", "");
    if card.has_window {
        // Com janela real o cartão é a barra de título dela: fica logo acima
        // do conteúdo (x, y), e o corpo fica exatamente sob a superfície. A
        // borda de 1px do cartão é descontada para o conteúdo casar.
        let _ = style.set_property("left", &format!("{}px", card.x - 1.0));
        let _ = style.set_property("top", &format!("{}px", card.y - CARD_BAR));
        let _ = style.set_property("width", &format!("{}px", card.w + 2.0));
        let _ = style.set_property("height", &format!("{}px", card.h + CARD_BAR + 1.0));
    } else {
        let _ = style.set_property("left", &format!("{}px", card.x));
        let _ = style.set_property("top", &format!("{}px", card.y));
        let _ = style.set_property("width", &format!("{}px", card.w));
        let _ = style.set_property("height", &format!("{}px", card.h));
    }
}

fn set_card_position(document: &Document, id: u32, x: f64, y: f64) {
    let card = STATE.with(|state| {
        let mut state = state.borrow_mut();
        let entry = state.cards.iter_mut().find(|entry| entry.id == id)?;
        entry.x = x;
        entry.y = y;
        Some(*entry)
    });
    if let Some(card) = card {
        apply_card_geometry(document, card);
    }
}

/// Aplica o estado da visão geral ao shell.
///
/// Com janelas Wayland reais o compositor desenha as miniaturas e os cartões
/// de demonstração somem. Sem nenhuma janela real, os próprios cartões formam
/// a grade — assim o botão **Visão geral** sempre responde, mesmo numa área de
/// trabalho vazia.
fn set_overview(document: &Document, active: bool, windows: usize) -> Result<(), JsValue> {
    let card_grid = active && windows == 0;
    STATE.with(|state| state.borrow_mut().card_overview = card_grid);

    if let Ok(root) = document_element(document) {
        if card_grid {
            let _ = root.set_attribute("data-overview", "cards");
        } else if active {
            let _ = root.set_attribute("data-overview", "true");
        } else {
            let _ = root.remove_attribute("data-overview");
        }
    }
    if let Ok(button) = by_id(document, "overview-btn") {
        let _ = button.set_attribute("aria-pressed", if active { "true" } else { "false" });
    }

    apply_card_overview(document, card_grid)
}

/// Reposiciona os cartões numa grade de tela (visão geral sem janelas reais)
/// ou devolve cada um à posição de mundo guardada em `State::cards`.
///
/// Durante a grade o `#world` fica sem transformação (ver CSS), então as
/// coordenadas são as mesmas da tela.
fn apply_card_overview(document: &Document, active: bool) -> Result<(), JsValue> {
    let (viewport_w, viewport_h, cards) = STATE.with(|state| {
        let state = state.borrow();
        (state.viewport_w, state.viewport_h, state.cards.clone())
    });

    let cells = if active {
        overview_cells_screen(viewport_w, viewport_h, cards.len())
    } else {
        Vec::new()
    };

    for (index, card) in cards.iter().enumerate() {
        let Some(element) = document.get_element_by_id(&format!("card-{}", card.id)) else {
            continue;
        };
        let Ok(element) = element.dyn_into::<web_sys::HtmlElement>() else {
            continue;
        };
        let style = element.style();

        if active {
            let Some(&(cell_x, cell_y, cell_w, cell_h)) = cells.get(index) else {
                continue;
            };
            // Mantém a proporção do cartão e centraliza na célula.
            let fit = (cell_w / card.w.max(1.0)).min(cell_h / card.h.max(1.0));
            let thumb_w = (card.w * fit).max(1.0);
            let thumb_h = (card.h * fit).max(1.0);
            let _ = style.set_property("left", &format!("{}px", cell_x + (cell_w - thumb_w) / 2.0));
            let _ = style.set_property("top", &format!("{}px", cell_y + (cell_h - thumb_h) / 2.0));
            let _ = style.set_property("width", &format!("{thumb_w}px"));
            let _ = style.set_property("height", &format!("{thumb_h}px"));
            let _ = element.class_list().add_1("overview-card");
        } else {
            apply_card_geometry(document, *card);
            let _ = element.class_list().remove_1("overview-card");
        }
    }

    Ok(())
}

/// Grade de células em coordenadas de tela. Repete o cálculo de
/// `overview_cells` do compositor para que cartões e janelas reais ocupem a
/// mesma área útil.
fn overview_cells_screen(viewport_w: f64, viewport_h: f64, count: usize) -> Vec<(f64, f64, f64, f64)> {
    if count == 0 {
        return Vec::new();
    }

    let margin = common::OVERVIEW_MARGIN_X as f64;
    let gap = common::OVERVIEW_GAP as f64;
    let top = common::OVERVIEW_TOP as f64;
    let area_x = margin;
    let area_y = top;
    let area_w = (viewport_w - margin * 2.0).max(1.0);
    let area_h = (viewport_h - top - common::OVERVIEW_BOTTOM as f64).max(1.0);

    let count_f = count as f64;
    let columns = ((count_f * area_w / area_h).sqrt().ceil() as usize).clamp(1, count);
    let rows = (count + columns - 1) / columns;
    let cell_w = ((area_w - gap * (columns as f64 - 1.0)) / columns as f64).max(1.0);
    let cell_h = ((area_h - gap * (rows as f64 - 1.0)) / rows as f64).max(1.0);

    (0..count)
        .map(|index| {
            let column = (index % columns) as f64;
            let row = (index / columns) as f64;
            (
                area_x + column * (cell_w + gap),
                area_y + row * (cell_h + gap),
                cell_w,
                cell_h,
            )
        })
        .collect()
}

fn apply_transform(document: &Document) -> Result<(), JsValue> {
    let (scale, offset_x, offset_y, viewport_w, viewport_h) = STATE.with(|state| {
        let state = state.borrow();
        (
            state.scale,
            state.offset_x,
            state.offset_y,
            state.viewport_w,
            state.viewport_h,
        )
    });

    let world = by_id(document, "world")?;
    world.set_attribute(
        "style",
        &format!(
            "transform: translate({offset_x}px, {offset_y}px) scale({scale}); transform-origin: 0 0;"
        ),
    )?;

    drawing::set_view(scale, offset_x, offset_y, viewport_w, viewport_h);
    update_zoom_label(document, scale);
    update_glow(document)?;
    update_region_active(document);
    maybe_auto_maximize();
    Ok(())
}

fn update_zoom_label(document: &Document, scale: f64) {
    if let Ok(element) = by_id(document, "zoom") {
        let _ = element.set_inner_html(&format!("{:.0}%", scale * 100.0));
    }
}

fn update_glow(document: &Document) -> Result<(), JsValue> {
    let glow = by_id(document, "active-window-glow")?;
    let active = STATE.with(|state| state.borrow().active);

    let Some(id) = active else {
        glow.set_attribute("style", "opacity:0;")?;
        let _ = glow.class_list().remove_1("on");
        return Ok(());
    };

    // Janela real: o brilho envolve a moldura (barra + conteúdo), usando a
    // geometria guardada no estado (mesma transformação do canvas).
    let real_window = STATE.with(|state| {
        let state = state.borrow();
        state.cards.iter().find(|card| card.id == id).and_then(|card| {
            card.has_window.then(|| {
                (
                    (card.x - 1.0) * state.scale + state.offset_x,
                    (card.y - CARD_BAR) * state.scale + state.offset_y,
                    (card.w + 2.0) * state.scale,
                    (card.h + CARD_BAR + 1.0) * state.scale,
                )
            })
        })
    });
    if let Some((left, top, width, height)) = real_window {
        glow.set_attribute(
            "style",
            &format!("left:{left}px; top:{top}px; width:{width}px; height:{height}px; opacity:1;"),
        )?;
        let _ = glow.class_list().add_1("on");
        return Ok(());
    }

    let Some(card) = document.get_element_by_id(&format!("card-{id}")) else {
        return Ok(());
    };
    let rect: DomRect = card.get_bounding_client_rect();
    glow.set_attribute(
        "style",
        &format!(
            "left:{}px; top:{}px; width:{}px; height:{}px; opacity:1;",
            rect.left(),
            rect.top(),
            rect.width(),
            rect.height()
        ),
    )?;
    let _ = glow.class_list().add_1("on");
    Ok(())
}

fn maybe_auto_maximize() {
    let mut changes = Vec::new();

    STATE.with(|state| {
        let mut state = state.borrow_mut();
        let viewport_w = state.viewport_w.max(1.0);
        let viewport_h = state.viewport_h.max(1.0);
        let scale = state.scale;
        let cards = state.cards.clone();

        for card in cards {
            let visible_w = card.w * scale;
            let visible_h = card.h * scale;
            let should_maximize = visible_w >= viewport_w * 0.9 || visible_h >= viewport_h * 0.9;

            if should_maximize && state.maximized.insert(card.id) {
                changes.push((card.id, true));
            } else if !should_maximize && state.maximized.remove(&card.id) {
                changes.push((card.id, false));
            }
        }
    });

    for (window_id, state) in changes {
        send_message(&IpcMessage::AutoMaximize { window_id, state });
    }
}

// ---------------------------------------------------------------------------
// IPC WebSocket
// ---------------------------------------------------------------------------

fn connect_websocket() {
    let Ok(socket) = WebSocket::new(WS_URL) else {
        set_status("WebSocket indisponível");
        return;
    };

    let on_open = Closure::<dyn FnMut()>::new(|| {
        set_status("conectado");
        send_transform();
        send_cluster();
        send_theme();
        send_message(&IpcMessage::NotificationHistoryRequest);
        send_message(&IpcMessage::RegionListRequest);
        send_message(&IpcMessage::TrayRequest);
        send_message(&IpcMessage::MediaRequest);
        if let Some(document) = web_sys::window().and_then(|window| window.document()) {
            send_chrome_rects(&document);
        }
    });
    socket.set_onopen(Some(on_open.as_ref().unchecked_ref()));
    on_open.forget();

    let on_message = Closure::<dyn FnMut(MessageEvent)>::new(|event: MessageEvent| {
        if let Some(text) = event.data().as_string() {
            handle_server_message(&text);
        }
    });
    socket.set_onmessage(Some(on_message.as_ref().unchecked_ref()));
    on_message.forget();

    let on_close = Closure::<dyn FnMut(CloseEvent)>::new(|_event: CloseEvent| {
        set_status("desconectado — tentando reconectar…");
        schedule_reconnect();
    });
    socket.set_onclose(Some(on_close.as_ref().unchecked_ref()));
    on_close.forget();

    let on_error = Closure::<dyn FnMut(Event)>::new(|_event: Event| {
        set_status("erro de conexão");
    });
    socket.set_onerror(Some(on_error.as_ref().unchecked_ref()));
    on_error.forget();

    SOCKET.with(|slot| {
        *slot.borrow_mut() = Some(socket);
    });
}

fn schedule_reconnect() {
    let Some(window) = web_sys::window() else {
        return;
    };
    let callback = Closure::once(connect_websocket);
    let _ = window.set_timeout_with_callback_and_timeout_and_arguments_0(
        callback.as_ref().unchecked_ref(),
        1500,
    );
    callback.forget();
}

fn send_message(message: &IpcMessage) {
    let Ok(json) = message.to_json() else {
        return;
    };

    SOCKET.with(|slot| {
        let socket = slot.borrow();
        let Some(socket) = socket.as_ref() else {
            return;
        };
        if socket.ready_state() == WebSocket::OPEN {
            let _ = socket.send_with_str(&json);
        }
    });
}

fn send_transform() {
    let (scale, offset_x, offset_y) = STATE.with(|state| {
        let state = state.borrow();
        (state.scale, state.offset_x, state.offset_y)
    });
    send_message(&IpcMessage::CanvasTransform {
        scale,
        offset_x,
        offset_y,
    });
}

fn send_theme() {
    let theme = STATE.with(|state| state.borrow().theme);
    send_message(&IpcMessage::SetTheme { theme });
}

fn send_close_window(window_id: u32) {
    send_message(&IpcMessage::CloseWindow { window_id });
}

fn remove_card(window_id: u32) {
    STATE.with(|state| {
        state
            .borrow_mut()
            .cards
            .retain(|card| card.id != window_id);
    });
    if let Some(window) = web_sys::window() {
        if let Some(document) = window.document() {
            if let Some(element) = document.get_element_by_id(&format!("card-{window_id}")) {
                element.remove();
            }
            let _ = update_glow(&document);
        }
    }
}

/// Envia ao compositor as áreas do chrome (HUD, deck e gaveta aberta) para que
/// elas fiquem acima das janelas reais e recebam os cliques.
fn send_chrome_rects(document: &Document) {
    let mut rects = Vec::new();
    for id in [
        "hud",
        "deck-shell",
        "app-drawer",
        "calendar-popover",
        "theme-popover",
        "bar-settings-panel",
        "tray-menu",
    ] {
        let Some(element) = document.get_element_by_id(id) else {
            continue;
        };
        if element.has_attribute("hidden") {
            continue;
        }
        let rect: DomRect = element.get_bounding_client_rect();
        rects.push(ChromeRect {
            x: rect.left(),
            y: rect.top(),
            w: rect.width(),
            h: rect.height(),
        });
    }
    // Popups e painel de notificações são chrome: precisam ficar acima das
    // janelas reais e receber os cliques. Cada popup vira um retângulo
    // próprio para os vãos entre eles continuarem clicáveis no canvas.
    if let Ok(nodes) = document.query_selector_all(".notification-popup") {
        for index in 0..nodes.length() {
            let Some(node) = nodes.item(index) else {
                continue;
            };
            let Ok(element) = node.dyn_into::<Element>() else {
                continue;
            };
            let rect: DomRect = element.get_bounding_client_rect();
            rects.push(ChromeRect {
                x: rect.left(),
                y: rect.top(),
                w: rect.width(),
                h: rect.height(),
            });
        }
    }
    if let Some(panel) = document.get_element_by_id("notification-panel") {
        if !panel.has_attribute("hidden") {
            let rect: DomRect = panel.get_bounding_client_rect();
            rects.push(ChromeRect {
                x: rect.left(),
                y: rect.top(),
                w: rect.width(),
                h: rect.height(),
            });
        }
    }
    let hud_bottom = document
        .get_element_by_id("hud")
        .map(|hud| {
            let rect: DomRect = hud.get_bounding_client_rect();
            rect.bottom()
        })
        .unwrap_or(0.0);
    send_message(&IpcMessage::ChromeRects { rects, hud_bottom });
}

fn send_cluster() {
    let cards = STATE.with(|state| state.borrow().cards.clone());
    let placements = cards
        .iter()
        .map(|card| ClusterPlacement {
            id: card.id.to_string(),
            x: card.x,
            y: card.y,
            w: 0.0,
            h: 0.0,
            title: String::new(),
        })
        .collect();

    send_message(&IpcMessage::ClusterUpdate { placements });
}

fn handle_server_message(text: &str) {
    let Ok(message) = IpcMessage::from_json(text) else {
        return;
    };

    match message {
        IpcMessage::AppList { apps } => render_apps(&apps),
        IpcMessage::CanvasTransform {
            scale,
            offset_x,
            offset_y,
        } => {
            STATE.with(|state| {
                let mut state = state.borrow_mut();
                state.scale = scale;
                state.offset_x = offset_x;
                state.offset_y = offset_y;
            });
            if let Some(window) = web_sys::window() {
                if let Some(document) = window.document() {
                    let _ = apply_transform(&document);
                    let _ = draw_grid(&window, &document);
                }
            }
        }
        IpcMessage::SetTheme { theme } => {
            if let Some(window) = web_sys::window() {
                if let Some(document) = window.document() {
                    let _ = apply_theme_to_document(&document, theme);
                    let _ = draw_grid(&window, &document);
                }
            }
        }
        IpcMessage::ClusterUpdate { placements } => {
            // Movimento iniciado pelo compositor (arrasto da janela real ou
            // `move_request` do cliente): reflete a posição no cartão sem
            // devolver a mensagem, para não criar um laço de atualizações.
            apply_server_cluster(&placements);
        }
        IpcMessage::WindowClosed { window_id } => remove_card(window_id),
        IpcMessage::ZoomCanvas { action } => {
            if let Some(window) = web_sys::window() {
                if let Some(document) = window.document() {
                    match action {
                        CanvasZoom::In => zoom_by(&window, &document, 1.2),
                        CanvasZoom::Out => zoom_by(&window, &document, 1.0 / 1.2),
                        CanvasZoom::Reset => reset_zoom(&window, &document),
                        CanvasZoom::Fit => fit_zoom(&window, &document),
                    }
                }
            }
        }
        IpcMessage::OverviewState { active, windows } => {
            if let Some(window) = web_sys::window() {
                if let Some(document) = window.document() {
                    let _ = set_overview(&document, active, windows);
                }
            }
        }
        IpcMessage::Toast { message } => {
            if let Some(window) = web_sys::window() {
                if let Some(document) = window.document() {
                    show_toast(&document, &message);
                }
            }
        }
        IpcMessage::Notification {
            id,
            app_name,
            summary,
            body,
            icon,
            urgency,
            actions,
            timeout,
            ..
        } => {
            if let Some(document) = web_sys::window().and_then(|window| window.document()) {
                handle_notification(
                    &document,
                    NotificationItem {
                        id,
                        app_name,
                        summary,
                        body,
                        icon,
                        urgency,
                        actions,
                        timestamp_ms: js_sys::Date::now() as u64,
                    },
                    timeout,
                );
            }
        }
        IpcMessage::NotificationClosed { id, .. } => {
            remove_popup(id);
            if let Some(document) = web_sys::window().and_then(|window| window.document()) {
                let _ = render_notifications(&document);
            }
        }
        IpcMessage::NotificationHistory { items } => {
            // Mescla por id: uma resposta de histórico pode chegar depois de
            // notificações mais novas (corrida com o anúncio do daemon) e não
            // pode apagá-las. O mais recente (timestamp) fica no topo.
            STATE.with(|state| {
                let mut state = state.borrow_mut();
                for item in items {
                    if state.notifications.iter().all(|known| known.id != item.id) {
                        state.notifications.push(item);
                    }
                }
                state
                    .notifications
                    .sort_by(|a, b| b.timestamp_ms.cmp(&a.timestamp_ms));
                if state.notifications.len() > HISTORY_LIMIT {
                    state.notifications.truncate(HISTORY_LIMIT);
                }
            });
            if let Some(document) = web_sys::window().and_then(|window| window.document()) {
                let _ = render_notifications(&document);
            }
        }
        IpcMessage::RegionList { regions } => {
            STATE.with(|state| state.borrow_mut().regions = regions);
            if let Some(document) = web_sys::window().and_then(|window| window.document()) {
                render_region_chips(&document);
            }
        }
        IpcMessage::RegionSaved { region } => {
            STATE.with(|state| {
                let mut state = state.borrow_mut();
                state.regions.retain(|known| known.id != region.id);
                state.regions.push(region);
                state.regions.sort_by_key(|region| region.id);
            });
            if let Some(document) = web_sys::window().and_then(|window| window.document()) {
                render_region_chips(&document);
            }
        }
        IpcMessage::GotoRegion { id } => {
            // Pedido de um cliente externo (ou eco de outra aba): o frontend
            // anima até a região; o compositor acompanha o CanvasTransform.
            goto_region(id);
        }
        IpcMessage::TrayUpdate { items } => {
            STATE.with(|state| state.borrow_mut().tray_items = items);
            if let Some(document) = web_sys::window().and_then(|window| window.document()) {
                render_tray(&document);
            }
        }
        IpcMessage::TrayMenu { id, items } => {
            STATE.with(|state| state.borrow_mut().tray_menu_owner = Some(id));
            if let Some(document) = web_sys::window().and_then(|window| window.document()) {
                show_tray_menu(&document, &items);
            }
        }
        IpcMessage::MediaState {
            player,
            title,
            artist,
            status,
            can_next,
            can_prev,
        } => {
            STATE.with(|state| {
                let mut state = state.borrow_mut();
                state.media_player = player;
                state.media_title = title;
                state.media_artist = artist;
                state.media_status = status;
                state.media_can_next = can_next;
                state.media_can_prev = can_prev;
            });
            if let Some(document) = web_sys::window().and_then(|window| window.document()) {
                render_media(&document);
            }
        }
        IpcMessage::DndState { active } => {
            STATE.with(|state| state.borrow_mut().dnd = active);
            if let Some(document) = web_sys::window().and_then(|window| window.document()) {
                update_dnd_button(&document, active);
            }
        }
        IpcMessage::CloseWindow { .. } => {}
        IpcMessage::NotificationAction { .. }
        | IpcMessage::NotificationHold { .. }
        | IpcMessage::NotificationHistoryRequest
        | IpcMessage::RegionListRequest
        | IpcMessage::SaveRegion { .. }
        | IpcMessage::DeleteRegion { .. }
        | IpcMessage::TrayActivate { .. }
        | IpcMessage::TraySecondaryActivate { .. }
        | IpcMessage::TrayMenuRequest { .. }
        | IpcMessage::TrayRequest
        | IpcMessage::TrayMenuEvent { .. }
        | IpcMessage::MediaCommand { .. }
        | IpcMessage::MediaRequest => {}
        _ => {}
    }
}

fn apply_server_cluster(placements: &[ClusterPlacement]) {
    let Some(window) = web_sys::window() else {
        return;
    };
    let Some(document) = window.document() else {
        return;
    };

    for placement in placements {
        let Ok(id) = placement.id.parse::<u32>() else {
            continue;
        };
        // Tamanho zero significa cartão de demonstração: só a posição muda.
        let sized = placement.w > 0.0 && placement.h > 0.0;

        let card = STATE.with(|state| {
            let mut state = state.borrow_mut();
            let entry = match state.cards.iter_mut().find(|entry| entry.id == id) {
                Some(entry) => entry,
                None => {
                    state.cards.push(Card {
                        id,
                        x: placement.x,
                        y: placement.y,
                        w: placement.w.max(320.0),
                        h: placement.h.max(240.0),
                        has_window: true,
                    });
                    state.cards.last_mut().expect("cartão recém-criado")
                }
            };
            entry.x = placement.x;
            entry.y = placement.y;
            entry.has_window = true;
            if sized {
                entry.w = placement.w;
                entry.h = placement.h;
            }
            Some(*entry)
        });

        ensure_card(&document, id, &placement.title);
        if let Some(card) = card {
            apply_card_geometry(&document, card);
        }
    }

    let _ = update_glow(&document);
}

fn render_apps(apps: &[DesktopApp]) {
    let Some(window) = web_sys::window() else {
        return;
    };
    let Some(document) = window.document() else {
        return;
    };

    let (query, total) = STATE.with(|state| {
        let mut state = state.borrow_mut();
        state.apps = apps.to_vec();
        (state.app_query.clone(), state.apps.len())
    });

    if let Ok(count) = by_id(&document, "app-count") {
        let _ = count.set_inner_html(&total.to_string());
    }
    render_app_list(&document, &query);
}

fn render_app_list(document: &Document, query: &str) {
    let (apps, favorites, recent) = STATE.with(|state| {
        let state = state.borrow();
        (
            state.apps.clone(),
            state.favorites.clone(),
            state.recent.clone(),
        )
    });
    let normalized = query.trim().to_lowercase();
    let mut html = String::new();
    let mut shown = 0usize;
    let mut rendered = HashSet::new();

    if normalized.is_empty() {
        let favorites_apps: Vec<&DesktopApp> = favorites
            .iter()
            .filter_map(|exec| apps.iter().find(|app| &app.exec == exec))
            .collect();
        if !favorites_apps.is_empty() {
            html.push_str("<div class=\"app-section\">Favoritos</div>");
            for app in favorites_apps {
                html.push_str(&app_tile_html(app, true));
                rendered.insert(app.exec.clone());
                shown += 1;
            }
        }

        let recent_apps: Vec<&DesktopApp> = recent
            .iter()
            .filter(|exec| !rendered.contains(*exec))
            .filter_map(|exec| apps.iter().find(|app| &app.exec == exec))
            .collect();
        if !recent_apps.is_empty() {
            html.push_str("<div class=\"app-section\">Recentes</div>");
            for app in recent_apps {
                html.push_str(&app_tile_html(app, favorites.contains(&app.exec)));
                rendered.insert(app.exec.clone());
                shown += 1;
            }
        }

        let mut allapps: Vec<&DesktopApp> = apps
            .iter()
            .filter(|app| !rendered.contains(&app.exec))
            .collect();
        allapps.sort_by_key(|app| app.name.to_lowercase());
        if !allapps.is_empty() {
            html.push_str("<div class=\"app-section\">Todos os aplicativos</div>");
            for app in allapps {
                html.push_str(&app_tile_html(app, favorites.contains(&app.exec)));
                shown += 1;
            }
        }
    } else {
        let mut ranked: Vec<(i32, &DesktopApp)> = apps
            .iter()
            .filter_map(|app| {
                fuzzy_score(&normalized, &app.name, &app.exec).map(|score| {
                    let bonus = if favorites.contains(&app.exec) { 40 } else { 0 };
                    (score + bonus, app)
                })
            })
            .collect();
        ranked.sort_by(|left, right| {
            right
                .0
                .cmp(&left.0)
                .then_with(|| left.1.name.to_lowercase().cmp(&right.1.name.to_lowercase()))
        });
        for (_, app) in ranked {
            html.push_str(&app_tile_html(app, favorites.contains(&app.exec)));
            shown += 1;
        }
    }

    if shown == 0 {
        html = "<div class=\"app-drawer__empty\">Nenhum aplicativo encontrado.</div>".to_owned();
    }

    if let Ok(list) = by_id(document, "app-list") {
        let _ = list.set_inner_html(&html);
    }
    if let Ok(summary) = by_id(document, "app-drawer-summary") {
        let total = apps.len();
        let text = if normalized.is_empty() {
            let favorites_label = if favorites.len() == 1 {
                "1 favorito".to_owned()
            } else {
                format!("{} favoritos", favorites.len())
            };
            let recent_label = if recent.len() == 1 {
                "1 recente".to_owned()
            } else {
                format!("{} recentes", recent.len())
            };
            format!("{total} aplicativos · {favorites_label} · {recent_label}")
        } else {
            format!("{shown} de {total} aplicativos.")
        };
        let _ = summary.set_inner_html(&escape_html(&text));
    }
}

fn app_tile_html(app: &DesktopApp, favorite: bool) -> String {
    let name = if app.name.trim().is_empty() {
        "Aplicativo"
    } else {
        app.name.as_str()
    };
    let initial = name
        .chars()
        .next()
        .map(|character| character.to_uppercase().collect::<String>())
        .unwrap_or_else(|| "•".to_owned());
    let icon = if app.icon.trim().is_empty() {
        "application-x-executable"
    } else {
        app.icon.as_str()
    };
    let pressed = if favorite { "true" } else { "false" };
    let star_title = if favorite {
        "Remover dos favoritos"
    } else {
        "Fixar nos favoritos"
    };
    format!(
        "<div class=\"app-tile-wrap\">\
         <button class=\"app-tile\" type=\"button\" data-exec=\"{}\" data-name=\"{}\" title=\"{}\">\
         <span class=\"app-tile__icon\"><i>{}</i>\
         <img src=\"/icon/{}\" alt=\"\" loading=\"lazy\" onerror=\"this.style.display='none'\">\
         </span><span class=\"app-tile__name\">{}</span></button>\
         <button class=\"app-tile__fav\" type=\"button\" data-fav=\"{}\" aria-pressed=\"{pressed}\" title=\"{star_title}\" aria-label=\"{star_title}\">\
         <svg viewBox=\"0 0 24 24\" aria-hidden=\"true\"><path d=\"M12 3.2l2.7 5.5 6.1.9-4.4 4.3 1 6.1L12 17.1l-5.4 2.9 1-6.1L3.2 9.6l6.1-.9L12 3.2z\"/></svg>\
         </button></div>",
        escape_html(&app.exec),
        escape_html(name),
        escape_html(&app.exec),
        escape_html(&initial),
        percent_encode(icon),
        escape_html(name),
        escape_html(&app.exec),
    )
}

/// Codifica o nome do ícone para a rota `/icon/...` do compositor.
fn percent_encode(input: &str) -> String {
    let mut encoded = String::new();
    for byte in input.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-' | b'~') {
            encoded.push(byte as char);
        } else {
            encoded.push_str(&format!("%{byte:02X}"));
        }
    }
    encoded
}

/// Move o foco para o primeiro/último tile visível do launcher.
fn focus_app_tile(document: &Document, last: bool) {
    let Ok(list) = by_id(document, "app-list") else {
        return;
    };
    let Ok(tiles) = list.query_selector_all(".app-tile") else {
        return;
    };
    if tiles.length() == 0 {
        return;
    }
    let index = if last { tiles.length() - 1 } else { 0 };
    if let Some(tile) = tiles.get(index) {
        if let Ok(tile) = tile.dyn_into::<web_sys::HtmlElement>() {
            let _ = tile.focus();
        }
    }
}

/// Move o foco uma posição a partir do tile atual.
fn focus_adjacent_tile(document: &Document, delta: i32) {
    let Ok(list) = by_id(document, "app-list") else {
        return;
    };
    let Ok(tiles) = list.query_selector_all(".app-tile") else {
        return;
    };
    if tiles.length() == 0 {
        return;
    }
    let current = document
        .active_element()
        .and_then(|element| element.closest(".app-tile").ok().flatten())
        .and_then(|active| {
            (0..tiles.length()).find(|index| {
                tiles
                    .get(*index)
                    .map(|tile| tile.is_same_node(Some(&active)))
                    .unwrap_or(false)
            })
        });
    let next = match (current, delta.signum()) {
        (Some(index), 1) => (index + 1).min(tiles.length() - 1),
        (Some(index), -1) => index.saturating_sub(1),
        (None, -1) => 0,
        (None, _) => 0,
        _ => 0,
    };
    if let Some(tile) = tiles.get(next) {
        if let Ok(tile) = tile.dyn_into::<web_sys::HtmlElement>() {
            let _ = tile.focus();
        }
    }
}

fn load_launcher_state(window: &Window) {
    let favorites = load_string_list(window, FAVORITES_KEY);
    let recent = load_string_list(window, RECENT_KEY);
    STATE.with(|state| {
        let mut state = state.borrow_mut();
        state.favorites = favorites;
        state.recent = recent;
    });
}

fn load_string_list(window: &Window, key: &str) -> Vec<String> {
    let Some(storage) = window.local_storage().ok().flatten() else {
        return Vec::new();
    };
    let Ok(Some(raw)) = storage.get_item(key) else {
        return Vec::new();
    };
    serde_json::from_str::<Vec<String>>(&raw).unwrap_or_default()
}

fn save_string_list(window: &Window, key: &str, values: &[String]) {
    let Some(storage) = window.local_storage().ok().flatten() else {
        return;
    };
    if let Ok(json) = serde_json::to_string(values) {
        let _ = storage.set_item(key, &json);
    }
}

fn remember_recent(exec: &str) {
    let Some(window) = web_sys::window() else {
        return;
    };
    let recent = STATE.with(|state| {
        let mut state = state.borrow_mut();
        push_recent(&mut state.recent, exec, RECENT_LIMIT);
        state.recent.clone()
    });
    save_string_list(&window, RECENT_KEY, &recent);
}

fn setup_deck_apps(window: &Window, document: &Document) -> Result<(), JsValue> {
    if let Ok(button) = by_id(document, "apps-btn") {
        let doc = document.clone();
        let callback = Closure::<dyn FnMut()>::new(move || {
            toggle_app_drawer(&doc);
        });
        button.add_event_listener_with_callback("click", callback.as_ref().unchecked_ref())?;
        callback.forget();
    }

    if let Ok(button) = by_id(document, "app-drawer-close") {
        let doc = document.clone();
        let callback = Closure::<dyn FnMut()>::new(move || {
            let _ = set_app_drawer_open(&doc, false);
        });
        button.add_event_listener_with_callback("click", callback.as_ref().unchecked_ref())?;
        callback.forget();
    }

    if let Ok(input) = by_id(document, "app-search") {
        let input: HtmlInputElement = input.dyn_into()?;
        let doc = document.clone();
        let input_for_input = input.clone();
        let callback = Closure::<dyn FnMut(Event)>::new(move |_event: Event| {
            let query = input_for_input.value();
            STATE.with(|state| state.borrow_mut().app_query = query.clone());
            render_app_list(&doc, &query);
        });
        input.add_event_listener_with_callback("input", callback.as_ref().unchecked_ref())?;
        callback.forget();

        let doc = document.clone();
        let callback = Closure::<dyn FnMut(KeyboardEvent)>::new(move |event: KeyboardEvent| {
            match event.key().as_str() {
                "Escape" => {
                    event.prevent_default();
                    let _ = set_app_drawer_open(&doc, false);
                }
                "ArrowDown" => {
                    event.prevent_default();
                    focus_app_tile(&doc, false);
                }
                "ArrowUp" => {
                    event.prevent_default();
                    focus_app_tile(&doc, true);
                }
                "Enter" => {
                    if let Ok(Some(tile)) = by_id(&doc, "app-list")
                        .and_then(|list| list.query_selector(".app-tile"))
                    {
                        event.prevent_default();
                        if let Ok(tile) = tile.dyn_into::<web_sys::HtmlElement>() {
                            let _ = tile.click();
                        }
                    }
                }
                _ => {}
            }
        });
        input.add_event_listener_with_callback("keydown", callback.as_ref().unchecked_ref())?;
        callback.forget();
    }

    if let Ok(list) = by_id(document, "app-list") {
        let doc = document.clone();
        let callback = Closure::<dyn FnMut(Event)>::new(move |event: Event| {
            let Some(target) = event
                .target()
                .and_then(|target| target.dyn_into::<Element>().ok())
            else {
                return;
            };

            // Estrela de favoritos: alterna sem iniciar o aplicativo.
            if let Some(star) = target.closest(".app-tile__fav").ok().flatten() {
                event.prevent_default();
                event.stop_propagation();
                let Some(exec) = star.get_attribute("data-fav") else {
                    return;
                };
                let favorites = STATE.with(|state| {
                    let mut state = state.borrow_mut();
                    toggle_favorite(&mut state.favorites, &exec);
                    state.favorites.clone()
                });
                if let Some(window) = web_sys::window() {
                    save_string_list(&window, FAVORITES_KEY, &favorites);
                }
                let query = STATE.with(|state| state.borrow().app_query.clone());
                render_app_list(&doc, &query);
                return;
            }

            let Some(button) = target.closest("[data-exec]").ok().flatten() else {
                return;
            };
            let Some(exec) = button.get_attribute("data-exec") else {
                return;
            };
            if exec.trim().is_empty() {
                return;
            }
            let name = button
                .get_attribute("data-name")
                .unwrap_or_else(|| "aplicativo".to_owned());
            send_launch_app(&exec);
            remember_recent(&exec);
            let _ = set_app_drawer_open(&doc, false);
            show_toast(&doc, &format!("Abrindo {name}…"));
        });
        list.add_event_listener_with_callback("click", callback.as_ref().unchecked_ref())?;
        callback.forget();

        let doc = document.clone();
        let list_for_keys = list.clone();
        let callback = Closure::<dyn FnMut(KeyboardEvent)>::new(move |event: KeyboardEvent| {
            match event.key().as_str() {
                "Escape" => {
                    event.prevent_default();
                    let _ = set_app_drawer_open(&doc, false);
                }
                "ArrowDown" | "ArrowRight" => {
                    event.prevent_default();
                    focus_adjacent_tile(&doc, 1);
                }
                "ArrowUp" | "ArrowLeft" => {
                    event.prevent_default();
                    focus_adjacent_tile(&doc, -1);
                }
                "Home" => {
                    event.prevent_default();
                    if let Ok(Some(tile)) = list_for_keys.query_selector(".app-tile") {
                        if let Ok(tile) = tile.dyn_into::<web_sys::HtmlElement>() {
                            let _ = tile.focus();
                        }
                    }
                }
                "End" => {
                    event.prevent_default();
                    focus_app_tile(&doc, true);
                }
                _ => {}
            }
        });
        list.add_event_listener_with_callback("keydown", callback.as_ref().unchecked_ref())?;
        callback.forget();
    }

    let doc = document.clone();
    let callback = Closure::<dyn FnMut(PointerEvent)>::new(move |event: PointerEvent| {
        let Some(target) = event
            .target()
            .and_then(|target| target.dyn_into::<Element>().ok())
        else {
            return;
        };
        if target
            .closest("#app-drawer, #deck, #apps-btn")
            .ok()
            .flatten()
            .is_some()
        {
            return;
        }
        let _ = set_app_drawer_open(&doc, false);
    });
    document.add_event_listener_with_callback("pointerdown", callback.as_ref().unchecked_ref())?;
    callback.forget();

    let _ = window;
    Ok(())
}

fn toggle_app_drawer(document: &Document) {
    let open = by_id(document, "app-drawer")
        .map(|drawer| drawer.has_attribute("hidden"))
        .unwrap_or(false);
    let _ = set_app_drawer_open(document, open);
}

fn set_app_drawer_open(document: &Document, open: bool) -> Result<(), JsValue> {
    let drawer = by_id(document, "app-drawer")?;
    if open {
        drawer.remove_attribute("hidden")?;
    } else {
        drawer.set_attribute("hidden", "")?;
    }
    // Com a gaveta aberta, o conjunto continua ancorado na base da tela e
    // cresce para cima; o deck e a lista dividem o mesmo trapézio.
    if let Ok(shell) = by_id(document, "deck-shell") {
        let _ = shell.class_list().toggle_with_force("open", open);
    }
    if let Ok(button) = by_id(document, "apps-btn") {
        button.set_attribute("aria-expanded", if open { "true" } else { "false" })?;
    }
    send_chrome_rects(document);
    if let Ok(input) = by_id(document, "app-search") {
        let input: HtmlInputElement = input.dyn_into()?;
        if open {
            let _ = input.focus();
        } else {
            let _ = input.blur();
        }
    }
    Ok(())
}

/// Desliga a ferramenta de redimensionamento (chamada também pelos atalhos
/// de desenho, que não passam pelo clique nos botões).
pub(crate) fn disable_resize_tool(document: &Document) {
    let was_active = STATE.with(|state| {
        let mut state = state.borrow_mut();
        let was_active = state.resize_tool;
        state.resize_tool = false;
        was_active
    });
    if !was_active {
        return;
    }
    if let Ok(button) = by_id(document, "tool-resize") {
        let _ = button.set_attribute("aria-pressed", "false");
    }
    send_message(&IpcMessage::WindowResizeTool { active: false });
}

pub(crate) fn close_app_drawer(document: &Document) {
    let _ = set_app_drawer_open(document, false);
}

fn send_launch_app(exec: &str) {
    send_message(&IpcMessage::LaunchApp {
        exec: exec.to_owned(),
    });
}



// ---------------------------------------------------------------------------
// Barra de módulos, relógio/calendário e regiões do canvas
// ---------------------------------------------------------------------------

impl BarConfig {
    fn load(window: &Window) -> Self {
        let Some(storage) = window.local_storage().ok().flatten() else {
            return Self::default();
        };
        let Some(text) = storage.get_item(BAR_KEY).ok().flatten() else {
            return Self::default();
        };
        let mut config: Self = serde_json::from_str(&text).unwrap_or_default();

        // Garante todos os módulos conhecidos e descarta ids antigos.
        config
            .modules
            .retain(|module| BAR_MODULES.iter().any(|(id, _, _)| *id == module.id));
        for (id, _, slot) in BAR_MODULES {
            if !config.modules.iter().any(|module| module.id == id) {
                config.modules.push(BarModule {
                    id: id.to_owned(),
                    slot: slot.to_owned(),
                });
            }
        }
        for module in &mut config.modules {
            if !matches!(module.slot.as_str(), "left" | "center" | "right" | "hidden") {
                module.slot = "right".to_owned();
            }
        }
        config
    }

    fn save(&self, window: &Window) {
        if let Ok(Some(storage)) = window.local_storage() {
            if let Ok(json) = serde_json::to_string(self) {
                let _ = storage.set_item(BAR_KEY, &json);
            }
        }
    }

    fn label(id: &str) -> &'static str {
        match BAR_MODULES.iter().find(|(module, _, _)| *module == id) {
            Some((_, label, _)) => *label,
            None => "Módulo",
        }
    }
}

fn setup_bar(window: &Window, document: &Document) -> Result<(), JsValue> {
    let config = BarConfig::load(window);
    STATE.with(|state| state.borrow_mut().bar = config.clone());
    apply_bar_config(document, &config);
    render_bar_settings(document, &config);

    // Tema: botão compacto abre a lista de chips.
    if let Ok(button) = by_id(document, "theme-menu-btn") {
        let document_for_click = document.clone();
        let click = Closure::<dyn FnMut()>::new(move || {
            let open = is_hidden(&document_for_click, "theme-popover");
            close_popover_if_open(&document_for_click, "bar-settings-panel", "bar-settings-btn");
            close_popover_if_open(&document_for_click, "calendar-popover", "clock-btn");
            set_popover(&document_for_click, "theme-popover", "theme-menu-btn", open);
        });
        button.add_event_listener_with_callback("click", click.as_ref().unchecked_ref())?;
        click.forget();
    }

    // Configuração da barra.
    if let Ok(button) = by_id(document, "bar-settings-btn") {
        let document_for_click = document.clone();
        let click = Closure::<dyn FnMut()>::new(move || {
            let open = is_hidden(&document_for_click, "bar-settings-panel");
            close_popover_if_open(&document_for_click, "theme-popover", "theme-menu-btn");
            close_popover_if_open(&document_for_click, "calendar-popover", "clock-btn");
            set_popover(
                &document_for_click,
                "bar-settings-panel",
                "bar-settings-btn",
                open,
            );
        });
        button.add_event_listener_with_callback("click", click.as_ref().unchecked_ref())?;
        click.forget();
    }

    // Slots (change nos selects) e ordem (↑/↓).
    if let Ok(panel) = by_id(document, "bar-settings-panel") {
        let document_for_change = document.clone();
        let change = Closure::<dyn FnMut(Event)>::new(move |event: Event| {
            let Some(select) = event
                .target()
                .and_then(|target| target.dyn_into::<HtmlSelectElement>().ok())
            else {
                return;
            };
            let Some(id) = select.get_attribute("data-bar-slot") else {
                return;
            };
            let slot = select.value();
            update_bar_slot(&document_for_change, &id, &slot);
        });
        panel.add_event_listener_with_callback("change", change.as_ref().unchecked_ref())?;
        change.forget();

        let document_for_move = document.clone();
        let click = Closure::<dyn FnMut(Event)>::new(move |event: Event| {
            let Some(target) = event
                .target()
                .and_then(|target| target.dyn_into::<Element>().ok())
            else {
                return;
            };
            let Ok(Some(button)) = target.closest("[data-bar-move]") else {
                return;
            };
            let Some(id) = button.get_attribute("data-module") else {
                return;
            };
            let Some(delta) = button
                .get_attribute("data-bar-move")
                .and_then(|value| value.parse::<i32>().ok())
            else {
                return;
            };
            move_bar_module(&document_for_move, &id, delta);
        });
        panel.add_event_listener_with_callback("click", click.as_ref().unchecked_ref())?;
        click.forget();
    }

    // Relógio em 12/24h.
    if let Ok(element) = by_id(document, "bar-hour24") {
        let Ok(input) = element.dyn_into::<HtmlInputElement>() else {
            return Ok(());
        };
        let input_for_change = input.clone();
        let document_for_change = document.clone();
        let window_for_change = window.clone();
        let change = Closure::<dyn FnMut()>::new(move || {
            let checked = input_for_change.checked();
            let config = STATE.with(|state| {
                let mut state = state.borrow_mut();
                state.bar.hour24 = checked;
                state.bar.clone()
            });
            config.save(&window_for_change);
            update_clock(&document_for_change);
        });
        input.add_event_listener_with_callback("change", change.as_ref().unchecked_ref())?;
        change.forget();
    }

    let _ = bind_clock_and_calendar(document);
    let _ = bind_regions(document);
    start_clock_ticker();
    start_animation_ticker();
    update_clock(document);
    let _ = render_region_chips(document);

    Ok(())
}

fn is_hidden(document: &Document, id: &str) -> bool {
    document
        .get_element_by_id(id)
        .map(|element| element.has_attribute("hidden"))
        .unwrap_or(true)
}

fn set_popover(document: &Document, popover: &str, button: &str, open: bool) {
    if let Some(element) = document.get_element_by_id(popover) {
        if open {
            let _ = element.remove_attribute("hidden");
        } else {
            let _ = element.set_attribute("hidden", "");
        }
    }
    if let Some(element) = document.get_element_by_id(button) {
        let _ = element.set_attribute("aria-expanded", if open { "true" } else { "false" });
    }
    if popover == "calendar-popover" && open {
        render_calendar(document);
    }
    send_chrome_rects(document);
}

/// Fecha um popover da barra, se estiver aberto.
fn close_popover_if_open(document: &Document, popover: &str, button: &str) {
    if !is_hidden(document, popover) {
        set_popover(document, popover, button, false);
    }
}

fn apply_bar_config(document: &Document, config: &BarConfig) {
    for module in &config.modules {
        let Some(element) = document.get_element_by_id(&format!("bar-mod-{}", module.id)) else {
            continue;
        };
        if module.slot == "hidden" {
            let _ = element.set_attribute("hidden", "");
            continue;
        }
        let _ = element.remove_attribute("hidden");
        if let Some(parent) = document.get_element_by_id(&format!("bar-{}", module.slot)) {
            let _ = parent.append_child(&element);
        }
    }
}

fn slot_select_html(slot: &str) -> String {
    [
        ("left", "Esquerda"),
        ("center", "Centro"),
        ("right", "Direita"),
        ("hidden", "Oculto"),
    ]
    .iter()
    .map(|(value, label)| {
        format!(
            "<option value=\"{value}\"{}>{label}</option>",
            if *value == slot { " selected" } else { "" }
        )
    })
    .collect::<String>()
}

fn render_bar_settings(document: &Document, config: &BarConfig) {
    let html = config
        .modules
        .iter()
        .map(|module| {
            let id = escape_html(&module.id);
            format!(
                "<div class=\"bar-settings__row\">\
                 <span class=\"bar-settings__name\">{label}</span>\
                 <select data-bar-slot=\"{id}\">{options}</select>\
                 <button type=\"button\" class=\"region-btn\" data-bar-move=\"-1\" data-module=\"{id}\" title=\"Mover para cima\">↑</button>\
                 <button type=\"button\" class=\"region-btn\" data-bar-move=\"1\" data-module=\"{id}\" title=\"Mover para baixo\">↓</button>\
                 </div>",
                label = escape_html(BarConfig::label(&module.id)),
                options = slot_select_html(&module.slot),
            )
        })
        .collect::<String>();
    if let Ok(list) = by_id(document, "bar-settings-list") {
        let _ = list.set_inner_html(&html);
    }
    if let Some(element) = document.get_element_by_id("bar-hour24") {
        if let Ok(input) = element.dyn_into::<HtmlInputElement>() {
            input.set_checked(config.hour24);
        }
    }
}

fn persist_bar(config: &BarConfig) {
    if let Some(window) = web_sys::window() {
        config.save(&window);
    }
}

fn update_bar_slot(document: &Document, id: &str, slot: &str) {
    let config = STATE.with(|state| {
        let mut state = state.borrow_mut();
        if let Some(module) = state.bar.modules.iter_mut().find(|module| module.id == id) {
            module.slot = slot.to_owned();
        }
        state.bar.clone()
    });
    persist_bar(&config);
    apply_bar_config(document, &config);
    render_bar_settings(document, &config);
    send_chrome_rects(document);
}

fn move_bar_module(document: &Document, id: &str, delta: i32) {
    let config = STATE.with(|state| {
        let mut state = state.borrow_mut();
        let Some(index) = state.bar.modules.iter().position(|module| module.id == id) else {
            return state.bar.clone();
        };
        let slot = state.bar.modules[index].slot.clone();
        let same_slot: Vec<usize> = state
            .bar
            .modules
            .iter()
            .enumerate()
            .filter(|(_, module)| module.slot == slot)
            .map(|(index, _)| index)
            .collect();
        if let Some(position) = same_slot.iter().position(|candidate| *candidate == index) {
            let target = (position as i32 + delta).clamp(0, same_slot.len() as i32 - 1) as usize;
            if target != position {
                state.bar.modules.swap(same_slot[position], same_slot[target]);
            }
        }
        state.bar.clone()
    });
    persist_bar(&config);
    apply_bar_config(document, &config);
    render_bar_settings(document, &config);
    send_chrome_rects(document);
}

fn bind_clock_and_calendar(document: &Document) -> Result<(), JsValue> {
    if let Ok(button) = by_id(document, "clock-btn") {
        let document_for_click = document.clone();
        let click = Closure::<dyn FnMut()>::new(move || {
            let open = is_hidden(&document_for_click, "calendar-popover");
            if open {
                let now = js_sys::Date::new_0();
                STATE.with(|state| {
                    let mut state = state.borrow_mut();
                    state.calendar_year = now.get_full_year() as i32;
                    state.calendar_month = now.get_month();
                });
            }
            close_popover_if_open(&document_for_click, "theme-popover", "theme-menu-btn");
            close_popover_if_open(&document_for_click, "bar-settings-panel", "bar-settings-btn");
            set_popover(&document_for_click, "calendar-popover", "clock-btn", open);
        });
        button.add_event_listener_with_callback("click", click.as_ref().unchecked_ref())?;
        click.forget();
    }

    for (id, delta) in [("calendar-prev", -1_i32), ("calendar-next", 1_i32)] {
        let Ok(button) = by_id(document, id) else {
            continue;
        };
        let document_for_click = document.clone();
        let click = Closure::<dyn FnMut()>::new(move || {
            STATE.with(|state| {
                let mut state = state.borrow_mut();
                let month = state.calendar_month as i32 + delta;
                if month < 0 {
                    state.calendar_month = 11;
                    state.calendar_year -= 1;
                } else if month > 11 {
                    state.calendar_month = 0;
                    state.calendar_year += 1;
                } else {
                    state.calendar_month = month as u32;
                }
            });
            render_calendar(&document_for_click);
        });
        button.add_event_listener_with_callback("click", click.as_ref().unchecked_ref())?;
        click.forget();
    }
    Ok(())
}

fn render_calendar(document: &Document) {
    let (year, month) = STATE.with(|state| {
        let state = state.borrow();
        (state.calendar_year, state.calendar_month)
    });
    if year == 0 {
        return;
    }

    let first = js_sys::Date::new_with_year_month_day(year as u32, month as i32, 1);
    let start = first.get_day();
    let days =
        js_sys::Date::new_with_year_month_day(year as u32, month as i32 + 1, 0).get_date();
    let today = js_sys::Date::new_0();
    let same_month = today.get_full_year() as i32 == year && today.get_month() == month;

    let mut cells = String::new();
    for _ in 0..start {
        cells.push_str("<span class=\"calendar__day calendar__day--empty\"></span>");
    }
    for day in 1..=days {
        let class = if same_month && today.get_date() == day {
            "calendar__day calendar__day--today"
        } else {
            "calendar__day"
        };
        cells.push_str(&format!("<span class=\"{class}\">{day}</span>"));
    }
    let total = start + days;
    let rest = (7 - (total % 7)) % 7;
    for _ in 0..rest {
        cells.push_str("<span class=\"calendar__day calendar__day--empty\"></span>");
    }

    if let Some(grid) = document.get_element_by_id("calendar-grid") {
        grid.set_inner_html(&cells);
    }
    if let Some(label) = document.get_element_by_id("calendar-label") {
        let text = format!("{} {}", MONTHS_FULL[month as usize], year);
        let _ = label.set_text_content(Some(&text));
    }
}

fn update_clock(document: &Document) {
    let now = js_sys::Date::new_0();
    let hour24 = STATE.with(|state| state.borrow().bar.hour24);
    let hours = now.get_hours();
    let minutes = now.get_minutes();
    let time = if hour24 {
        format!("{hours:02}:{minutes:02}")
    } else {
        let hour12 = if hours % 12 == 0 { 12 } else { hours % 12 };
        let suffix = if hours < 12 { "AM" } else { "PM" };
        format!("{hour12}:{minutes:02} {suffix}")
    };
    if let Some(element) = document.get_element_by_id("clock-time") {
        let _ = element.set_text_content(Some(&time));
    }
    if let Some(element) = document.get_element_by_id("clock-date") {
        let date = format!(
            "{}, {} {}",
            WEEKDAYS[now.get_day() as usize],
            now.get_date(),
            MONTHS[now.get_month() as usize]
        );
        let _ = element.set_text_content(Some(&date));
    }
}

fn start_clock_ticker() {
    let callback = Closure::<dyn FnMut()>::new(|| {
        if let Some(document) = web_sys::window().and_then(|window| window.document()) {
            update_clock(&document);
        }
    });
    if let Some(window) = web_sys::window() {
        let _ = window.set_interval_with_callback_and_timeout_and_arguments_0(
            callback.as_ref().unchecked_ref(),
            1_000,
        );
    }
    callback.forget();
}

fn bind_regions(document: &Document) -> Result<(), JsValue> {
    if let Ok(chips) = by_id(document, "region-chips") {
        let click = Closure::<dyn FnMut(Event)>::new(move |event: Event| {
            let Some(target) = event
                .target()
                .and_then(|target| target.dyn_into::<Element>().ok())
            else {
                return;
            };
            let Ok(Some(chip)) = target.closest("[data-region-id]") else {
                return;
            };
            let Some(id) = chip
                .get_attribute("data-region-id")
                .and_then(|value| value.parse::<u32>().ok())
            else {
                return;
            };
            if target.closest("[data-region-delete]").ok().flatten().is_some() {
                send_message(&IpcMessage::DeleteRegion { id });
            } else {
                goto_region(id);
                send_message(&IpcMessage::GotoRegion { id });
            }
        });
        chips.add_event_listener_with_callback("click", click.as_ref().unchecked_ref())?;
        click.forget();
    }

    if let Ok(button) = by_id(document, "region-save") {
        let document_for_click = document.clone();
        let click = Closure::<dyn FnMut()>::new(move || {
            open_region_form(&document_for_click);
        });
        button.add_event_listener_with_callback("click", click.as_ref().unchecked_ref())?;
        click.forget();
    }

    if let Ok(form) = by_id(document, "region-save-form") {
        let document_for_submit = document.clone();
        let submit = Closure::<dyn FnMut(Event)>::new(move |event: Event| {
            event.prevent_default();
            let Some(input) = document_for_submit.get_element_by_id("region-name") else {
                return;
            };
            let name = input
                .dyn_into::<HtmlInputElement>()
                .map(|input| input.value())
                .unwrap_or_default();
            let name = name.trim().to_owned();
            close_region_form(&document_for_submit);
            if name.is_empty() {
                return;
            }
            let (scale, offset_x, offset_y) = STATE.with(|state| {
                let state = state.borrow();
                (state.scale, state.offset_x, state.offset_y)
            });
            send_message(&IpcMessage::SaveRegion {
                name,
                scale,
                offset_x,
                offset_y,
            });
        });
        form.add_event_listener_with_callback("submit", submit.as_ref().unchecked_ref())?;
        submit.forget();
    }

    if let Ok(button) = by_id(document, "region-cancel") {
        let document_for_click = document.clone();
        let click = Closure::<dyn FnMut()>::new(move || {
            close_region_form(&document_for_click);
        });
        button.add_event_listener_with_callback("click", click.as_ref().unchecked_ref())?;
        click.forget();
    }

    if let Ok(input) = by_id(document, "region-name") {
        let document_for_key = document.clone();
        let key = Closure::<dyn FnMut(KeyboardEvent)>::new(move |event: KeyboardEvent| {
            if event.key() == "Escape" {
                close_region_form(&document_for_key);
                event.prevent_default();
            }
        });
        input.add_event_listener_with_callback("keydown", key.as_ref().unchecked_ref())?;
        key.forget();
    }

    Ok(())
}

fn open_region_form(document: &Document) {
    if let Some(form) = document.get_element_by_id("region-save-form") {
        let _ = form.remove_attribute("hidden");
    }
    if let Some(input) = document.get_element_by_id("region-name") {
        if let Ok(input) = input.dyn_into::<HtmlInputElement>() {
            input.set_value("");
            let _ = input.focus();
        }
    }
    send_chrome_rects(document);
}

fn close_region_form(document: &Document) {
    if let Some(form) = document.get_element_by_id("region-save-form") {
        let _ = form.set_attribute("hidden", "");
    }
    send_chrome_rects(document);
}

fn render_region_chips(document: &Document) {
    let (regions, scale, offset_x, offset_y) = STATE.with(|state| {
        let state = state.borrow();
        (state.regions.clone(), state.scale, state.offset_x, state.offset_y)
    });
    let active = active_region_id(&regions, scale, offset_x, offset_y);
    let html = regions
        .iter()
        .map(|region| {
            let name = escape_html(&region.name);
            format!(
                "<span class=\"region-chip\" data-region-id=\"{id}\" data-active=\"{active}\" title=\"{name} · {zoom}%\">\
                 <button type=\"button\" class=\"region-chip__go\" data-region-go=\"{id}\">{name}</button>\
                 <button type=\"button\" class=\"region-chip__del\" data-region-delete=\"{id}\" aria-label=\"Excluir região {name}\">×</button>\
                 </span>",
                id = region.id,
                active = active == Some(region.id),
                zoom = (region.scale * 100.0).round() as i64,
            )
        })
        .collect::<String>();
    if let Ok(chips) = by_id(document, "region-chips") {
        chips.set_inner_html(&html);
    }
    send_chrome_rects(document);
}

fn active_region_id(
    regions: &[CanvasRegion],
    scale: f64,
    offset_x: f64,
    offset_y: f64,
) -> Option<u32> {
    regions
        .iter()
        .map(|region| (region.id, region.distance(scale, offset_x, offset_y)))
        .min_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal))
        .filter(|(_, distance)| *distance < 24.0)
        .map(|(id, _)| id)
}

/// Atualiza o destaque da região ativa sem reconstruir os chips.
fn update_region_active(document: &Document) {
    let (regions, scale, offset_x, offset_y) = STATE.with(|state| {
        let state = state.borrow();
        (state.regions.clone(), state.scale, state.offset_x, state.offset_y)
    });
    if regions.is_empty() {
        return;
    }
    let active = active_region_id(&regions, scale, offset_x, offset_y);
    if let Ok(nodes) = document.query_selector_all("[data-region-id]") {
        for index in 0..nodes.length() {
            let Some(node) = nodes.item(index) else {
                continue;
            };
            let Ok(chip) = node.dyn_into::<Element>() else {
                continue;
            };
            let id = chip
                .get_attribute("data-region-id")
                .and_then(|value| value.parse::<u32>().ok());
            let pressed = if id.is_some() && id == active { "true" } else { "false" };
            let _ = chip.set_attribute("data-active", pressed);
        }
    }
}

fn goto_region(id: u32) {
    let region = STATE.with(|state| {
        state
            .borrow()
            .regions
            .iter()
            .find(|region| region.id == id)
            .cloned()
    });
    let Some(region) = region else {
        return;
    };
    let from = STATE.with(|state| {
        let state = state.borrow();
        (state.scale, state.offset_x, state.offset_y)
    });
    STATE.with(|state| {
        state.borrow_mut().anim = Some(TransformAnim {
            start_ms: js_sys::Date::now(),
            from,
            to: (region.scale, region.offset_x, region.offset_y),
        });
    });
}

fn goto_region_by_index(index: usize) {
    let id = STATE.with(|state| state.borrow().regions.get(index).map(|region| region.id));
    if let Some(id) = id {
        goto_region(id);
        send_message(&IpcMessage::GotoRegion { id });
    }
}

fn start_animation_ticker() {
    let callback = Closure::<dyn FnMut()>::new(|| {
        let animating = STATE.with(|state| {
            let mut state = state.borrow_mut();
            let now = js_sys::Date::now();
            let (start_ms, from, to) = match state.anim.as_ref() {
                Some(anim) => (anim.start_ms, anim.from, anim.to),
                None => return false,
            };
            let t = ((now - start_ms) / 250.0).clamp(0.0, 1.0);
            let eased = t * t * (3.0 - 2.0 * t);
            state.scale = from.0 + (to.0 - from.0) * eased;
            state.offset_x = from.1 + (to.1 - from.1) * eased;
            state.offset_y = from.2 + (to.2 - from.2) * eased;
            if t >= 1.0 {
                state.anim = None;
            }
            true
        });
        if !animating {
            return;
        }
        if let Some(window) = web_sys::window() {
            if let Some(document) = window.document() {
                let _ = apply_transform(&document);
                send_transform();
            }
        }
    });
    if let Some(window) = web_sys::window() {
        let _ = window.set_interval_with_callback_and_timeout_and_arguments_0(
            callback.as_ref().unchecked_ref(),
            16,
        );
    }
    callback.forget();
}


// ---------------------------------------------------------------------------
// Bandeja do sistema (SNI) e mídia (MPRIS)
// ---------------------------------------------------------------------------

fn setup_tray_media(document: &Document) -> Result<(), JsValue> {
    if let Ok(container) = by_id(document, "tray-items") {
        let click = Closure::<dyn FnMut(Event)>::new(move |event: Event| {
            let Some(target) = event
                .target()
                .and_then(|target| target.dyn_into::<Element>().ok())
            else {
                return;
            };
            let Ok(Some(button)) = target.closest("[data-tray-id]") else {
                return;
            };
            if let Some(id) = button.get_attribute("data-tray-id") {
                send_message(&IpcMessage::TrayActivate { id });
            }
        });
        container.add_event_listener_with_callback("click", click.as_ref().unchecked_ref())?;
        click.forget();

        let context = Closure::<dyn FnMut(Event)>::new(move |event: Event| {
            let Some(target) = event
                .target()
                .and_then(|target| target.dyn_into::<Element>().ok())
            else {
                return;
            };
            let Ok(Some(button)) = target.closest("[data-tray-id]") else {
                return;
            };
            event.prevent_default();
            if let Some(id) = button.get_attribute("data-tray-id") {
                send_message(&IpcMessage::TrayMenuRequest { id });
            }
        });
        container
            .add_event_listener_with_callback("contextmenu", context.as_ref().unchecked_ref())?;
        context.forget();
    }

    if let Ok(menu) = by_id(document, "tray-menu") {
        let document_for_click = document.clone();
        let click = Closure::<dyn FnMut(Event)>::new(move |event: Event| {
            let Some(target) = event
                .target()
                .and_then(|target| target.dyn_into::<Element>().ok())
            else {
                return;
            };
            let Ok(Some(button)) = target.closest("[data-tray-menu-id]") else {
                return;
            };
            let Some(event_id) = button
                .get_attribute("data-tray-menu-id")
                .and_then(|value| value.parse::<i32>().ok())
            else {
                return;
            };
            let owner = STATE.with(|state| state.borrow().tray_menu_owner.clone());
            if let Some(owner) = owner {
                send_message(&IpcMessage::TrayMenuEvent {
                    id: owner,
                    event_id,
                });
            }
            hide_tray_menu(&document_for_click);
        });
        menu.add_event_listener_with_callback("click", click.as_ref().unchecked_ref())?;
        click.forget();
    }

    for (id, action) in [
        ("media-prev", MediaAction::Previous),
        ("media-play", MediaAction::PlayPause),
        ("media-next", MediaAction::Next),
    ] {
        let Ok(button) = by_id(document, id) else {
            continue;
        };
        let click = Closure::<dyn FnMut()>::new(move || {
            send_message(&IpcMessage::MediaCommand { command: action });
        });
        button.add_event_listener_with_callback("click", click.as_ref().unchecked_ref())?;
        click.forget();
    }

    // Clique fora fecha o menu da bandeja.
    let document_for_outside = document.clone();
    let outside = Closure::<dyn FnMut(Event)>::new(move |event: Event| {
        let Some(target) = event
            .target()
            .and_then(|target| target.dyn_into::<Element>().ok())
        else {
            return;
        };
        if target
            .closest("#tray-menu, #tray-items")
            .ok()
            .flatten()
            .is_some()
        {
            return;
        }
        hide_tray_menu(&document_for_outside);
    });
    document.add_event_listener_with_callback("pointerdown", outside.as_ref().unchecked_ref())?;
    outside.forget();

    Ok(())
}

fn render_tray(document: &Document) {
    let items = STATE.with(|state| state.borrow().tray_items.clone());
    let html = items.iter().map(tray_item_html).collect::<String>();
    if let Ok(container) = by_id(document, "tray-items") {
        container.set_inner_html(&html);
    }

    let owner = STATE.with(|state| state.borrow().tray_menu_owner.clone());
    if let Some(owner) = owner {
        if !items.iter().any(|item| item.id == owner) {
            hide_tray_menu(document);
        }
    }
    send_chrome_rects(document);
}

fn tray_item_html(item: &TrayItem) -> String {
    let initial = item
        .title
        .chars()
        .find(|character| !character.is_whitespace())
        .map(|character| character.to_uppercase().collect::<String>())
        .unwrap_or_else(|| "•".to_owned());
    let icon = if item.icon_name.is_empty() {
        String::new()
    } else {
        format!(
            "<img src=\"/icon/{}\" alt=\"\" onerror=\"this.style.display='none';this.nextElementSibling.style.display='grid'\">",
            percent_encode(&item.icon_name)
        )
    };
    format!(
        "<button class=\"tray-item\" type=\"button\" data-tray-id=\"{id}\" data-status=\"{status}\" title=\"{title}\">{icon}\
         <span class=\"tray-item__fallback\"{fallback_style}>{initial}</span></button>",
        id = escape_html(&item.id),
        status = escape_html(&item.status),
        title = escape_html(&item.title),
        initial = escape_html(&initial),
        fallback_style = if item.icon_name.is_empty() {
            ""
        } else {
            " style=\"display:none\""
        },
    )
}

fn show_tray_menu(document: &Document, items: &[TrayMenuEntry]) {
    let Some(menu) = document.get_element_by_id("tray-menu") else {
        return;
    };
    let html = if items.is_empty() {
        "<p class=\"notification-empty\">Este item não expõe menu.</p>".to_owned()
    } else {
        format!(
            "<ul class=\"tray-menu__list\">{}</ul>",
            menu_entries_html(items)
        )
    };
    let _ = menu.set_inner_html(&html);
    let _ = menu.remove_attribute("hidden");
    send_chrome_rects(document);
}

fn hide_tray_menu(document: &Document) {
    let Some(menu) = document.get_element_by_id("tray-menu") else {
        return;
    };
    if menu.has_attribute("hidden") {
        return;
    }
    let _ = menu.set_attribute("hidden", "");
    STATE.with(|state| state.borrow_mut().tray_menu_owner = None);
    send_chrome_rects(document);
}

fn menu_entries_html(entries: &[TrayMenuEntry]) -> String {
    entries
        .iter()
        .filter(|entry| entry.visible)
        .map(|entry| {
            if entry.is_separator {
                return "<li class=\"tray-menu__sep\"></li>".to_owned();
            }
            let label = escape_html(&strip_mnemonic(&entry.label));
            if entry.children.is_empty() {
                format!(
                    "<li><button type=\"button\" class=\"tray-menu__item\" data-tray-menu-id=\"{id}\"{disabled}>{label}</button></li>",
                    id = entry.id,
                    disabled = if entry.enabled { "" } else { " disabled" },
                )
            } else {
                format!(
                    "<li><span class=\"tray-menu__label\">{label}</span>\
                     <ul class=\"tray-menu__list tray-menu__children\">{children}</ul></li>",
                    children = menu_entries_html(&entry.children),
                )
            }
        })
        .collect()
}

/// Remove mnemônicos de GTK (`_Abrir`) e Qt (`&Abrir`).
fn strip_mnemonic(label: &str) -> String {
    let mut output = String::with_capacity(label.len());
    let mut characters = label.chars().peekable();
    while let Some(character) = characters.next() {
        match character {
            '_' => {}
            '&' => {
                if characters.peek() == Some(&'&') {
                    output.push('&');
                    characters.next();
                }
            }
            _ => output.push(character),
        }
    }
    output
}

fn render_media(document: &Document) {
    let (player, title, artist, status, can_next, can_prev) = STATE.with(|state| {
        let state = state.borrow();
        (
            state.media_player.clone(),
            state.media_title.clone(),
            state.media_artist.clone(),
            state.media_status.clone(),
            state.media_can_next,
            state.media_can_prev,
        )
    });

    let Some(body) = document.get_element_by_id("media-body") else {
        return;
    };
    if player.is_empty() {
        let _ = body.set_attribute("hidden", "");
        return;
    }
    let _ = body.remove_attribute("hidden");
    let _ = body.set_attribute("title", &player);

    if let Some(element) = document.get_element_by_id("media-title") {
        let text = if title.is_empty() {
            "Sem título".to_owned()
        } else {
            title
        };
        let _ = element.set_text_content(Some(&text));
    }
    if let Some(element) = document.get_element_by_id("media-artist") {
        let _ = element.set_text_content(Some(&artist));
    }
    if let Ok(button) = by_id(document, "media-play") {
        let _ = button.set_text_content(Some(if status == "Playing" { "||" } else { "▶" }));
    }
    for (id, enabled) in [("media-prev", can_prev), ("media-next", can_next)] {
        if let Ok(button) = by_id(document, id) {
            if enabled {
                let _ = button.remove_attribute("disabled");
            } else {
                let _ = button.set_attribute("disabled", "");
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Notificações
// ---------------------------------------------------------------------------

/// Mesmo padrão do daemon: `0` nunca expira, `> 0` milissegundos e `-1` usa o
/// padrão de 5 s (notificações críticas ficam até serem fechadas).
fn resolve_notification_timeout(timeout: i32, urgency: u8) -> Option<u32> {
    if timeout > 0 {
        Some(timeout as u32)
    } else if timeout == 0 {
        None
    } else if urgency >= 2 {
        None
    } else {
        Some(5_000)
    }
}

fn setup_notifications(window: &Window, document: &Document) -> Result<(), JsValue> {
    let _ = window;

    if let Ok(button) = by_id(document, "dnd-btn") {
        let document_for_click = document.clone();
        let click = Closure::<dyn FnMut()>::new(move || {
            let active = STATE.with(|state| {
                let mut state = state.borrow_mut();
                state.dnd = !state.dnd;
                state.dnd
            });
            update_dnd_button(&document_for_click, active);
            send_message(&IpcMessage::DndState { active });
        });
        button.add_event_listener_with_callback("click", click.as_ref().unchecked_ref())?;
        click.forget();
    }

    if let Ok(button) = by_id(document, "notifications-btn") {
        let document_for_click = document.clone();
        let click = Closure::<dyn FnMut()>::new(move || {
            let open = STATE.with(|state| !state.borrow().notif_panel_open);
            set_notification_panel(&document_for_click, open);
        });
        button.add_event_listener_with_callback("click", click.as_ref().unchecked_ref())?;
        click.forget();
    }

    if let Ok(button) = by_id(document, "notification-close") {
        let document_for_click = document.clone();
        let click = Closure::<dyn FnMut()>::new(move || {
            set_notification_panel(&document_for_click, false);
        });
        button.add_event_listener_with_callback("click", click.as_ref().unchecked_ref())?;
        click.forget();
    }

    if let Ok(button) = by_id(document, "notification-clear") {
        let document_for_click = document.clone();
        let click = Closure::<dyn FnMut()>::new(move || {
            STATE.with(|state| state.borrow_mut().notifications.clear());
            let _ = render_notifications(&document_for_click);
        });
        button.add_event_listener_with_callback("click", click.as_ref().unchecked_ref())?;
        click.forget();
    }

    // Clique fora fecha o painel (sem interferir no pan/desenho do canvas).
    let document_for_outside = document.clone();
    let outside = Closure::<dyn FnMut(Event)>::new(move |event: Event| {
        if !STATE.with(|state| state.borrow().notif_panel_open) {
            return;
        }
        let Some(target) = event
            .target()
            .and_then(|target| target.dyn_into::<Element>().ok())
        else {
            return;
        };
        if target
            .closest("#notification-panel, #notifications-btn")
            .ok()
            .flatten()
            .is_some()
        {
            return;
        }
        set_notification_panel(&document_for_outside, false);
    });
    document.add_event_listener_with_callback("pointerdown", outside.as_ref().unchecked_ref())?;
    outside.forget();

    bind_notification_popups(document)?;
    bind_notification_history(document)?;
    start_notification_ticker();
    let _ = render_notifications(document);
    Ok(())
}

fn bind_notification_popups(document: &Document) -> Result<(), JsValue> {
    let Ok(container) = by_id(document, "notification-popups") else {
        return Ok(());
    };

    let document_for_click = document.clone();
    let click = Closure::<dyn FnMut(Event)>::new(move |event: Event| {
        let Some(target) = event
            .target()
            .and_then(|target| target.dyn_into::<Element>().ok())
        else {
            return;
        };
        let Ok(Some(card)) = target.closest("[data-notif-id]") else {
            return;
        };
        let Some(id) = card
            .get_attribute("data-notif-id")
            .and_then(|value| value.parse::<u32>().ok())
        else {
            return;
        };

        let is_close = target
            .closest("[data-notif-close]")
            .ok()
            .flatten()
            .is_some();
        let action = if is_close {
            None
        } else if let Ok(Some(button)) = target.closest("[data-notif-action]") {
            button.get_attribute("data-notif-action")
        } else {
            default_notification_action(id)
        };

        if let Some(action) = action.as_deref() {
            send_message(&IpcMessage::NotificationAction {
                id,
                action: action.to_owned(),
            });
        }
        if is_close || action.is_some() {
            remove_popup(id);
            send_message(&IpcMessage::NotificationClosed {
                id,
                reason: NOTIFICATION_CLOSED_DISMISSED,
            });
            let _ = render_notifications(&document_for_click);
        }
    });
    container.add_event_listener_with_callback("click", click.as_ref().unchecked_ref())?;
    click.forget();

    // Hover pausa o prazo; o daemon recebe o mesmo aviso para não fechar a
    // notificação por baixo do ponteiro.
    let over = Closure::<dyn FnMut(PointerEvent)>::new(move |event: PointerEvent| {
        let Some(card) = notification_card_from_event(event.as_ref()) else {
            return;
        };
        let Some(id) = card
            .get_attribute("data-notif-id")
            .and_then(|value| value.parse::<u32>().ok())
        else {
            return;
        };
        let changed = STATE.with(|state| {
            let mut state = state.borrow_mut();
            let Some(popup) = state
                .popups
                .iter_mut()
                .find(|popup| popup.item.id == id)
            else {
                return false;
            };
            if popup.paused {
                return false;
            }
            popup.paused = true;
            let now = js_sys::Date::now();
            popup.remaining = popup.deadline.map(|deadline| (deadline - now).max(0.0));
            popup.deadline = None;
            true
        });
        if changed {
            send_message(&IpcMessage::NotificationHold { id, held: true });
        }
    });
    container.add_event_listener_with_callback("pointerover", over.as_ref().unchecked_ref())?;
    over.forget();

    let out = Closure::<dyn FnMut(PointerEvent)>::new(move |event: PointerEvent| {
        let Some(card) = notification_card_from_event(event.as_ref()) else {
            return;
        };
        // Troca entre filhos do mesmo popup não retoma o prazo.
        let inside = event
            .related_target()
            .and_then(|target| target.dyn_into::<web_sys::Node>().ok())
            .is_some_and(|node| card.contains(Some(&node)));
        if inside {
            return;
        }
        let Some(id) = card
            .get_attribute("data-notif-id")
            .and_then(|value| value.parse::<u32>().ok())
        else {
            return;
        };
        let changed = STATE.with(|state| {
            let mut state = state.borrow_mut();
            let Some(popup) = state
                .popups
                .iter_mut()
                .find(|popup| popup.item.id == id)
            else {
                return false;
            };
            if !popup.paused {
                return false;
            }
            popup.paused = false;
            if let Some(remaining) = popup.remaining.take() {
                popup.deadline = Some(js_sys::Date::now() + remaining);
            }
            true
        });
        if changed {
            send_message(&IpcMessage::NotificationHold { id, held: false });
        }
    });
    container.add_event_listener_with_callback("pointerout", out.as_ref().unchecked_ref())?;
    out.forget();
    Ok(())
}

fn bind_notification_history(document: &Document) -> Result<(), JsValue> {
    let Ok(list) = by_id(document, "notification-list") else {
        return Ok(());
    };
    let click = Closure::<dyn FnMut(Event)>::new(move |event: Event| {
        let Some(target) = event
            .target()
            .and_then(|target| target.dyn_into::<Element>().ok())
        else {
            return;
        };
        let Ok(Some(card)) = target.closest("[data-notif-id]") else {
            return;
        };
        let Some(id) = card
            .get_attribute("data-notif-id")
            .and_then(|value| value.parse::<u32>().ok())
        else {
            return;
        };
        let action = if let Ok(Some(button)) = target.closest("[data-notif-action]") {
            button.get_attribute("data-notif-action")
        } else {
            default_notification_action(id)
        };
        if let Some(action) = action {
            send_message(&IpcMessage::NotificationAction { id, action });
        }
    });
    list.add_event_listener_with_callback("click", click.as_ref().unchecked_ref())?;
    click.forget();
    Ok(())
}

fn notification_card_from_event(event: &Event) -> Option<Element> {
    let target = event.target()?.dyn_into::<Element>().ok()?;
    target.closest("[data-notif-id]").ok().flatten()
}

fn default_notification_action(id: u32) -> Option<String> {
    STATE.with(|state| {
        let state = state.borrow();
        let item = state
            .popups
            .iter()
            .find(|popup| popup.item.id == id)
            .map(|popup| &popup.item)
            .or_else(|| state.notifications.iter().find(|item| item.id == id))?;
        item.actions
            .iter()
            .find(|action| action.key == "default")
            .map(|action| action.key.clone())
    })
}

fn remove_popup(id: u32) {
    STATE.with(|state| {
        state
            .borrow_mut()
            .popups
            .retain(|popup| popup.item.id != id);
    });
}

fn handle_notification(document: &Document, item: NotificationItem, timeout: i32) {
    STATE.with(|state| {
        let mut state = state.borrow_mut();
        // Substituição (replaces_id): o mesmo id troca de conteúdo.
        state.popups.retain(|popup| popup.item.id != item.id);
        state.notifications.retain(|entry| entry.id != item.id);
        state.notifications.insert(0, item.clone());
        if state.notifications.len() > HISTORY_LIMIT {
            state.notifications.truncate(HISTORY_LIMIT);
        }

        // DND silencia urgência normal/baixa; críticas sempre aparecem. Com o
        // painel aberto o popup ficaria escondido atrás dele: só o histórico.
        if (state.dnd && item.urgency < 2) || state.notif_panel_open {
            return;
        }

        let deadline = resolve_notification_timeout(timeout, item.urgency)
            .map(|ms| js_sys::Date::now() + ms as f64);
        state.popups.insert(
            0,
            PopupState {
                item,
                deadline,
                remaining: None,
                paused: false,
            },
        );
    });

    let _ = render_notifications(document);
}

fn start_notification_ticker() {
    let callback = Closure::<dyn FnMut()>::new(|| {
        let now = js_sys::Date::now();
        let expired = STATE.with(|state| {
            let mut state = state.borrow_mut();
            let expired: Vec<u32> = state
                .popups
                .iter()
                .filter(|popup| {
                    !popup.paused && popup.deadline.is_some_and(|deadline| deadline <= now)
                })
                .map(|popup| popup.item.id)
                .collect();
            if !expired.is_empty() {
                state
                    .popups
                    .retain(|popup| !expired.contains(&popup.item.id));
            }
            expired
        });
        if expired.is_empty() {
            return;
        }
        for id in expired {
            send_message(&IpcMessage::NotificationClosed {
                id,
                reason: NOTIFICATION_CLOSED_EXPIRED,
            });
        }
        if let Some(document) = web_sys::window().and_then(|window| window.document()) {
            let _ = render_notifications(&document);
        }
    });
    if let Some(window) = web_sys::window() {
        let _ = window.set_interval_with_callback_and_timeout_and_arguments_0(
            callback.as_ref().unchecked_ref(),
            250,
        );
    }
    callback.forget();
}

fn render_notifications(document: &Document) -> Result<(), JsValue> {
    render_notification_popups(document)?;
    render_notification_history(document)?;
    update_notification_badge(document);
    send_chrome_rects(document);
    Ok(())
}

fn render_notification_popups(document: &Document) -> Result<(), JsValue> {
    let html = STATE.with(|state| {
        let state = state.borrow();
        state
            .popups
            .iter()
            .map(notification_popup_html)
            .collect::<String>()
    });
    if let Ok(container) = by_id(document, "notification-popups") {
        container.set_inner_html(&html);
    }
    Ok(())
}

fn render_notification_history(document: &Document) -> Result<(), JsValue> {
    let (html, count) = STATE.with(|state| {
        let state = state.borrow();
        (
            state
                .notifications
                .iter()
                .map(notification_item_html)
                .collect::<String>(),
            state.notifications.len(),
        )
    });
    if let Ok(list) = by_id(document, "notification-list") {
        list.set_inner_html(&html);
    }
    if let Ok(empty) = by_id(document, "notification-empty") {
        if count == 0 {
            let _ = empty.remove_attribute("hidden");
        } else {
            let _ = empty.set_attribute("hidden", "");
        }
    }
    Ok(())
}

fn notification_popup_html(popup: &PopupState) -> String {
    let item = &popup.item;
    let initial = item
        .app_name
        .chars()
        .next()
        .map(|character| character.to_uppercase().collect::<String>())
        .unwrap_or_else(|| "•".to_owned());
    let actions = notification_actions_html(
        &item.actions,
        "notification-popup__actions",
        "notification-popup__action",
    );
    let body = if item.body.is_empty() {
        String::new()
    } else {
        format!(
            "<p class=\"notification-popup__body\">{}</p>",
            escape_html(&plain_text(&item.body))
        )
    };
    format!(
        "<article class=\"notification-popup\" data-notif-id=\"{id}\" data-urgency=\"{urgency}\">\
         <div class=\"notification-popup__head\">\
         <span class=\"notification-popup__badge\">{initial}</span>\
         <span class=\"notification-popup__app\">{app}</span>\
         <span class=\"notification-popup__time\">{time}</span>\
         <button class=\"notification-popup__close\" type=\"button\" data-notif-close aria-label=\"Dispensar\">×</button>\
         </div>\
         <p class=\"notification-popup__summary\">{summary}</p>\
         {body}{actions}</article>",
        id = item.id,
        urgency = item.urgency,
        initial = escape_html(&initial),
        app = escape_html(&item.app_name),
        time = notification_time(item.timestamp_ms),
        summary = escape_html(&item.summary),
        body = body,
        actions = actions,
    )
}

fn notification_item_html(item: &NotificationItem) -> String {
    let clickable = item.actions.iter().any(|action| action.key == "default");
    let actions = notification_actions_html(
        &item.actions,
        "notification-item__actions",
        "notification-item__action",
    );
    let body = if item.body.is_empty() {
        String::new()
    } else {
        format!(
            "<p class=\"notification-item__body\">{}</p>",
            escape_html(&plain_text(&item.body))
        )
    };
    format!(
        "<li class=\"notification-item\" data-notif-id=\"{id}\" data-urgency=\"{urgency}\" data-clickable=\"{clickable}\">\
         <div class=\"notification-item__head\"><b>{app}</b><time>{time}</time></div>\
         <p class=\"notification-item__summary\">{summary}</p>\
         {body}{actions}</li>",
        id = item.id,
        urgency = item.urgency,
        clickable = clickable,
        app = escape_html(&item.app_name),
        time = notification_time(item.timestamp_ms),
        summary = escape_html(&item.summary),
        body = body,
        actions = actions,
    )
}

fn notification_actions_html(
    actions: &[NotificationButton],
    container_class: &str,
    button_class: &str,
) -> String {
    if actions.is_empty() {
        return String::new();
    }
    let buttons = actions
        .iter()
        .map(|action| {
            format!(
                "<button type=\"button\" class=\"{button_class}\" data-notif-action=\"{key}\">{label}</button>",
                key = escape_html(&action.key),
                label = escape_html(&action.label),
            )
        })
        .collect::<String>();
    format!("<div class=\"{container_class}\">{buttons}</div>")
}

fn notification_time(timestamp_ms: u64) -> String {
    let date = js_sys::Date::new(&JsValue::from_f64(timestamp_ms as f64));
    format!("{:02}:{:02}", date.get_hours(), date.get_minutes())
}

/// Remove marcação básica (`<b>`, `<i>`, `<a …>`) antes de escapar o texto:
/// o corpo vem com markup opcional e não é renderizado como HTML.
fn plain_text(input: &str) -> String {
    let mut output = String::with_capacity(input.len());
    let mut in_tag = false;
    for character in input.chars() {
        match character {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => output.push(character),
            _ => {}
        }
    }
    output
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&apos;", "'")
        .replace("&amp;", "&")
}

fn update_notification_badge(document: &Document) {
    let count = STATE.with(|state| state.borrow().notifications.len());
    if let Ok(badge) = by_id(document, "notifications-count") {
        if count == 0 {
            let _ = badge.set_attribute("hidden", "");
        } else {
            let _ = badge.remove_attribute("hidden");
            let text = if count > 99 {
                "99+".to_owned()
            } else {
                count.to_string()
            };
            let _ = badge.set_inner_html(&text);
        }
    }
}

fn update_dnd_button(document: &Document, active: bool) {
    if let Ok(button) = by_id(document, "dnd-btn") {
        let _ = button.set_attribute("aria-pressed", if active { "true" } else { "false" });
        let _ = button.set_attribute(
            "title",
            if active {
                "Não perturbe ligado — críticas ainda aparecem"
            } else {
                "Não perturbe (silencia notificações normais)"
            },
        );
    }
}

fn set_notification_panel(document: &Document, open: bool) {
    STATE.with(|state| state.borrow_mut().notif_panel_open = open);
    if let Ok(panel) = by_id(document, "notification-panel") {
        if open {
            let _ = panel.remove_attribute("hidden");
        } else {
            let _ = panel.set_attribute("hidden", "");
        }
    }
    if let Ok(button) = by_id(document, "notifications-btn") {
        let _ = button.set_attribute("aria-expanded", if open { "true" } else { "false" });
    }
    if open {
        let _ = render_notification_history(document);
    }
    send_chrome_rects(document);
}

pub(crate) fn show_toast(document: &Document, message: &str) {
    let Ok(toast) = by_id(document, "toast") else {
        return;
    };
    let generation = TOAST_GENERATION.with(|cell| {
        let next = cell.get().wrapping_add(1);
        cell.set(next);
        next
    });
    let _ = toast.set_inner_html(&escape_html(message));
    let _ = toast.class_list().add_1("show");

    let Some(window) = web_sys::window() else {
        return;
    };
    let callback = Closure::once(move || {
        if TOAST_GENERATION.with(|cell| cell.get()) == generation {
            if let Some(window) = web_sys::window() {
                if let Some(document) = window.document() {
                    if let Ok(toast) = by_id(&document, "toast") {
                        let _ = toast.class_list().remove_1("show");
                    }
                }
            }
        }
    });
    let _ = window.set_timeout_with_callback_and_timeout_and_arguments_0(
        callback.as_ref().unchecked_ref(),
        2400,
    );
    callback.forget();
}

fn set_status(text: &str) {
    let Some(window) = web_sys::window() else {
        return;
    };
    let Some(document) = window.document() else {
        return;
    };
    let _ = set_text(&document, "status", text);
}
