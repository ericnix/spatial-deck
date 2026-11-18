//! Spatial Deck — compositor Wayland nativo.
//!
//! Arquitetura:
//! - Smithay + winit: backend aninhado (X11/Wayland) com renderer OpenGL ES.
//! - `Space<Window>`: mantém as superfícies `xdg_toplevel` e aplica a
//!   transformação do canvas (`screen = world * scale + offset`).
//! - Tokio: servidor IPC no socket Unix `/tmp/spatial-deck.sock` e ponte
//!   WebSocket em `ws://127.0.0.1:7337` para o frontend WASM.
//! - Tokio: servidor HTTP estático em `http://127.0.0.1:7338` para abrir a
//!   interface do frontend sem depender de outro servidor.
//! - Zoom real: `canvas_render.rs` escala as superfícies no renderizador e a
//!   visão geral (`Overview`) desenha miniaturas ao vivo numa grade de tela.
//!
//! O frontend WASM nunca consegue abrir um socket Unix diretamente no
//! navegador, por isso o WebSocket existe como ponte para o mesmo contrato
//! `IpcMessage`. O socket Unix continua disponível para clientes nativos.

use std::{
    collections::{HashMap, HashSet},
    env, fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{mpsc, Arc, atomic::AtomicU64},
    time::{Duration, Instant},
};

mod canvas_render;
mod minimalist_render;

use anyhow::{Context, Result};
use canvas_render::{scaled_window_elements, ScaledSurfaceElement};
use common::{
    CanvasTheme, CanvasZoom, ChromeRect, ClusterPlacement, DesktopApp, IpcMessage,
    OVERVIEW_BOTTOM, OVERVIEW_GAP, OVERVIEW_MARGIN_X, OVERVIEW_TOP,
};
use freedesktop_entry_parser::parse_entry;
use futures_util::{SinkExt, StreamExt};
use smithay::{
    backend::{
        input::{
            AbsolutePositionEvent, Axis, ButtonState, InputEvent, KeyState, KeyboardKeyEvent,
            PointerAxisEvent, PointerButtonEvent,
        },
        renderer::{
            element::{
                surface::{render_elements_from_surface_tree, WaylandSurfaceRenderElement},
                utils::CropRenderElement,
                AsRenderElements, Kind,
            },
            gles::GlesRenderer,
            utils::{draw_render_elements, on_commit_buffer_handler},
            Color32F, Frame, Renderer,
        },
        winit::{self, WinitEvent},
    },
    delegate_compositor, delegate_data_device, delegate_foreign_toplevel_list, delegate_layer_shell,
    delegate_output, delegate_seat, delegate_shm, delegate_xdg_activation, delegate_xdg_shell,
    desktop::{
        find_popup_root_surface, layer_map_for_output, space::Space, LayerSurface as DesktopLayerSurface,
        PopupKind, PopupManager, Window, WindowSurfaceType,
    },
    input::{
        keyboard::{FilterResult, KeyboardHandle, Keysym},
        pointer::{AxisFrame, ButtonEvent, CursorImageStatus, MotionEvent, PointerHandle},
        Seat, SeatHandler, SeatState,
    },
    output::{Mode, Output, PhysicalProperties, Scale, Subpixel},
    reexports::{
        wayland_protocols::xdg::shell::server::xdg_toplevel,
        wayland_server::{
            backend::{ClientData, ClientId, DisconnectReason},
            protocol::{
                wl_buffer,
                wl_seat,
                wl_surface::WlSurface,
            },
            Client, Display, DisplayHandle, ListeningSocket,
        },
        winit::{
            dpi::LogicalSize, platform::pump_events::PumpStatus, window::Window as WinitWindow,
        },
    },
    utils::{Point, Rectangle, Size, Transform, SERIAL_COUNTER},
    wayland::{
        buffer::BufferHandler,
        compositor::{with_states, CompositorClientState, CompositorHandler, CompositorState},
        output::OutputManagerState,
        seat::WaylandFocus,
        selection::{
            data_device::{
                ClientDndGrabHandler, DataDeviceHandler, DataDeviceState, ServerDndGrabHandler,
            },
            SelectionHandler,
        },
        foreign_toplevel_list::{
            ForeignToplevelHandle, ForeignToplevelListHandler, ForeignToplevelListState,
        },
        shell::{
            wlr_layer::{
                KeyboardInteractivity, Layer as WlrLayer, LayerSurfaceData, WlrLayerShellHandler,
                WlrLayerShellState,
            },
            xdg::{
                decoration::{XdgDecorationHandler, XdgDecorationState},
                PopupSurface, PositionerState, ToplevelSurface, XdgShellHandler, XdgShellState,
                XdgToplevelSurfaceData,
            },
        },
        shm::{ShmHandler, ShmState},
        xdg_activation::{
            XdgActivationHandler, XdgActivationState, XdgActivationToken, XdgActivationTokenData,
        },
        xwayland_shell::{XWaylandShellHandler, XWaylandShellState},
    },
    xwayland::{
        xwm::{Reorder, ResizeEdge as X11ResizeEdge, WmWindowProperty, XwmId},
        X11Surface, X11Wm, XWayland, XWaylandClientData, XWaylandEvent, XwmHandler,
    },
};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    net::{TcpListener, TcpStream, UnixListener},
    sync::broadcast,
};
use tokio_tungstenite::{accept_async, tungstenite::Message};
use tracing::{debug, info, warn};

const IPC_SOCKET: &str = "/tmp/spatial-deck.sock";
const WS_ADDR: &str = "127.0.0.1:7337";
const HTTP_ADDR: &str = "127.0.0.1:7338";
const WAYLAND_SOCKET_NAME: &str = "spatial-deck";
const DEFAULT_WINDOW_SIZE: (i32, i32) = (900, 600);
/// Tamanho inicial usado para centralizar diálogos antes do primeiro commit.
const DIALOG_WINDOW_SIZE: (i32, i32) = (520, 360);
/// Tamanho mínimo aceito no redimensionamento interativo.
const MIN_WINDOW_SIZE: (i32, i32) = (240, 160);
/// Faixa (em pixels de tela) junto às bordas que inicia o redimensionamento.
const RESIZE_MARGIN: i32 = 8;
/// Valor contínuo de rolagem equivalente a um entalhe de roda (`v120 = 120`).
/// O winit no X11 só informa o passo discreto, então convertemos para o
/// `value` obrigatório do protocolo wl_pointer.
const WHEEL_STEP: f64 = 10.0;
/// Ids de diálogos ficam acima dos ids dos cartões (1..4) para não
/// consumirem a sequência usada pelo frontend.
const DIALOG_ID_BASE: u32 = 1000;
/// Ids provisórios usados até o primeiro buffer, quando já é possível
/// distinguir uma janela normal de um diálogo transiente.
const TEMP_ID_BASE: u32 = 1_000_000;
/// Camadas de empilhamento (`Space` ordena do menor para o maior).
const Z_SHELL: u8 = 0;
const Z_WINDOW: u8 = 10;
const Z_MAXIMIZED: u8 = 210;
const Z_DIALOG: u8 = 220;
/// `app_id` do cliente GTK/WebKit que hospeda a interface do desktop.
const SHELL_APP_ID: &str = "spatial-deck-shell";

/// Item do barramento IPC: `(origem, mensagem)`. A origem é o id da conexão
/// que produziu a mensagem; `IPC_SOURCE_COMPOSITOR` marca as do compositor.
/// Cada conexão ignora as próprias mensagens para não receber eco.
type Broadcast = (u64, IpcMessage);
const IPC_SOURCE_COMPOSITOR: u64 = 0;
/// Ids de conexão IPC (Unix e WebSocket) começam em 1.
static NEXT_IPC_CLIENT: AtomicU64 = AtomicU64::new(1);

fn next_ipc_client_id() -> u64 {
    NEXT_IPC_CLIENT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
}

fn main() -> Result<()> {
    if let Ok(filter) = tracing_subscriber::EnvFilter::try_from_default_env() {
        tracing_subscriber::fmt().with_env_filter(filter).init();
    } else {
        tracing_subscriber::fmt().init();
    }

    let apps = Arc::new(scan_desktop_apps());
    info!(count = apps.len(), "aplicativos .desktop encontrados");

    let (tx, rx) = mpsc::channel::<IpcMessage>();
    let broadcast = start_ipc_runtime(tx, apps.clone())?;

    eprintln!(
        "Spatial Deck\n  Wayland:   WAYLAND_DISPLAY={WAYLAND_SOCKET_NAME}\n  IPC Unix:  {IPC_SOCKET}\n  WebSocket: ws://{WS_ADDR}\n  UI:        http://{HTTP_ADDR}/"
    );

    run_compositor(rx, broadcast)
}

// ---------------------------------------------------------------------------
// Compositor Smithay
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy)]
struct CanvasTransform {
    scale: f64,
    offset_x: f64,
    offset_y: f64,
}

impl Default for CanvasTransform {
    fn default() -> Self {
        Self {
            scale: 1.0,
            offset_x: 0.0,
            offset_y: 0.0,
        }
    }
}

struct ManagedWindow {
    /// Id numérico exposto no IPC (`ClusterPlacement.id` e `AutoMaximize`).
    id: u32,
    window: Window,
    /// Posição no espaço de mundo, antes da transformação do canvas.
    world: Point<f64, smithay::utils::Logical>,
    /// Último tamanho conhecido do cliente, em pixels lógicos de tela.
    size: Size<i32, smithay::utils::Logical>,
    /// Quando maximizada, a janela fica ancorada em (0,0) e ignora o pan/zoom.
    maximized: bool,
    /// Quando em tela cheia, a janela cobre a saída e fica acima de tudo.
    fullscreen: bool,
    /// Geometria anterior, restaurada ao sair de maximizado/tela cheia.
    restore: Option<WindowRestore>,
    /// Diálogo transiente (tem `parent` xdg_toplevel).
    dialog: bool,
    /// Id do pai, quando for diálogo.
    parent_id: Option<u32>,
    /// Centraliza o diálogo no pai assim que o primeiro buffer chega.
    dialog_needs_center: bool,
    /// `true` enquanto o id é provisório (primeiro buffer ainda não chegou).
    pending_id: bool,
    /// `true` depois que o usuário redimensiona a janela manualmente. Nesse
    /// caso o limite de tamanho inicial deixa de ser aplicado.
    allow_oversize: bool,
}

#[derive(Debug, Clone, Copy)]
struct WindowRestore {
    world: Point<f64, smithay::utils::Logical>,
    size: Size<i32, smithay::utils::Logical>,
}

/// Arrasto interativo em andamento (move_request do cliente ou Super/Alt+arrasto).
struct MoveState {
    window_id: u32,
    /// Deslocamento entre o ponteiro e o canto superior esquerdo da janela,
    /// em coordenadas de mundo.
    grab_x: f64,
    grab_y: f64,
    /// `true` quando o cliente já recebeu o botão pressionado (move_request)
    /// e ainda precisa receber o botão solto.
    forward_buttons: bool,
}

/// Bordas ativas em um redimensionamento interativo.
#[derive(Debug, Clone, Copy, Default)]
struct ResizeEdges {
    top: bool,
    bottom: bool,
    left: bool,
    right: bool,
}

impl ResizeEdges {
    fn from_xdg(edge: xdg_toplevel::ResizeEdge) -> Self {
        use xdg_toplevel::ResizeEdge::*;
        match edge {
            None => Self::default(),
            Top => Self { top: true, ..Self::default() },
            Bottom => Self { bottom: true, ..Self::default() },
            Left => Self { left: true, ..Self::default() },
            Right => Self { right: true, ..Self::default() },
            TopLeft => Self { top: true, left: true, ..Self::default() },
            BottomLeft => Self { bottom: true, left: true, ..Self::default() },
            TopRight => Self { top: true, right: true, ..Self::default() },
            BottomRight => Self { bottom: true, right: true, ..Self::default() },
            _ => Self::default(),
        }
    }

    fn from_x11(edge: X11ResizeEdge) -> Self {
        use X11ResizeEdge::*;
        match edge {
            Top => Self { top: true, ..Self::default() },
            Bottom => Self { bottom: true, ..Self::default() },
            Left => Self { left: true, ..Self::default() },
            Right => Self { right: true, ..Self::default() },
            TopLeft => Self { top: true, left: true, ..Self::default() },
            BottomLeft => Self { bottom: true, left: true, ..Self::default() },
            TopRight => Self { top: true, right: true, ..Self::default() },
            BottomRight => Self { bottom: true, right: true, ..Self::default() },
        }
    }

    fn any(self) -> bool {
        self.top || self.bottom || self.left || self.right
    }
}

/// Escolhe o canto de redimensionamento mais próximo do clique: quadrante
/// esquerdo/direito e superior/inferior.
fn resize_edges_from_point(
    rect: Rectangle<i32, smithay::utils::Logical>,
    point: Point<f64, smithay::utils::Logical>,
) -> ResizeEdges {
    let center_x = rect.loc.x as f64 + rect.size.w as f64 / 2.0;
    let center_y = rect.loc.y as f64 + rect.size.h as f64 / 2.0;
    ResizeEdges {
        left: point.x < center_x,
        right: point.x >= center_x,
        top: point.y < center_y,
        bottom: point.y >= center_y,
    }
}

/// Calcula a nova posição de mundo e o novo tamanho a partir do arrasto de
/// uma borda. `dx`/`dy` são o deslocamento do ponteiro em pixels de tela.
fn resize_geometry(
    edges: ResizeEdges,
    start_world: Point<f64, smithay::utils::Logical>,
    start_size: Size<i32, smithay::utils::Logical>,
    dx: f64,
    dy: f64,
    scale: f64,
) -> (Point<f64, smithay::utils::Logical>, Size<i32, smithay::utils::Logical>) {
    let scale = scale.max(0.01);
    let mut world = start_world;
    let mut size = start_size;
    if edges.left {
        world.x = start_world.x + dx / scale;
        size.w = start_size.w - dx as i32;
    }
    if edges.right {
        size.w = start_size.w + dx as i32;
    }
    if edges.top {
        world.y = start_world.y + dy / scale;
        size.h = start_size.h - dy as i32;
    }
    if edges.bottom {
        size.h = start_size.h + dy as i32;
    }

    size.w = size.w.max(MIN_WINDOW_SIZE.0);
    size.h = size.h.max(MIN_WINDOW_SIZE.1);

    // Se a borda esquerda/superior bateu no tamanho mínimo, mantém a borda
    // oposta fixa em vez de encolher a janela além do limite.
    if edges.left {
        world.x = start_world.x + (start_size.w - size.w) as f64 / scale;
    }
    if edges.top {
        world.y = start_world.y + (start_size.h - size.h) as f64 / scale;
    }

    (world, size)
}

/// Redimensionamento interativo em andamento.
struct ResizeState {
    window_id: u32,
    edges: ResizeEdges,
    /// Posição do ponteiro no início, em pixels de tela.
    start_pointer_screen: Point<f64, smithay::utils::Logical>,
    /// Posição da janela no início, em coordenadas de mundo.
    start_world: Point<f64, smithay::utils::Logical>,
    /// Tamanho da janela no início, em pixels lógicos.
    start_size: Size<i32, smithay::utils::Logical>,
    /// `true` quando o cliente iniciou o resize (resize_request) e ainda
    /// precisa receber o botão solto.
    forward_buttons: bool,
}

/// Calcula a grade de miniaturas da visão geral. Cada janela vira um
/// retângulo ajustado à sua proporção dentro de uma célula, centralizado.
fn overview_cells(
    viewport: Size<i32, smithay::utils::Logical>,
    windows: &[(u32, Size<i32, smithay::utils::Logical>)],
) -> Vec<(u32, Rectangle<i32, smithay::utils::Logical>)> {
    if windows.is_empty() {
        return Vec::new();
    }

    let area: Rectangle<i32, smithay::utils::Logical> = Rectangle::new(
        Point::from((OVERVIEW_MARGIN_X, OVERVIEW_TOP)),
        Size::from((
            (viewport.w - OVERVIEW_MARGIN_X * 2).max(1),
            (viewport.h - OVERVIEW_TOP - OVERVIEW_BOTTOM).max(1),
        )),
    );

    let count = windows.len() as i32;
    let columns = (((count as f64) * (area.size.w as f64) / (area.size.h as f64)).sqrt().ceil()
        as i32)
        .clamp(1, count);
    let rows = (count + columns - 1) / columns;
    let cell_w = ((area.size.w - OVERVIEW_GAP * (columns - 1)) / columns).max(1);
    let cell_h = ((area.size.h - OVERVIEW_GAP * (rows - 1)) / rows).max(1);

    windows
        .iter()
        .enumerate()
        .map(|(index, (id, size))| {
            let column = index as i32 % columns;
            let row = index as i32 / columns;
            let cell = Rectangle::new(
                Point::from((
                    area.loc.x + column * (cell_w + OVERVIEW_GAP),
                    area.loc.y + row * (cell_h + OVERVIEW_GAP),
                )),
                Size::from((cell_w, cell_h)),
            );
            (*id, fit_thumbnail(*size, cell))
        })
        .collect()
}

/// Encaixa uma janela na célula preservando a proporção original.
fn fit_thumbnail(
    size: Size<i32, smithay::utils::Logical>,
    cell: Rectangle<i32, smithay::utils::Logical>,
) -> Rectangle<i32, smithay::utils::Logical> {
    let width = size.w.max(1) as f64;
    let height = size.h.max(1) as f64;
    let scale = (cell.size.w as f64 / width).min(cell.size.h as f64 / height);
    let thumb_w = (width * scale).round().max(1.0) as i32;
    let thumb_h = (height * scale).round().max(1.0) as i32;
    Rectangle::new(
        Point::from((
            cell.loc.x + (cell.size.w - thumb_w) / 2,
            cell.loc.y + (cell.size.h - thumb_h) / 2,
        )),
        Size::from((thumb_w, thumb_h)),
    )
}

/// Modo de visão geral: todas as janelas viram miniaturas ao vivo numa grade.
struct Overview {
    /// Id da janela e o retângulo da miniatura em coordenadas de tela.
    cells: Vec<(u32, Rectangle<i32, smithay::utils::Logical>)>,
}

struct App {
    compositor_state: CompositorState,
    xdg_shell_state: XdgShellState,
    #[allow(dead_code)]
    xdg_decoration_state: XdgDecorationState,
    xwayland_shell_state: XWaylandShellState,
    /// Superfícies de shell externas (barras, OSD, overlays) ancoradas nas bordas.
    layer_shell_state: WlrLayerShellState,
    /// Janelas expostas a ferramentas externas (barra, atalhos, taskbars).
    foreign_toplevel_state: ForeignToplevelListState,
    /// Tokens xdg-activation (foco correto ao lançar/ativar aplicativos).
    activation_state: XdgActivationState,
    /// Handle de foreign-toplevel de cada janela interna, por id.
    foreign_toplevels: HashMap<u32, ForeignToplevelHandle>,
    /// Layer surface que detém o foco de teclado (exclusive/on-demand).
    layer_focus: Option<DesktopLayerSurface>,
    xwm: Option<X11Wm>,
    xdisplay: Option<u32>,
    shm_state: ShmState,
    seat_state: SeatState<Self>,
    data_device_state: DataDeviceState,
    _output_manager_state: OutputManagerState,
    _seat: Seat<Self>,
    keyboard: KeyboardHandle<Self>,
    pointer: PointerHandle<Self>,

    output: Output,
    output_physical_size: Size<i32, smithay::utils::Physical>,
    output_logical_size: Size<i32, smithay::utils::Logical>,
    space: Space<Window>,
    /// Popups xdg (menus, tooltips, comboboxes) das janelas.
    popup_manager: PopupManager,
    /// Janela do shell (interface do desktop), sempre em (0,0), sem pan/zoom.
    shell_window: Option<ManagedWindow>,
    windows: Vec<ManagedWindow>,
    next_window_id: u32,
    next_dialog_id: u32,
    next_temp_id: u32,
    /// Janela interna que recebeu o foco de teclado por último.
    focused_window: Option<u32>,
    /// Ordem de foco, da mais antiga para a mais recente (Alt+Tab).
    focus_order: Vec<u32>,
    /// Pilha do canvas, do fundo para o topo (ids de janelas comuns).
    stack: Vec<u32>,
    move_state: Option<MoveState>,
    resize_state: Option<ResizeState>,
    /// Coalescência de ClusterUpdate durante arrastos para não atrasar o shell.
    last_cluster_at: Option<Instant>,
    pending_cluster: bool,
    /// Áreas do chrome do shell (HUD, deck, gaveta) que ficam acima das janelas.
    chrome_rects: Vec<Rectangle<i32, smithay::utils::Logical>>,
    /// Ferramenta de redimensionamento ativa no deck.
    resize_tool: bool,
    /// Linha inferior da barra superior: as janelas reais são cortadas aqui.
    hud_bottom: Option<i32>,
    /// Última posição absoluta do ponteiro no output. `PointerHandle` pode
    /// guardar uma coordenada sintetizada para o cliente durante o zoom.
    pointer_screen: Point<f64, smithay::utils::Logical>,
    /// Visão geral ativa (miniaturas em grade).
    overview: Option<Overview>,

    canvas: CanvasTransform,
    theme: CanvasTheme,
    ipc: mpsc::Receiver<IpcMessage>,
    /// Envia atualizações de posição para os clientes WebSocket conectados.
    broadcast: broadcast::Sender<Broadcast>,
}

impl App {
    fn new(
        display: &smithay::reexports::wayland_server::DisplayHandle,
        ipc: mpsc::Receiver<IpcMessage>,
        broadcast: broadcast::Sender<Broadcast>,
    ) -> Result<Self> {
        let compositor_state = CompositorState::new::<Self>(display);
        let xdg_shell_state = XdgShellState::new::<Self>(display);
        let xdg_decoration_state = XdgDecorationState::new::<Self>(display);
        let xwayland_shell_state = XWaylandShellState::new::<Self>(display);
        let layer_shell_state = WlrLayerShellState::new::<Self>(display);
        let foreign_toplevel_state = ForeignToplevelListState::new::<Self>(display);
        let activation_state = XdgActivationState::new::<Self>(display);
        let shm_state = ShmState::new::<Self>(display, vec![]);
        let data_device_state = DataDeviceState::new::<Self>(display);
        let output_manager_state = OutputManagerState::new_with_xdg_output::<Self>(display);

        let mut seat_state = SeatState::new();
        let mut seat = seat_state.new_wl_seat(display, "spatial-deck");
        let keyboard = seat
            .add_keyboard(Default::default(), 200, 25)
            .context("falha ao criar teclado xkb")?;
        let pointer = seat.add_pointer();

        let output_physical_size = Size::from((1280, 720));
        let output_logical_size = Size::from((1280, 720));
        let output = Output::new(
            "spatial-deck".into(),
            PhysicalProperties {
                size: (338, 190).into(),
                subpixel: Subpixel::Unknown,
                make: "Spatial Deck".into(),
                model: "Nested Output".into(),
            },
        );
        output.change_current_state(
            Some(Mode {
                size: output_physical_size,
                refresh: 60_000,
            }),
            Some(Transform::Normal),
            Some(Scale::Integer(1)),
            Some(Point::from((0, 0))),
        );
        output.set_preferred(Mode {
            size: output_physical_size,
            refresh: 60_000,
        });
        output.create_global::<Self>(display);

        let mut space = Space::default();
        space.map_output(&output, (0, 0));

        Ok(Self {
            compositor_state,
            xdg_shell_state,
            xdg_decoration_state,
            xwayland_shell_state,
            layer_shell_state,
            foreign_toplevel_state,
            activation_state,
            foreign_toplevels: HashMap::new(),
            layer_focus: None,
            xwm: None,
            xdisplay: None,
            shm_state,
            seat_state,
            data_device_state,
            _output_manager_state: output_manager_state,
            _seat: seat,
            keyboard,
            pointer,
            output,
            output_physical_size,
            output_logical_size,
            space,
            popup_manager: PopupManager::default(),
            shell_window: None,
            windows: Vec::new(),
            next_window_id: 1,
            next_dialog_id: DIALOG_ID_BASE,
            next_temp_id: TEMP_ID_BASE,
            focused_window: None,
            focus_order: Vec::new(),
            stack: Vec::new(),
            move_state: None,
            resize_state: None,
            last_cluster_at: None,
            pending_cluster: false,
            chrome_rects: Vec::new(),
            resize_tool: false,
            hud_bottom: None,
            pointer_screen: Point::from((0.0, 0.0)),
            overview: None,
            canvas: CanvasTransform::default(),
            theme: CanvasTheme::default(),
            ipc,
            broadcast,
        })
    }

    fn to_screen(&self, world: Point<f64, smithay::utils::Logical>) -> Point<i32, smithay::utils::Logical> {
        canvas_screen(self.canvas, world)
    }

    fn to_world(&self, screen: Point<f64, smithay::utils::Logical>) -> Point<f64, smithay::utils::Logical> {
        let scale = self.canvas.scale.max(0.01);
        Point::from((
            (screen.x - self.canvas.offset_x) / scale,
            (screen.y - self.canvas.offset_y) / scale,
        ))
    }

    /// Área de teste de uma janela no espaço de mundo. Usa a bbox da árvore
    /// (que inclui CSD/subsurfaces) e cai para o tamanho do conteúdo quando
    /// o cliente ainda não commitou nenhuma superfície.
    fn window_hit_rect(
        world: Point<f64, smithay::utils::Logical>,
        fallback: Size<i32, smithay::utils::Logical>,
        bbox: Rectangle<i32, smithay::utils::Logical>,
    ) -> Rectangle<f64, smithay::utils::Logical> {
        if bbox.size.w > 0 && bbox.size.h > 0 {
            Rectangle::new(world + bbox.loc.to_f64(), bbox.size.to_f64())
        } else {
            Rectangle::new(world, fallback.to_f64())
        }
    }

    /// O HUD recorta o desenho das janelas; os cliques têm de ser recusados
    /// na mesma faixa, senão uma área invisível continua capturando mouse.
    fn hidden_under_hud(
        location: Point<f64, smithay::utils::Logical>,
        hud_bottom: Option<i32>,
    ) -> bool {
        hud_bottom.is_some_and(|line| location.y < line as f64)
    }

    /// Janela de aplicativo sob o ponteiro (o shell é ignorado).
    fn window_id_under(&self, location: Point<f64, smithay::utils::Logical>) -> Option<u32> {
        self.window_under(location).map(|(id, _, _, _)| id)
    }

    /// Retângulo que a janela ocupa na tela, já considerando o zoom do canvas
    /// e a grade da visão geral.
    fn window_screen_rect(
        &self,
        managed: &ManagedWindow,
    ) -> Rectangle<i32, smithay::utils::Logical> {
        if let Some(overview) = &self.overview {
            if let Some((_, rect)) = overview
                .cells
                .iter()
                .find(|(id, _)| *id == managed.id)
            {
                return *rect;
            }
        }

        if managed.maximized || managed.fullscreen {
            return Rectangle::new(Point::from((0, 0)), self.output_logical_size);
        }

        let size = if managed.size.w > 0 && managed.size.h > 0 {
            managed.size
        } else {
            managed.window.geometry().size
        };
        let scale = self.canvas.scale.max(0.01);
        Rectangle::new(
            self.to_screen(managed.world),
            Size::from((
                ((size.w as f64) * scale).round().max(1.0) as i32,
                ((size.h as f64) * scale).round().max(1.0) as i32,
            )),
        )
    }

    /// `true` para as faixas do HUD (topo) e do deck (base), que continuam
    /// interativas durante a visão geral. Clicar no canvas entre elas sai do
    /// modo.
    /// `true` quando o ponto cai numa área do chrome do shell (HUD, deck, gaveta).
    fn in_chrome(&self, location: Point<f64, smithay::utils::Logical>) -> bool {
        self.chrome_rects
            .iter()
            .any(|rect| rect.to_f64().contains(location))
    }

    fn is_shell_chrome(&self, location: Point<f64, smithay::utils::Logical>) -> bool {
        let height = self.output_logical_size.h as f64;
        location.y < OVERVIEW_TOP as f64 || location.y >= height - OVERVIEW_BOTTOM as f64
    }

    /// Posição de tela e fator de escala usados para renderizar a janela.
    fn window_render_transform(
        &self,
        managed: &ManagedWindow,
    ) -> (Point<i32, smithay::utils::Logical>, f64) {
        if let Some(overview) = &self.overview {
            if let Some((_, rect)) = overview
                .cells
                .iter()
                .find(|(id, _)| *id == managed.id)
            {
                let size = if managed.size.w > 0 && managed.size.h > 0 {
                    managed.size
                } else {
                    managed.window.geometry().size
                };
                let zoom = if size.w > 0 && size.h > 0 {
                    (rect.size.w as f64 / size.w as f64)
                        .min(rect.size.h as f64 / size.h as f64)
                } else {
                    1.0
                };
                return (rect.loc, zoom.clamp(0.01, 16.0));
            }
        }

        if managed.maximized || managed.fullscreen {
            return (Point::from((0, 0)), 1.0);
        }
        (self.to_screen(managed.world), self.canvas.scale.max(0.01))
    }

    /// Janela sob o ponteiro, com a superfície real (popup ou toplevel), o
    /// ponto local em coordenadas do cliente e o deslocamento da superfície
    /// dentro da janela. Faz o hit-test no espaço de mundo (ou na grade do
    /// overview), não na geometria não escalada do `Space`.
    fn window_under(
        &self,
        location: Point<f64, smithay::utils::Logical>,
    ) -> Option<(
        u32,
        WlSurface,
        Point<f64, smithay::utils::Logical>,
        Point<i32, smithay::utils::Logical>,
    )> {
        if self.overview.is_some() {
            let managed = self
                .windows
                .iter()
                .find(|managed| {
                    !managed.pending_id
                        && self.window_screen_rect(managed).to_f64().contains(location)
                })?;
            let rect = self.window_screen_rect(managed).to_f64();
            let (_, zoom) = self.window_render_transform(managed);
            let local = Point::from((
                (location.x - rect.loc.x) / zoom,
                (location.y - rect.loc.y) / zoom,
            ));
            let surface = managed.window.wl_surface()?.into_owned();
            return Some((managed.id, surface, local, Point::from((0, 0))));
        }

        // O chrome do shell fica acima das janelas: o clique ali é do shell.
        if self.in_chrome(location) || Self::hidden_under_hud(location, self.hud_bottom) {
            return None;
        }

        let world = self.to_world(location);
        for window in self.space.elements_for_output(&self.output).rev() {
            if self
                .shell_window
                .as_ref()
                .map(|shell| &shell.window == window)
                .unwrap_or(false)
            {
                continue;
            }
            let Some(managed) = self.windows.iter().find(|managed| &managed.window == window)
            else {
                continue;
            };

            // (área de teste, ponto, origem da superfície). A área é a árvore
            // inteira da superfície: a barra de título CSD do cliente fica
            // acima/à esquerda da origem de mundo, fora do retângulo do
            // conteúdo, e antes não recebia cliques.
            let (rect, point, origin) = if managed.maximized || managed.fullscreen {
                (
                    Rectangle::new(
                        Point::from((0.0, 0.0)),
                        self.output_logical_size.to_f64(),
                    ),
                    location,
                    Point::from((0.0, 0.0)),
                )
            } else {
                let fallback = if managed.size.w > 0 && managed.size.h > 0 {
                    managed.size
                } else {
                    managed.window.geometry().size
                };
                let rect = Self::window_hit_rect(
                    managed.world,
                    fallback,
                    managed.window.bbox(),
                );
                (rect, world, managed.world)
            };

            if !rect.contains(point) {
                continue;
            }
            let local = Point::from((point.x - origin.x, point.y - origin.y));
            if let Some((surface, surface_location)) =
                managed.window.surface_under(local, WindowSurfaceType::ALL)
            {
                return Some((managed.id, surface, local, surface_location));
            }
        }

        None
    }

    /// Foco de ponteiro para o shell (HUD, deck e gaveta), que nunca escala.
    fn shell_pointer_focus(
        &self,
        location: Point<f64, smithay::utils::Logical>,
    ) -> Option<(
        WlSurface,
        Point<f64, smithay::utils::Logical>,
        Point<f64, smithay::utils::Logical>,
    )> {
        let shell = self.shell_window.as_ref()?;
        let window_location = self
            .space
            .element_location(&shell.window)
            .unwrap_or_else(|| Point::from((0, 0)));
        let window_point = location - window_location.to_f64();
        let (surface, surface_location) =
            shell
                .window
                .surface_under(window_point, WindowSurfaceType::ALL)?;
        let origin = Point::from((
            window_location.x as f64 + surface_location.x as f64,
            window_location.y as f64 + surface_location.y as f64,
        ));
        Some((surface, origin, location))
    }

    /// Resolve o foco de ponteiro e a posição lógica a enviar ao cliente.
    /// Para janelas com zoom, a posição é sintetizada para que o cliente
    /// receba o deslocamento em coordenadas de mundo (sem a escala).
    fn pointer_focus(
        &self,
        location: Point<f64, smithay::utils::Logical>,
    ) -> Option<(
        WlSurface,
        Point<f64, smithay::utils::Logical>,
        Point<f64, smithay::utils::Logical>,
    )> {
        // Barras e overlays de layer ficam acima de tudo: o ponteiro vai
        // para elas antes de qualquer janela do canvas.
        if let Some(layer) = self.layer_pointer_focus(location) {
            return Some(layer);
        }

        if self.overview.is_some() {
            if self.window_under(location).is_some() {
                return None;
            }
            return self.shell_pointer_focus(location);
        }

        if let Some((id, surface, local, surface_location)) = self.window_under(location) {
            let managed = self.windows.iter().find(|managed| managed.id == id)?;
            let origin = if managed.maximized || managed.fullscreen {
                Point::from((0.0, 0.0))
            } else {
                self.to_screen(managed.world).to_f64()
            };
            let scale = if managed.maximized || managed.fullscreen {
                1.0
            } else {
                self.canvas.scale.max(0.01)
            };
            let surface_origin = Point::from((
                origin.x + surface_location.x as f64 * scale,
                origin.y + surface_location.y as f64 * scale,
            ));
            let client_local: Point<f64, smithay::utils::Logical> = Point::from((
                local.x - surface_location.x as f64,
                local.y - surface_location.y as f64,
            ));
            let motion = Point::from((
                surface_origin.x + client_local.x,
                surface_origin.y + client_local.y,
            ));
            return Some((surface, surface_origin, motion));
        }

        self.shell_pointer_focus(location)
    }

    fn window_id_for_wl_surface(&self, surface: &WlSurface) -> Option<u32> {
        self.windows
            .iter()
            .find(|managed| managed.window.wl_surface().as_deref() == Some(surface))
            .map(|managed| managed.id)
    }

    /// Posição de mundo que centraliza um diálogo sobre a janela pai.
    fn dialog_world(
        &self,
        parent_id: u32,
        dialog_size: Size<i32, smithay::utils::Logical>,
    ) -> Option<Point<f64, smithay::utils::Logical>> {
        let parent = self.windows.iter().find(|managed| managed.id == parent_id)?;
        // Maximizado/tela cheia ficam ancorados em (0,0), independentemente
        // da posição de mundo original.
        let parent_screen = if parent.maximized || parent.fullscreen {
            Point::from((0, 0))
        } else {
            self.to_screen(parent.world)
        };
        let screen = Point::from((
            parent_screen.x + (parent.size.w - dialog_size.w) / 2,
            parent_screen.y + (parent.size.h - dialog_size.h) / 2,
        ));
        Some(self.to_world(screen.to_f64()))
    }

    /// Limita janelas comuns ao espaço visível. Sem isso, um cliente que
    /// reabre no tamanho maximizado da sessão anterior cobre o deck inteiro
    /// (HUD, ferramentas e cartões). Diálogos não passam por aqui.
    fn clamp_window_size(
        &self,
        size: Size<i32, smithay::utils::Logical>,
    ) -> Size<i32, smithay::utils::Logical> {
        let viewport = self.output_logical_size;
        let max_w = ((viewport.w as f64) * 0.94) as i32;
        let max_h = ((viewport.h as f64) * 0.88) as i32;
        Size::from((
            size.w.min(max_w).max(320),
            size.h.min(max_h).max(240),
        ))
    }

    /// Centraliza uma janela nova na área visível do canvas, com um pequeno
    /// deslocamento em cascata para não empilhar várias janelas no mesmo
    /// ponto. É o que garante que um aplicativo aberto pelo deck apareça
    /// imediatamente, mesmo que o canvas esteja deslocado ou ampliado.
    fn centered_world(
        &self,
        size: Size<i32, smithay::utils::Logical>,
    ) -> Point<f64, smithay::utils::Logical> {
        let viewport = self.output_logical_size.to_f64();
        let cascade = (self.windows.iter().filter(|managed| !managed.dialog).count() % 8) as f64
            * 28.0;
        let screen = Point::from((
            ((viewport.w - size.w as f64) / 2.0 + cascade).max(0.0),
            ((viewport.h - size.h as f64) / 2.0 + cascade).max(0.0),
        ));
        self.to_world(screen)
    }

    /// Troca o id provisório pelo id definitivo no primeiro buffer, quando o
    /// `parent` xdg já foi informado e é possível distinguir diálogos.
    fn finalize_pending_windows(
        &mut self,
        pending: Vec<(u32, bool, Option<u32>, Size<i32, smithay::utils::Logical>)>,
    ) {
        if pending.is_empty() {
            return;
        }

        for (old_id, dialog, _parent_id, size) in pending {
            let new_id = if dialog {
                let id = self.next_dialog_id;
                self.next_dialog_id += 1;
                id
            } else {
                let id = self.next_window_id;
                self.next_window_id += 1;
                id
            };

            // O pai pode ter sido renumerado antes neste mesmo lote.
            let parent_id = self
                .windows
                .iter()
                .find(|managed| managed.id == old_id)
                .and_then(|managed| managed.parent_id);

            // Janelas comuns que pedem um tamanho maior que o espaço visível
            // (por exemplo, reabrindo no tamanho maximizado da sessão
            // anterior) são reduzidas para não esconder o deck inteiro.
            let requested_size = size;
            let size = if dialog {
                requested_size
            } else {
                self.clamp_window_size(requested_size)
            };

            let world = if dialog {
                parent_id
                    .and_then(|parent| self.dialog_world(parent, size))
                    .unwrap_or_else(|| Point::from((320.0, 240.0)))
            } else {
                self.centered_world(size)
            };

            if let Some(managed) = self.windows.iter_mut().find(|managed| managed.id == old_id) {
                managed.id = new_id;
                managed.pending_id = false;
                managed.world = world;
                managed.dialog = dialog;
                managed.parent_id = parent_id;
                managed.dialog_needs_center = false;
                managed.window.override_z_index(if dialog { Z_DIALOG } else { Z_WINDOW });
            }

            if !dialog && size != requested_size {
                if let Some(toplevel) = self
                    .windows
                    .iter()
                    .find(|managed| managed.id == new_id)
                    .and_then(|managed| managed.window.toplevel())
                {
                    info!(
                        window_id = new_id,
                        from_w = requested_size.w,
                        from_h = requested_size.h,
                        to_w = size.w,
                        to_h = size.h,
                        "reduzindo janela para caber no espaço visível"
                    );
                    toplevel.with_pending_state(|pending| {
                        pending.size = Some(size);
                    });
                    toplevel.send_configure();
                }
            }

            info!(
                window_id = new_id,
                dialog,
                x = world.x,
                y = world.y,
                w = size.w,
                h = size.h,
                "janela posicionada no canvas"
            );

            // Filhos que apontavam para o id antigo passam a apontar para o novo.
            for managed in &mut self.windows {
                if managed.parent_id == Some(old_id) {
                    managed.parent_id = Some(new_id);
                }
            }

            if self.focused_window == Some(old_id) {
                self.focused_window = Some(new_id);
            }
            for id in &mut self.focus_order {
                if *id == old_id {
                    *id = new_id;
                }
            }
            if let Some(move_state) = self.move_state.as_mut() {
                if move_state.window_id == old_id {
                    move_state.window_id = new_id;
                }
            }
            if let Some(resize_state) = self.resize_state.as_mut() {
                if resize_state.window_id == old_id {
                    resize_state.window_id = new_id;
                }
            }

            info!(old_id, new_id, dialog, "id definitivo atribuído");

            // Janela pronta para ser exposta a barras e taskbars externas.
            // O próprio shell não entra na lista.
            if let Some(window) = self
                .windows
                .iter()
                .find(|managed| managed.id == new_id)
                .map(|managed| managed.window.clone())
            {
                let is_shell = window.toplevel().is_some_and(is_shell_surface);
                if !is_shell {
                    self.register_foreign_toplevel(new_id, &window);
                }
            }
        }

        self.apply_stack_z();
        self.refresh_overview();
        self.apply_layout();
        // O shell precisa conhecer posição e tamanho para vestir o cartão
        // com a barra de título da janela recém-mapeada.
        self.broadcast_cluster();
    }

    /// Dá foco de teclado, sobe a janela na pilha e atualiza a ordem de foco.
    fn focus_window(&mut self, window_id: u32) {
        let Some((window, surface)) = self
            .windows
            .iter()
            .find(|managed| managed.id == window_id)
            .map(|managed| {
                (
                    managed.window.clone(),
                    managed.window.wl_surface().map(|surface| surface.into_owned()),
                )
            })
        else {
            return;
        };

        if let Some(surface) = surface {
            let keyboard = self.keyboard.clone();
            keyboard.set_focus(self, Some(surface), SERIAL_COUNTER.next_serial());
        }
        // Estado `Activated` é o que clientes com CSD usam para pintar a
        // decoração; sem ele a barra parece sempre inativa.
        for managed in &mut self.windows {
            if managed.id != window_id && managed.window.set_activated(false) {
                if let Some(toplevel) = managed.window.toplevel().cloned() {
                    toplevel.send_pending_configure();
                }
            }
        }
        if window.set_activated(true) {
            if let Some(toplevel) = window.toplevel().cloned() {
                toplevel.send_pending_configure();
            }
        }

        self.focused_window = Some(window_id);
        self.focus_order.retain(|id| *id != window_id);
        self.focus_order.push(window_id);
        self.raise_window(window_id);
    }

    /// Traz a janela para o topo da pilha do canvas.
    fn raise_window(&mut self, window_id: u32) {
        if !self.windows.iter().any(|window| window.id == window_id) {
            return;
        }
        self.stack.retain(|id| *id != window_id);
        self.stack.push(window_id);
        self.apply_stack_z();
    }

    /// Envia a janela para o fundo da pilha do canvas.
    fn lower_window(&mut self, window_id: u32) {
        if !self.windows.iter().any(|window| window.id == window_id) {
            return;
        }
        self.stack.retain(|id| *id != window_id);
        self.stack.insert(0, window_id);
        self.apply_stack_z();
    }

    /// Reaplica os índices Z a partir da pilha: janelas comuns ocupam
    /// `Z_WINDOW..`, maximizadas ficam em `Z_MAXIMIZED` e diálogos em
    /// `Z_DIALOG`. É a única fonte de ordem entre janelas.
    fn apply_stack_z(&mut self) {
        let mut stack = std::mem::take(&mut self.stack);
        stack.retain(|id| {
            self.windows
                .iter()
                .any(|window| window.id == *id && !window.pending_id)
        });
        for managed in &self.windows {
            if !managed.pending_id && !managed.dialog && !stack.contains(&managed.id) {
                stack.push(managed.id);
            }
        }

        let mut normal: u8 = 0;
        for id in &stack {
            let Some(managed) = self.windows.iter().find(|window| window.id == *id) else {
                continue;
            };
            if managed.maximized || managed.fullscreen {
                managed.window.override_z_index(Z_MAXIMIZED);
            } else {
                managed
                    .window
                    .override_z_index(Z_WINDOW.saturating_add(normal.min(180)));
                normal = normal.saturating_add(1);
            }
        }
        for managed in &self.windows {
            if managed.dialog {
                managed.window.override_z_index(Z_DIALOG);
            }
        }
        debug!(?stack, "pilha do canvas");
        self.stack = stack;
        self.space.refresh();
    }

    /// Nenhuma janela ativa (foco voltou para o shell).
    fn deactivate_windows(&mut self) {
        for managed in &mut self.windows {
            if managed.window.set_activated(false) {
                if let Some(toplevel) = managed.window.toplevel().cloned() {
                    toplevel.send_pending_configure();
                }
            }
        }
        self.focused_window = None;
    }

    fn cycle_focus(&mut self, reverse: bool) {
        if self.focus_order.is_empty() {
            return;
        }
        let len = self.focus_order.len();
        let next = match self
            .focused_window
            .and_then(|id| self.focus_order.iter().position(|candidate| *candidate == id))
        {
            // Nada focado (shell em primeiro plano): Alt+Tab traz a janela
            // mais recente; Alt+Shift+Tab traz a mais antiga.
            None => {
                if reverse {
                    0
                } else {
                    len - 1
                }
            }
            Some(current) => {
                if reverse {
                    (current + len - 1) % len
                } else {
                    (current + 1) % len
                }
            }
        };
        let id = self.focus_order[next];
        self.focus_window(id);
    }

    fn focused_or_pointer_window(&self) -> Option<u32> {
        self.focused_window
            .or_else(|| self.window_id_under(self.pointer_screen))
    }

    /// Só restaura: janelas maximizadas (por exemplo pelo AutoMaximize) voltam
    /// ao tamanho anterior. Não maximiza nada por atalho.
    fn restore_maximized_focused(&mut self) {
        let Some(id) = self.focused_or_pointer_window() else {
            return;
        };
        let maximized = self
            .windows
            .iter()
            .find(|managed| managed.id == id)
            .map(|managed| managed.maximized)
            .unwrap_or(false);
        if maximized {
            self.set_maximized(id, false);
        }
    }

    fn toggle_fullscreen_focused(&mut self) {
        let Some(id) = self.focused_or_pointer_window() else {
            return;
        };
        let next = self
            .windows
            .iter()
            .find(|managed| managed.id == id)
            .map(|managed| !managed.fullscreen)
            .unwrap_or(false);
        self.set_fullscreen(id, next);
    }

    /// Detecta se o ponteiro está sobre a borda de uma janela redimensionável.
    fn resize_edges_under(
        &self,
        location: Point<f64, smithay::utils::Logical>,
    ) -> Option<(u32, ResizeEdges)> {
        if self.overview.is_some() {
            return None;
        }
        let (window_id, _, _, _) = self.window_under(location)?;
        let managed = self
            .windows
            .iter()
            .find(|managed| managed.id == window_id)?;
        if managed.maximized || managed.fullscreen || managed.dialog {
            return None;
        }

        let geometry = self.window_screen_rect(managed).to_f64();
        if !geometry.contains(location) {
            return None;
        }

        let margin = RESIZE_MARGIN as f64;
        let mut edges = ResizeEdges::default();
        if location.x - geometry.loc.x <= margin {
            edges.left = true;
        }
        if geometry.loc.x + geometry.size.w - location.x <= margin {
            edges.right = true;
        }
        // A faixa superior fica para o cliente: é onde os clientes com
        // decoração própria (CSD) desenham a barra de título e os botões de
        // fechar/maximizar. Roubá-la transformava o gesto de arrastar a
        // janela em redimensionamento e engolia o clique no botão fechar.
        // Quem precisa redimensionar pelo topo usa o `resize_request` do CSD.
        if geometry.loc.y + geometry.size.h - location.y <= margin {
            edges.bottom = true;
        }

        edges.any().then_some((window_id, edges))
    }

    fn begin_resize(
        &mut self,
        window_id: u32,
        edges: ResizeEdges,
        pointer_screen: Point<f64, smithay::utils::Logical>,
        forward_buttons: bool,
    ) {
        if !edges.any() {
            return;
        }
        let Some(managed) = self
            .windows
            .iter()
            .find(|managed| managed.id == window_id)
        else {
            return;
        };
        if managed.maximized || managed.fullscreen || managed.dialog {
            return;
        }

        let size = if managed.size.w > 0 && managed.size.h > 0 {
            managed.size
        } else {
            managed.window.geometry().size
        };
        self.resize_state = Some(ResizeState {
            window_id,
            edges,
            start_pointer_screen: pointer_screen,
            start_world: managed.world,
            start_size: size,
            forward_buttons,
        });
        self.windows
            .iter_mut()
            .find(|candidate| candidate.id == window_id)
            .map(|candidate| candidate.allow_oversize = true);
        info!(window_id, ?edges, forward_buttons, "redimensionamento iniciado");
    }

    fn update_resize(&mut self, pointer_screen: Point<f64, smithay::utils::Logical>) {
        let canvas = self.canvas;
        let Some((window_id, edges, start_pointer, start_world, start_size)) =
            self.resize_state.as_ref().map(|state| {
                (
                    state.window_id,
                    state.edges,
                    state.start_pointer_screen,
                    state.start_world,
                    state.start_size,
                )
            })
        else {
            return;
        };

        let dx = pointer_screen.x - start_pointer.x;
        let dy = pointer_screen.y - start_pointer.y;
        let (world, size) = resize_geometry(
            edges,
            start_world,
            start_size,
            dx,
            dy,
            self.canvas.scale,
        );

        let Some(managed) = self
            .windows
            .iter_mut()
            .find(|managed| managed.id == window_id)
        else {
            self.resize_state = None;
            return;
        };
        managed.world = world;
        managed.size = size;

        if let Some(toplevel) = managed.window.toplevel().cloned() {
            toplevel.with_pending_state(|pending| {
                pending.size = Some(size);
            });
            toplevel.send_configure();
        } else if let Some(surface) = managed.window.x11_surface() {
            let _ = surface.configure(Rectangle::new(canvas_screen(canvas, managed.world), size));
        }

        self.apply_layout();
        self.schedule_cluster();
    }

    fn end_resize(&mut self) -> bool {
        let forward = self
            .resize_state
            .as_ref()
            .map(|state| state.forward_buttons)
            .unwrap_or(false);
        if self.resize_state.take().is_some() {
            self.pending_cluster = false;
            self.broadcast_cluster();
            info!("redimensionamento concluído");
        }
        forward
    }

    /// Atualiza a moldura do shell durante o arrasto sem inundar o WS. O
    /// próximo frame libera qualquer atualização pendente.
    fn schedule_cluster(&mut self) {
        const INTERVAL: Duration = Duration::from_millis(20);
        let now = Instant::now();
        if self
            .last_cluster_at
            .map(|last| now.duration_since(last) >= INTERVAL)
            .unwrap_or(true)
        {
            self.broadcast_cluster();
            self.last_cluster_at = Some(now);
            self.pending_cluster = false;
        } else {
            self.pending_cluster = true;
        }
    }

    /// Chamado uma vez por frame: envia a posição mais recente se já passou
    /// o intervalo mínimo desde a última mensagem.
    fn flush_cluster_frame(&mut self) {
        const INTERVAL: Duration = Duration::from_millis(20);
        if !self.pending_cluster {
            return;
        }
        let now = Instant::now();
        if self
            .last_cluster_at
            .map(|last| now.duration_since(last) >= INTERVAL)
            .unwrap_or(true)
        {
            self.broadcast_cluster();
            self.last_cluster_at = Some(now);
            self.pending_cluster = false;
        }
    }

    /// Avisa os clientes WebSocket sobre as posições atuais das janelas.
    fn broadcast_cluster(&self) {
        if self.broadcast.receiver_count() == 0 {
            return;
        }
        let placements: Vec<ClusterPlacement> = self
            .windows
            .iter()
            // Janelas com id provisório ainda não têm lugar no canvas: o
            // cartão do shell só pode ser montado depois do primeiro buffer.
            .filter(|managed| !managed.pending_id)
            .map(|managed| {
                let size = if managed.size.w > 0 && managed.size.h > 0 {
                    managed.size
                } else {
                    managed.window.geometry().size
                };
                let title = managed
                    .window
                    .toplevel()
                    .and_then(|toplevel| toplevel_title(toplevel))
                    .or_else(|| managed.window.x11_surface().map(|surface| surface.title()))
                    .unwrap_or_default();
                ClusterPlacement {
                    id: managed.id.to_string(),
                    x: managed.world.x,
                    y: managed.world.y,
                    w: size.w as f64,
                    h: size.h as f64,
                    title,
                }
            })
            .collect();
        self.publish(IpcMessage::ClusterUpdate { placements });
    }

    /// Publica uma mensagem originada pelo compositor para todas as conexões
    /// IPC (WebSocket do frontend e clientes Unix, como o `shell-services`).
    fn publish(&self, message: IpcMessage) {
        let _ = self.broadcast.send((IPC_SOURCE_COMPOSITOR, message));
    }

    fn begin_move(
        &mut self,
        window_id: u32,
        pointer_screen: Point<f64, smithay::utils::Logical>,
        forward_buttons: bool,
    ) {
        let Some((world_x, world_y, maximized, fullscreen)) = self
            .windows
            .iter()
            .find(|managed| managed.id == window_id)
            .map(|managed| {
                (
                    managed.world.x,
                    managed.world.y,
                    managed.maximized,
                    managed.fullscreen,
                )
            })
        else {
            return;
        };
        if maximized || fullscreen {
            return;
        }

        let pointer_world = self.to_world(pointer_screen);
        self.move_state = Some(MoveState {
            window_id,
            grab_x: pointer_world.x - world_x,
            grab_y: pointer_world.y - world_y,
            forward_buttons,
        });
        info!(window_id, forward_buttons, "arrasto de janela iniciado");
    }

    fn update_move(&mut self, pointer_screen: Point<f64, smithay::utils::Logical>) {
        let Some((window_id, grab_x, grab_y)) = self
            .move_state
            .as_ref()
            .map(|state| (state.window_id, state.grab_x, state.grab_y))
        else {
            return;
        };

        let pointer_world = self.to_world(pointer_screen);
        let Some(managed) = self
            .windows
            .iter_mut()
            .find(|managed| managed.id == window_id)
        else {
            self.move_state = None;
            return;
        };
        managed.world = Point::from((
            pointer_world.x - grab_x,
            pointer_world.y - grab_y,
        ));

        self.apply_layout();
        self.schedule_cluster();
    }

    fn end_move(&mut self) {
        if self.move_state.take().is_some() {
            self.pending_cluster = false;
            self.broadcast_cluster();
            info!("arrasto de janela concluído");
        }
    }

    /// Abre/fecha a visão geral.
    fn toggle_overview(&mut self) {
        if self.overview.is_some() {
            self.exit_overview();
        } else {
            self.enter_overview();
        }
    }

    fn enter_overview(&mut self) {
        if self.overview.is_some() {
            return;
        }
        let windows = self.overview_windows();
        let cells = overview_cells(self.output_logical_size, &windows);
        if cells.is_empty() {
            // Sem janelas Wayland reais não há miniatura do compositor, mas o
            // modo ainda abre: o frontend arruma os cartões de demonstração do
            // shell numa grade equivalente e o HUD mostra o botão pressionado.
            info!("visão geral aberta sem janelas reais");
        } else {
            info!(count = cells.len(), "visão geral aberta");
        }
        self.overview = Some(Overview { cells });
        self.broadcast_overview();
        self.apply_layout();
    }

    fn exit_overview(&mut self) {
        if self.overview.take().is_some() {
            info!("visão geral fechada");
            self.broadcast_overview();
            self.apply_layout();
        }
    }

    /// Recalcula a grade depois que uma janela abre, fecha ou a saída muda.
    fn refresh_overview(&mut self) {
        if self.overview.is_none() {
            return;
        }
        let windows = self.overview_windows();
        self.overview = Some(Overview {
            cells: overview_cells(self.output_logical_size, &windows),
        });
        self.broadcast_overview();
    }

    fn overview_windows(&self) -> Vec<(u32, Size<i32, smithay::utils::Logical>)> {
        self.windows
            .iter()
            .filter(|managed| !managed.pending_id)
            .map(|managed| {
                let size = if managed.size.w > 0 && managed.size.h > 0 {
                    managed.size
                } else {
                    managed.window.geometry().size
                };
                (managed.id, size)
            })
            .collect()
    }

    fn broadcast_overview(&self) {
        let windows = self
            .overview
            .as_ref()
            .map(|overview| overview.cells.len())
            .unwrap_or(0);
        self.publish(IpcMessage::OverviewState {
            active: self.overview.is_some(),
            windows,
        });
    }

    /// Reaplica a transformação do canvas a todas as janelas conhecidas.
    fn apply_layout(&mut self) {
        // O shell fica sempre em (0,0) e com z-index 0, abaixo das janelas.
        // `map_element` em um elemento já mapeado só atualiza a posição e o
        // recoloca no topo do próprio z-index; a janela focada é reerguida
        // no fim para preservar a pilha de foco.
        if let Some(shell) = self.shell_window.as_ref() {
            let window = shell.window.clone();
            window.override_z_index(Z_SHELL);
            self.space.map_element(window, Point::from((0, 0)), false);
        }

        let placements: Vec<(Window, Point<i32, smithay::utils::Logical>)> = self
            .windows
            .iter()
            .map(|managed| {
                let location = if managed.maximized || managed.fullscreen {
                    Point::from((0, 0))
                } else {
                    self.to_screen(managed.world)
                };
                (managed.window.clone(), location)
            })
            .collect();

        for (window, screen) in placements {
            self.space.map_element(window, screen, false);
        }

        // X11 precisa saber onde está no "root" do XWayland para posicionar
        // menus e diálogos. Só reconfigura quando a geometria mudou.
        for managed in &self.windows {
            if let Some(surface) = managed.window.x11_surface() {
                if !surface.alive() {
                    continue;
                }
                let rect = Rectangle::new(self.to_screen(managed.world), managed.size);
                if surface.geometry() != rect {
                    let _ = surface.configure(rect);
                }
            }
        }

        if let Some(id) = self.focused_window {
            if let Some(window) = self
                .windows
                .iter()
                .find(|managed| managed.id == id)
                .map(|managed| managed.window.clone())
            {
                self.space.raise_element(&window, false);
            }
        }
        self.space.refresh();
    }

    /// Transforma a janela recém-mapeada em shell do desktop: ocupa a saída,
    /// fica fixa em (0,0) e deixa de participar do canvas.
    fn adopt_shell(&mut self, surface: &ToplevelSurface) {
        if self.shell_window.is_some() {
            return;
        }
        let Some(index) = self
            .windows
            .iter()
            .position(|managed| managed.window.toplevel() == Some(surface))
        else {
            return;
        };

        let mut managed = self.windows.remove(index);
        // O shell não é uma janela de aplicativo: sai da lista de toplevels.
        self.unregister_foreign_toplevel(managed.id);
        managed.id = 0;
        managed.pending_id = false;
        managed.world = Point::from((0.0, 0.0));
        managed.maximized = false;
        managed.fullscreen = false;
        managed.restore = None;
        managed.dialog = false;
        managed.parent_id = None;
        managed.dialog_needs_center = false;
        managed.size = self.output_logical_size;
        managed.window.override_z_index(Z_SHELL);

        let size = self.output_logical_size;
        surface.with_pending_state(|pending| {
            pending.size = Some(size);
            pending.states.set(xdg_toplevel::State::Activated);
        });
        surface.send_configure();

        info!("janela do shell adotada");
        self.shell_window = Some(managed);
        // O shell não é uma janela interna fechável.
        self.focused_window = None;
        self.apply_layout();
    }

    /// Reenvia o tamanho da saída para o shell após um redimensionamento.
    fn resize_shell(&mut self) {
        let Some(shell) = self.shell_window.as_ref() else {
            return;
        };
        let Some(toplevel) = shell.window.toplevel().cloned() else {
            return;
        };
        let size = self.output_logical_size;
        toplevel.with_pending_state(|pending| {
            pending.size = Some(size);
            pending.states.set(xdg_toplevel::State::Activated);
        });
        toplevel.send_configure();
        self.space.refresh();
    }

    fn handle_ipc(&mut self, message: IpcMessage) {
        match message {
            IpcMessage::CanvasTransform {
                scale,
                offset_x,
                offset_y,
            } => {
                // O frontend mudou o enquadramento: a grade antiga deixaria
                // de corresponder às janelas.
                if self.overview.is_some() {
                    self.exit_overview();
                }
                self.canvas = CanvasTransform {
                    scale: scale.max(0.01),
                    offset_x,
                    offset_y,
                };
                info!(
                    scale = self.canvas.scale,
                    offset_x = self.canvas.offset_x,
                    offset_y = self.canvas.offset_y,
                    "canvas transform"
                );
                self.apply_layout();
            }
            IpcMessage::ClusterUpdate { placements } => {
                self.apply_cluster(placements);
            }
            IpcMessage::AutoMaximize { window_id, state } => {
                self.set_maximized(window_id, state);
            }
            IpcMessage::SetTheme { theme } => {
                self.theme = theme;
                info!(%theme, "tema recebido do frontend");
            }
            IpcMessage::LaunchApp { exec } => {
                self.launch_app(&exec);
            }
            IpcMessage::WindowResizeTool { active } => {
                self.resize_tool = active;
                info!(active, "ferramenta de redimensionamento");
            }
            IpcMessage::ChromeRects { rects, hud_bottom } => {
                self.hud_bottom = (hud_bottom > 0.0).then(|| hud_bottom.round() as i32);
                self.chrome_rects = rects
                    .iter()
                    .filter(|rect| rect.w > 0.0 && rect.h > 0.0)
                    .map(|rect: &ChromeRect| {
                        Rectangle::new(
                            Point::from((rect.x.round() as i32, rect.y.round() as i32)),
                            Size::from((rect.w.round() as i32, rect.h.round() as i32)),
                        )
                    })
                    .collect();
            }
            IpcMessage::CloseWindow { window_id } => {
                self.close_window(window_id);
            }
            IpcMessage::RaiseWindow { window_id } => {
                self.raise_window(window_id);
            }
            IpcMessage::LowerWindow { window_id } => {
                self.lower_window(window_id);
            }
            IpcMessage::WindowClosed { .. } => {
                // Mensagem enviada pelo compositor; ignorada se ecoada.
            }
            IpcMessage::AppList { .. } => {
                // Mensagem enviada pelo compositor; ignorada se ecoada.
            }
            IpcMessage::ZoomCanvas { .. } => {
                // Mensagem enviada pelo compositor; ignorada se ecoada.
            }
            IpcMessage::ToggleOverview => {
                self.toggle_overview();
            }
            IpcMessage::OverviewState { .. } => {
                // Mensagem enviada pelo compositor; ignorada se ecoada.
            }
            IpcMessage::Toast { .. } => {
                // Mensagem enviada pelo compositor; ignorada se ecoada.
            }
            IpcMessage::Notification { .. }
            | IpcMessage::NotificationClosed { .. }
            | IpcMessage::NotificationAction { .. }
            | IpcMessage::NotificationHold { .. }
            | IpcMessage::DndState { .. }
            | IpcMessage::NotificationHistory { .. }
            | IpcMessage::NotificationHistoryRequest
            | IpcMessage::RegionList { .. }
            | IpcMessage::RegionListRequest
            | IpcMessage::SaveRegion { .. }
            | IpcMessage::RegionSaved { .. }
            | IpcMessage::GotoRegion { .. }
            | IpcMessage::DeleteRegion { .. }
            | IpcMessage::TrayUpdate { .. }
            | IpcMessage::TrayActivate { .. }
            | IpcMessage::TraySecondaryActivate { .. }
            | IpcMessage::TrayMenuRequest { .. }
            | IpcMessage::TrayRequest
            | IpcMessage::TrayMenu { .. }
            | IpcMessage::TrayMenuEvent { .. }
            | IpcMessage::MediaState { .. }
            | IpcMessage::MediaCommand { .. }
            | IpcMessage::MediaRequest
            | IpcMessage::StatusRequest
            | IpcMessage::NetworkState { .. }
            | IpcMessage::BatteryState { .. }
            | IpcMessage::VolumeState { .. }
            | IpcMessage::BrightnessState { .. }
            | IpcMessage::VolumeCommand { .. }
            | IpcMessage::BrightnessCommand { .. } => {
                // Notificações são roteadas pelo barramento IPC entre o
                // `shell-services` (Unix) e o frontend (WebSocket). O
                // compositor não precisa agir sobre elas.
            }
        }
    }

    fn close_window(&mut self, window_id: u32) {
        let Some(managed) = self.windows.iter().find(|w| w.id == window_id) else {
            debug!(window_id, "CloseWindow para janela inexistente");
            // Cartão sem janela real (demonstração): remove só o cartão.
            self.publish(IpcMessage::WindowClosed { window_id });
            return;
        };
        if let Some(toplevel) = managed.window.toplevel().cloned() {
            info!(window_id, "fechando janela a pedido do frontend");
            toplevel.send_close();
        } else if let Some(surface) = managed.window.x11_surface() {
            info!(window_id, "fechando janela X11 a pedido do frontend");
            let _ = surface.close();
        }
    }

    fn launch_app(&mut self, exec: &str) {
        let exec = exec.trim();
        if exec.is_empty() {
            warn!("LaunchApp recebido sem comando");
            return;
        }

        // Token de ativação: o aplicativo lançado pode pedir foco ao mapear,
        // mesmo que o foco ainda esteja no shell.
        let activation_token = self
            .activation_state
            .create_external_token(None)
            .0
            .as_str()
            .to_string();

        // Aplicativos Waydroid só abrem com a sessão Android ativa. Se ela
        // estiver parada, sobe a sessão no display Wayland do deck, espera
        // ficar pronta e só então executa o comando do aplicativo.
        let script = if exec.starts_with("waydroid ") {
            format!(
                "if ! waydroid status 2>/dev/null | grep -q 'Session:[[:space:]]*RUNNING'; then\n\
                 : >/tmp/spatial-deck-waydroid.log\n\
                 waydroid session start >>/tmp/spatial-deck-waydroid.log 2>&1 &\n\
                 for _ in $(seq 1 120); do\n\
                 grep -q 'is ready' /tmp/spatial-deck-waydroid.log 2>/dev/null && break\n\
                 sleep 0.5\n\
                 done\n\
                 sleep 5\n\
                 fi\n\
                 exec {exec}"
            )
        } else {
            format!("exec {exec}")
        };

        let mut command = Command::new("sh");
        command
            .arg("-c")
            .arg(script)
            .env("WAYLAND_DISPLAY", WAYLAND_SOCKET_NAME)
            .env("GDK_BACKEND", "wayland")
            .env("QT_QPA_PLATFORM", "wayland")
            .env("SDL_VIDEODRIVER", "wayland")
            .env("MOZ_ENABLE_WAYLAND", "1")
            .env("ELECTRON_OZONE_PLATFORM_HINT", "wayland")
            .envs(
                self.xdisplay
                    .map(|display| ("DISPLAY", format!(":{display}")))
                    .into_iter(),
            )
            // O compositor roda com o sysroot local do shell (GTK/WebKit).
            // Aplicativos e o Waydroid devem usar as bibliotecas do sistema.
            .env_remove("LD_LIBRARY_PATH")
            .env("XDG_ACTIVATION_TOKEN", &activation_token)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());

        match command.spawn() {
            Ok(mut child) => {
                info!(exec, pid = child.id(), "aplicativo iniciado");
                // Recolhe o processo quando ele terminar para não deixar
                // zumbis no sistema.
                std::thread::spawn(move || {
                    let _ = child.wait();
                });
            }
            Err(error) => {
                warn!(exec, %error, "falha ao iniciar aplicativo");
                self.publish(IpcMessage::Toast {
                    message: format!("Não foi possível iniciar: {exec}"),
                });
            }
        }
    }

    fn apply_cluster(&mut self, placements: Vec<ClusterPlacement>) {
        for placement in placements {
            let Ok(id) = placement.id.parse::<u32>() else {
                continue;
            };
            if let Some(managed) = self
                .windows
                .iter_mut()
                .find(|w| w.id == id && !w.dialog)
            {
                managed.world = Point::from((placement.x, placement.y));
            }
        }
        self.apply_layout();
    }

    fn set_maximized(&mut self, window_id: u32, state: bool) {
        let output_size = self.output_logical_size;
        let (window, z_index) = {
            let Some(managed) = self.windows.iter_mut().find(|w| w.id == window_id) else {
                debug!(window_id, "AutoMaximize para janela ainda não mapeada");
                return;
            };
            if managed.dialog || managed.fullscreen || managed.maximized == state {
                return;
            }
            let window = managed.window.clone();
            let Some(toplevel) = managed.window.toplevel().cloned() else {
                return;
            };

            if state {
                let restore_size = if managed.size.w > 0 && managed.size.h > 0 {
                    managed.size
                } else {
                    managed.window.geometry().size
                };
                managed.restore = Some(WindowRestore {
                    world: managed.world,
                    size: restore_size,
                });
                managed.maximized = true;
                managed.size = output_size;
                toplevel.with_pending_state(|pending| {
                    pending.states.set(xdg_toplevel::State::Maximized);
                    pending.size = Some(output_size);
                });
                toplevel.send_configure();
                (window, Z_MAXIMIZED)
            } else {
                managed.maximized = false;
                if let Some(restore) = managed.restore.take() {
                    managed.world = restore.world;
                    managed.size = restore.size;
                    toplevel.with_pending_state(|pending| {
                        pending.states.unset(xdg_toplevel::State::Maximized);
                        pending.size = Some(restore.size);
                    });
                    toplevel.send_configure();
                }
                (window, Z_WINDOW)
            }
        };

        window.override_z_index(z_index);
        self.raise_window(window_id);
        self.apply_layout();
    }

    fn set_fullscreen(&mut self, window_id: u32, state: bool) {
        let output_size = self.output_logical_size;
        let (window, z_index) = {
            let Some(managed) = self.windows.iter_mut().find(|w| w.id == window_id) else {
                return;
            };
            if managed.dialog || managed.fullscreen == state {
                return;
            }
            let window = managed.window.clone();
            let Some(toplevel) = managed.window.toplevel().cloned() else {
                return;
            };

            if state {
                let restore_size = if managed.size.w > 0 && managed.size.h > 0 {
                    managed.size
                } else {
                    managed.window.geometry().size
                };
                // Preserva a geometria original se a janela já veio de um
                // maximizado, para o restaurar não perder a posição no canvas.
                if managed.restore.is_none() {
                    managed.restore = Some(WindowRestore {
                        world: managed.world,
                        size: restore_size,
                    });
                }
                managed.fullscreen = true;
                managed.maximized = false;
                managed.size = output_size;
                toplevel.with_pending_state(|pending| {
                    pending.states.set(xdg_toplevel::State::Fullscreen);
                    pending.size = Some(output_size);
                });
                toplevel.send_configure();
                (window, Z_MAXIMIZED)
            } else {
                managed.fullscreen = false;
                if let Some(restore) = managed.restore.take() {
                    managed.world = restore.world;
                    managed.size = restore.size;
                    toplevel.with_pending_state(|pending| {
                        pending.states.unset(xdg_toplevel::State::Fullscreen);
                        pending.size = Some(restore.size);
                    });
                    toplevel.send_configure();
                }
                (window, Z_WINDOW)
            }
        };

        window.override_z_index(z_index);
        self.raise_window(window_id);
        self.apply_layout();
    }

    fn window_id_for_x11(&self, surface: &X11Surface) -> Option<u32> {
        self.windows
            .iter()
            .find(|managed| managed.window.x11_surface() == Some(surface))
            .map(|managed| managed.id)
    }

    /// Insere uma janela X11 mapeada pelo XWM no canvas, com o mesmo
    /// tratamento de posicionamento e cartão das janelas Wayland.
    fn adopt_x11_window(&mut self, surface: X11Surface) {
        let id = self.next_window_id;
        self.next_window_id += 1;

        let size = self.clamp_window_size(surface.geometry().size);
        let world = self.centered_world(size);
        let screen = self.to_screen(world);
        let _ = surface.configure(Rectangle::new(screen, size));

        let window = Window::new_x11_window(surface.clone());
        window.override_z_index(Z_WINDOW);
        self.space.map_element(window.clone(), screen, false);

        self.windows.push(ManagedWindow {
            id,
            window: window.clone(),
            world,
            size,
            maximized: false,
            fullscreen: false,
            restore: None,
            dialog: false,
            parent_id: None,
            dialog_needs_center: false,
            pending_id: false,
            allow_oversize: false,
        });
        self.register_foreign_toplevel(id, &window);

        self.raise_window(id);
        self.focus_window(id);
        self.refresh_overview();
        self.apply_layout();
        self.broadcast_cluster();
        info!(window_id = id, "janela X11 adotada no canvas");
    }

    fn destroy_x11_window(&mut self, surface: &X11Surface) {
        let Some(index) = self
            .windows
            .iter()
            .position(|managed| managed.window.x11_surface() == Some(surface))
        else {
            return;
        };
        let managed = self.windows.remove(index);
        self.space.unmap_elem(&managed.window);
        self.unregister_foreign_toplevel(managed.id);
        self.stack.retain(|id| *id != managed.id);
        self.focus_order.retain(|id| *id != managed.id);
        if self.focused_window == Some(managed.id) {
            self.focused_window = None;
            let next = self.focus_order.last().copied();
            if let Some(next) = next {
                self.focus_window(next);
            }
        }
        self.publish(IpcMessage::WindowClosed { window_id: managed.id });
        self.refresh_overview();
        self.apply_layout();
    }

    fn window_id_for_surface(&self, surface: &ToplevelSurface) -> Option<u32> {
        self.windows
            .iter()
            .find(|w| w.window.toplevel() == Some(surface))
            .map(|w| w.id)
    }

    fn resize_output(&mut self, size: Size<i32, smithay::utils::Physical>) {
        if size == self.output_physical_size || size.w <= 0 || size.h <= 0 {
            return;
        }
        info!(w = size.w, h = size.h, "saída redimensionada");
        self.output_physical_size = size;
        self.output_logical_size = Size::from((size.w, size.h));
        self.output.change_current_state(
            Some(Mode {
                size,
                refresh: 60_000,
            }),
            None,
            None,
            None,
        );
        self.space.map_output(&self.output, (0, 0));
        // Layers ancoradas nas bordas acompanham o novo tamanho da saída.
        layer_map_for_output(&self.output).arrange();
        self.resize_shell();
        self.refresh_overview();
        self.space.refresh();
    }
}

impl App {
    /// Primeiro commit de uma layer surface: calcula a geometria ancorada no
    /// output e envia o configure inicial exigido pelo protocolo.
    fn commit_layer_surface(&mut self, surface: &WlSurface) {
        let mut map = layer_map_for_output(&self.output);
        let Some(layer) = map
            .layer_for_surface(surface, WindowSurfaceType::TOPLEVEL)
            .cloned()
        else {
            return;
        };

        let initial = with_states(layer.wl_surface(), |states| {
            states
                .data_map
                .get::<LayerSurfaceData>()
                .map(|data| data.lock().unwrap().initial_configure_sent)
        })
        .unwrap_or(true);

        // O mapa recalcula posição/tamanho; no primeiro commit isso prepara o
        // estado que o configure inicial vai anunciar ao cliente.
        map.arrange();
        drop(map);

        if !initial {
            layer.layer_surface().send_configure();
            info!(namespace = layer.namespace(), "layer surface configurada");
        }

        // Layers exclusivas (lock screen, menus de barra) capturam o teclado
        // assim que aparecem; as on-demand recebem foco no clique.
        if layer.cached_state().keyboard_interactivity == KeyboardInteractivity::Exclusive
            && self.layer_focus.as_ref() != Some(&layer)
        {
            self.focus_layer_surface(&layer);
        }
    }

    /// Dá foco de teclado a uma layer surface e tira o foco das janelas.
    fn focus_layer_surface(&mut self, layer: &DesktopLayerSurface) {
        self.layer_focus = Some(layer.clone());
        self.focused_window = None;
        let keyboard = self.keyboard.clone();
        keyboard.set_focus(
            self,
            Some(layer.wl_surface().clone()),
            SERIAL_COUNTER.next_serial(),
        );
    }

    /// Layer surface sob o ponteiro nas camadas acima das janelas.
    fn layer_under_pointer(
        &self,
        location: Point<f64, smithay::utils::Logical>,
    ) -> Option<DesktopLayerSurface> {
        let map = layer_map_for_output(&self.output);
        for wl_layer in [WlrLayer::Overlay, WlrLayer::Top] {
            if let Some(layer) = map.layer_under(wl_layer, location) {
                return Some(layer.clone());
            }
        }
        None
    }

    /// Foco de ponteiro para layers de topo/overlay (escala 1:1, sem canvas).
    fn layer_pointer_focus(
        &self,
        location: Point<f64, smithay::utils::Logical>,
    ) -> Option<(
        WlSurface,
        Point<f64, smithay::utils::Logical>,
        Point<f64, smithay::utils::Logical>,
    )> {
        let map = layer_map_for_output(&self.output);
        for wl_layer in [WlrLayer::Overlay, WlrLayer::Top] {
            let Some(layer) = map.layer_under(wl_layer, location) else {
                continue;
            };
            let geometry = map.layer_geometry(layer)?;
            let local = location - geometry.loc.to_f64();
            let (surface, surface_location) = layer.surface_under(local, WindowSurfaceType::ALL)?;
            let origin = geometry.loc.to_f64() + surface_location.to_f64();
            return Some((surface, origin, location));
        }
        None
    }

    /// Cria o handle ext-foreign-toplevel-list de uma janela recém-adotada.
    fn register_foreign_toplevel(&mut self, id: u32, window: &Window) {
        if self.foreign_toplevels.contains_key(&id) {
            return;
        }
        let (title, app_id) = window_identity(window);
        let handle = self
            .foreign_toplevel_state
            .new_toplevel::<Self>(title, app_id);
        self.foreign_toplevels.insert(id, handle);
    }

    /// Propaga título/app_id para as ferramentas externas que listam janelas.
    fn update_foreign_toplevel(&mut self, id: u32, window: &Window) {
        let Some(handle) = self.foreign_toplevels.get(&id) else {
            return;
        };
        let (title, app_id) = window_identity(window);
        handle.send_title(&title);
        handle.send_app_id(&app_id);
        handle.send_done();
    }

    fn unregister_foreign_toplevel(&mut self, id: u32) {
        if let Some(handle) = self.foreign_toplevels.remove(&id) {
            handle.send_closed();
        }
    }
}

/// Título e `app_id` de uma janela Wayland ou X11, para o foreign-toplevel.
fn window_identity(window: &Window) -> (String, String) {
    if let Some(toplevel) = window.toplevel() {
        (
            toplevel_title(toplevel).unwrap_or_default(),
            toplevel_app_id(toplevel).unwrap_or_default(),
        )
    } else if let Some(surface) = window.x11_surface() {
        (surface.title(), surface.class())
    } else {
        (String::new(), String::new())
    }
}

/// Elementos de uma layer surface (superfície + popups) na posição ancorada
/// pelo `LayerMap`. Layers não participam do pan/zoom do canvas.
fn layer_surface_elements(
    renderer: &mut GlesRenderer,
    layer: &DesktopLayerSurface,
    location: Point<i32, smithay::utils::Logical>,
) -> Vec<WaylandSurfaceRenderElement<GlesRenderer>> {
    let mut elements = Vec::new();
    for (popup, popup_offset) in PopupManager::popups_for_surface(layer.wl_surface()) {
        let offset = location + popup_offset - popup.geometry().loc;
        elements.extend(render_elements_from_surface_tree(
            renderer,
            popup.wl_surface(),
            offset.to_physical_precise_round(1.0),
            smithay::utils::Scale::from(1.0),
            1.0,
            Kind::Unspecified,
        ));
    }
    elements.extend(render_elements_from_surface_tree(
        renderer,
        layer.wl_surface(),
        location.to_physical_precise_round(1.0),
        smithay::utils::Scale::from(1.0),
        1.0,
        Kind::Unspecified,
    ));
    elements
}

fn toplevel_app_id(surface: &ToplevelSurface) -> Option<String> {
    with_states(surface.wl_surface(), |states| {
        states
            .data_map
            .get::<XdgToplevelSurfaceData>()
            .and_then(|data| data.lock().ok().and_then(|data| data.app_id.clone()))
    })
}

fn toplevel_title(surface: &ToplevelSurface) -> Option<String> {
    with_states(surface.wl_surface(), |states| {
        states
            .data_map
            .get::<XdgToplevelSurfaceData>()
            .and_then(|data| data.lock().ok().and_then(|data| data.title.clone()))
    })
}

fn is_shell_surface(surface: &ToplevelSurface) -> bool {
    toplevel_app_id(surface).as_deref() == Some(SHELL_APP_ID)
        || toplevel_title(surface).as_deref() == Some("Spatial Deck Shell")
}

impl BufferHandler for App {
    fn buffer_destroyed(&mut self, _buffer: &wl_buffer::WlBuffer) {}
}

impl CompositorHandler for App {
    fn compositor_state(&mut self) -> &mut CompositorState {
        &mut self.compositor_state
    }

    fn client_compositor_state<'a>(&self, client: &'a Client) -> &'a CompositorClientState {
        if let Some(state) = client.get_data::<ClientState>() {
            return &state.compositor_state;
        }
        if let Some(state) = client.get_data::<XWaylandClientData>() {
            return &state.compositor_state;
        }
        panic!("cliente sem estado de compositor conhecido");
    }

    fn commit(&mut self, surface: &WlSurface) {
        on_commit_buffer_handler::<Self>(surface);
        self.popup_manager.commit(surface);
        self.commit_layer_surface(surface);

        // O commit pode vir de uma subsurface; atualiza a árvore de todas as
        // janelas, não só da superfície que commitou.
        if let Some(shell) = &mut self.shell_window {
            shell.window.on_commit();
            let size = shell.window.geometry().size;
            if size.w > 0 && size.h > 0 {
                shell.size = size;
            }
        }

        let mut recenter = None;
        let mut pending = Vec::new();
        let mut oversize = Vec::new();
        let mut resized = false;
        let viewport = self.output_logical_size;
        let max_w = ((viewport.w as f64) * 0.94) as i32;
        let max_h = ((viewport.h as f64) * 0.88) as i32;
        for managed in &mut self.windows {
            managed.window.on_commit();
            let geometry = managed.window.geometry();
            if geometry.size.w > 0 && geometry.size.h > 0 {
                managed.size = geometry.size;
                if managed.pending_id {
                    pending.push((
                        managed.id,
                        managed.dialog,
                        managed.parent_id,
                        geometry.size,
                    ));
                } else if managed.dialog_needs_center {
                    managed.dialog_needs_center = false;
                    if let Some(parent_id) = managed.parent_id {
                        recenter = Some((managed.id, parent_id, geometry.size));
                    }
                } else if !managed.dialog
                    && !managed.maximized
                    && !managed.fullscreen
                    && !managed.allow_oversize
                    && (geometry.size.w > max_w || geometry.size.h > max_h)
                {
                    // Alguns clientes reabrem no tamanho maximizado da sessão
                    // anterior, cobrindo o deck inteiro. Reduz e recentraliza.
                    oversize.push((
                        managed.id,
                        Size::from((
                            geometry.size.w.min(max_w).max(320),
                            geometry.size.h.min(max_h).max(240),
                        )),
                    ));
                }
            }
        }

        for (id, size) in oversize {
            let world = self.centered_world(size);
            let toplevel = self
                .windows
                .iter_mut()
                .find(|managed| managed.id == id)
                .map(|managed| {
                    managed.world = world;
                    managed.size = size;
                    managed.window.toplevel().cloned()
                })
                .flatten();
            if let Some(toplevel) = toplevel {
                info!(
                    window_id = id,
                    w = size.w,
                    h = size.h,
                    "reduzindo janela que cobria o deck"
                );
                toplevel.with_pending_state(|pending| {
                    pending.size = Some(size);
                });
                toplevel.send_configure();
            }
            resized = true;
        }
        if resized {
            self.apply_layout();
        }

        self.finalize_pending_windows(pending);

        if let Some((id, parent_id, size)) = recenter {
            if let Some(world) = self.dialog_world(parent_id, size) {
                if let Some(managed) = self.windows.iter_mut().find(|w| w.id == id) {
                    managed.world = world;
                }
                self.apply_layout();
            }
        }

        self.popup_manager.cleanup();
        self.space.refresh();
    }
}

/// Pede decoração do lado do servidor: a barra de título vem do desktop (o
/// cartão do shell), e o cliente não desenha a barra própria por cima.
fn request_server_decoration(toplevel: &ToplevelSurface) {
    toplevel.with_pending_state(|state| {
        state.decoration_mode = Some(
            smithay::reexports::wayland_protocols::xdg::decoration::zv1::server::zxdg_toplevel_decoration_v1::Mode::ServerSide,
        );
    });
    toplevel.send_pending_configure();
}

impl XdgDecorationHandler for App {
    fn new_decoration(&mut self, toplevel: ToplevelSurface) {
        request_server_decoration(&toplevel);
    }

    fn request_mode(
        &mut self,
        toplevel: ToplevelSurface,
        _mode: smithay::reexports::wayland_protocols::xdg::decoration::zv1::server::zxdg_toplevel_decoration_v1::Mode,
    ) {
        request_server_decoration(&toplevel);
    }

    fn unset_mode(&mut self, toplevel: ToplevelSurface) {
        request_server_decoration(&toplevel);
    }
}

smithay::delegate_xdg_decoration!(App);

impl XwmHandler for App {
    fn xwm_state(&mut self, _xwm: XwmId) -> &mut X11Wm {
        self.xwm.as_mut().expect("XWM não inicializado")
    }

    fn new_window(&mut self, _xwm: XwmId, _window: X11Surface) {}

    fn new_override_redirect_window(&mut self, _xwm: XwmId, _window: X11Surface) {}

    fn map_window_request(&mut self, _xwm: XwmId, window: X11Surface) {
        let _ = window.set_mapped(true);
        self.adopt_x11_window(window);
    }

    fn mapped_override_redirect_window(&mut self, _xwm: XwmId, window: X11Surface) {
        // Menus/tooltips X11 chegam como override-redirect. Ainda não há
        // rastreamento de popup X11 no canvas; manter no X evita cartões
        // fantasma.
        debug!(geometry = ?window.geometry(), "override-redirect X11 ignorado");
    }

    fn unmapped_window(&mut self, _xwm: XwmId, window: X11Surface) {
        // Não chama `set_mapped(false)`: o servidor X já desmapeou, e o
        // ReparentWindow que o XWM emitiria pode atingir uma janela
        // destruída (BadWindow). O estado local é limpo no destroy.
        self.destroy_x11_window(&window);
    }

    fn destroyed_window(&mut self, _xwm: XwmId, window: X11Surface) {
        self.destroy_x11_window(&window);
    }

    fn property_notify(&mut self, _xwm: XwmId, window: X11Surface, property: WmWindowProperty) {
        // Título/classe de janelas X11 também alimentam o foreign-toplevel.
        if matches!(property, WmWindowProperty::Title | WmWindowProperty::Class) {
            let Some(id) = self
                .windows
                .iter()
                .find(|managed| managed.window.x11_surface() == Some(&window))
                .map(|managed| managed.id)
            else {
                return;
            };
            if let Some(managed) = self
                .windows
                .iter()
                .find(|managed| managed.id == id)
                .map(|managed| managed.window.clone())
            {
                self.update_foreign_toplevel(id, &managed);
            }
        }
    }

    fn configure_request(
        &mut self,
        _xwm: XwmId,
        window: X11Surface,
        _x: Option<i32>,
        _y: Option<i32>,
        w: Option<u32>,
        h: Option<u32>,
        _reorder: Option<Reorder>,
    ) {
        if !window.alive() {
            return;
        }
        let Some(id) = self.window_id_for_x11(&window) else {
            return;
        };
        let canvas = self.canvas;
        let Some(managed) = self.windows.iter_mut().find(|managed| managed.id == id) else {
            return;
        };
        if let Some(width) = w {
            managed.size.w = width as i32;
        }
        if let Some(height) = h {
            managed.size.h = height as i32;
        }
        let rect = Rectangle::new(canvas_screen(canvas, managed.world), managed.size);
        let _ = window.configure(rect);
        self.broadcast_cluster();
    }

    fn configure_notify(
        &mut self,
        _xwm: XwmId,
        window: X11Surface,
        geometry: Rectangle<i32, smithay::utils::Logical>,
        _above: Option<u32>,
    ) {
        if !window.alive() {
            return;
        }
        let Some(id) = self.window_id_for_x11(&window) else {
            return;
        };
        let canvas = self.canvas;
        if let Some(managed) = self.windows.iter_mut().find(|managed| managed.id == id) {
            managed.world = canvas_world(canvas, geometry.loc.to_f64());
            managed.size = geometry.size;
            self.space
                .map_element(managed.window.clone(), geometry.loc, false);
        }
        self.broadcast_cluster();
    }

    fn resize_request(
        &mut self,
        _xwm: XwmId,
        window: X11Surface,
        _button: u32,
        edge: X11ResizeEdge,
    ) {
        if let Some(id) = self.window_id_for_x11(&window) {
            self.focus_window(id);
            self.begin_resize(
                id,
                ResizeEdges::from_x11(edge),
                self.pointer_screen,
                true,
            );
        }
    }

    fn move_request(&mut self, _xwm: XwmId, window: X11Surface, _button: u32) {
        if let Some(id) = self.window_id_for_x11(&window) {
            self.focus_window(id);
            self.begin_move(id, self.pointer_screen, true);
        }
    }
}

impl XdgShellHandler for App {
    fn xdg_shell_state(&mut self) -> &mut XdgShellState {
        &mut self.xdg_shell_state
    }

    fn new_toplevel(&mut self, surface: ToplevelSurface) {
        let parent_surface = with_states(surface.wl_surface(), |states| {
            states
                .data_map
                .get::<XdgToplevelSurfaceData>()
                .and_then(|data| data.lock().ok().and_then(|data| data.parent.clone()))
        });
        let parent_id = parent_surface
            .as_ref()
            .and_then(|parent| self.window_id_for_wl_surface(parent));
        // Diálogos do próprio shell (seletor de arquivos do WebKit, por
        // exemplo) têm a janela do shell como pai transitório. Tratá-los como
        // diálogo evita que virem um cartão comum e ficarem atrás do deck.
        let parent_is_shell = parent_surface.as_ref().is_some_and(|parent| {
            self.shell_window
                .as_ref()
                .and_then(|shell| shell.window.wl_surface())
                .as_deref()
                == Some(parent)
        });
        let dialog = parent_id.is_some() || parent_is_shell;

        // O `parent` xdg só é conhecido depois de `get_toplevel`, então o id
        // definitivo (cartão 1..4 ou diálogo 1000+) é atribuído no primeiro
        // buffer, em `finalize_pending_windows`.
        let id = self.next_temp_id;
        self.next_temp_id += 1;

        let window = Window::new_wayland_window(surface.clone());
        let world = if let Some(parent_id) = parent_id {
            self.dialog_world(parent_id, Size::from(DIALOG_WINDOW_SIZE))
                .unwrap_or_else(|| Point::from((320.0, 240.0)))
        } else {
            self.centered_world(Size::from(DEFAULT_WINDOW_SIZE))
        };
        let screen = self.to_screen(world);

        window.override_z_index(if dialog { Z_DIALOG } else { Z_WINDOW });

        surface.with_pending_state(|pending| {
            if !dialog {
                pending.size = Some(Size::from(DEFAULT_WINDOW_SIZE));
            }
            pending.states.set(xdg_toplevel::State::Activated);
        });
        surface.send_configure();

        self.space.map_element(window.clone(), screen, false);
        let initial_size = if dialog {
            Size::from(DIALOG_WINDOW_SIZE)
        } else {
            Size::from(DEFAULT_WINDOW_SIZE)
        };
        self.windows.push(ManagedWindow {
            id,
            window,
            world,
            size: initial_size,
            maximized: false,
            fullscreen: false,
            restore: None,
            dialog,
            parent_id,
            dialog_needs_center: dialog,
            pending_id: true,
            allow_oversize: false,
        });

        // Só a primeira janela do shell é adotada; diálogos posteriores do
        // mesmo processo precisam de foco próprio para serem utilizáveis.
        let shell_surface = self.shell_window.is_none() && is_shell_surface(&surface);
        if !shell_surface {
            self.focus_window(id);
        } else {
            let keyboard = self.keyboard.clone();
            keyboard.set_focus(
                self,
                Some(surface.wl_surface().clone()),
                SERIAL_COUNTER.next_serial(),
            );
        }
        info!(
            id,
            dialog,
            app_id = ?toplevel_app_id(&surface),
            title = ?toplevel_title(&surface),
            "nova janela xdg_toplevel"
        );

        if self.shell_window.is_none() && is_shell_surface(&surface) {
            self.adopt_shell(&surface);
        }
    }

    fn app_id_changed(&mut self, surface: ToplevelSurface) {
        if self.shell_window.is_none() && is_shell_surface(&surface) {
            self.adopt_shell(&surface);
            return;
        }
        if let Some(id) = self.window_id_for_surface(&surface) {
            if let Some(window) = self
                .windows
                .iter()
                .find(|managed| managed.id == id)
                .map(|managed| managed.window.clone())
            {
                self.update_foreign_toplevel(id, &window);
            }
        }
    }

    fn title_changed(&mut self, surface: ToplevelSurface) {
        if self.shell_window.is_none() && is_shell_surface(&surface) {
            self.adopt_shell(&surface);
            return;
        }
        if let Some(id) = self.window_id_for_surface(&surface) {
            if let Some(window) = self
                .windows
                .iter()
                .find(|managed| managed.id == id)
                .map(|managed| managed.window.clone())
            {
                self.update_foreign_toplevel(id, &window);
            }
        }
        // O título também é o nome exibido na barra do cartão.
        self.broadcast_cluster();
    }

    fn new_popup(&mut self, surface: PopupSurface, _positioner: PositionerState) {
        // Sem o configure inicial o cliente não mapeia menus, tooltips e
        // comboboxes. O PopupManager cuida de renderização e hit-test.
        let _ = surface.send_configure();
        if let Err(error) = self.popup_manager.track_popup(PopupKind::Xdg(surface)) {
            debug!(?error, "falha ao rastrear popup");
        }
    }

    fn grab(&mut self, surface: PopupSurface, seat: wl_seat::WlSeat, serial: smithay::utils::Serial) {
        let popup = PopupKind::Xdg(surface);
        let Ok(root) = find_popup_root_surface(&popup) else {
            return;
        };
        let Some(seat) = Seat::from_resource(&seat) else {
            return;
        };
        if let Err(error) = self
            .popup_manager
            .grab_popup::<Self>(root, popup, &seat, serial)
        {
            debug!(?error, "grab de popup negado");
        }
    }

    fn move_request(
        &mut self,
        surface: ToplevelSurface,
        _seat: wl_seat::WlSeat,
        _serial: smithay::utils::Serial,
    ) {
        // Arrasto iniciado pela barra de título do cliente (CSD). A posição
        // inicial do ponteiro serve de âncora para o movimento.
        if let Some(id) = self.window_id_for_surface(&surface) {
            // `current_location` está nas coordenadas sintetizadas do foco
            // (sem escala); o arrasto precisa da posição real de tela.
            let location = self.pointer_screen;
            self.begin_move(id, location, true);
        }
    }

    fn reposition_request(
        &mut self,
        surface: PopupSurface,
        _positioner: PositionerState,
        token: u32,
    ) {
        surface.send_repositioned(token);
    }

    fn maximize_request(&mut self, surface: ToplevelSurface) {
        // Maximizar ainda não faz parte do desktop: a barra de título do
        // cliente ficaria fora da tela. O pedido é ignorado até existir um
        // modo de maximização próprio.
        if let Some(id) = self.window_id_for_surface(&surface) {
            info!(window_id = id, "pedido de maximizar ignorado");
        }
    }

    fn unmaximize_request(&mut self, surface: ToplevelSurface) {
        if let Some(id) = self.window_id_for_surface(&surface) {
            self.set_maximized(id, false);
        }
    }

    fn resize_request(
        &mut self,
        surface: ToplevelSurface,
        _seat: wl_seat::WlSeat,
        _serial: smithay::utils::Serial,
        edges: xdg_toplevel::ResizeEdge,
    ) {
        if let Some(id) = self.window_id_for_surface(&surface) {
            let pointer = self.pointer_screen;
            self.begin_resize(id, ResizeEdges::from_xdg(edges), pointer, true);
        }
    }

    fn fullscreen_request(
        &mut self,
        surface: ToplevelSurface,
        _output: Option<smithay::reexports::wayland_server::protocol::wl_output::WlOutput>,
    ) {
        if let Some(id) = self.window_id_for_surface(&surface) {
            info!(window_id = id, "cliente pediu tela cheia");
            self.set_fullscreen(id, true);
        }
    }

    fn unfullscreen_request(&mut self, surface: ToplevelSurface) {
        if let Some(id) = self.window_id_for_surface(&surface) {
            self.set_fullscreen(id, false);
        }
    }

    fn minimize_request(&mut self, surface: ToplevelSurface) {
        // Não há barra de tarefas para restaurar; minimizar esconderia a
        // janela sem caminho de volta. O pedido é registrado e ignorado.
        if let Some(id) = self.window_id_for_surface(&surface) {
            debug!(window_id = id, "minimizar ignorado (sem taskbar)");
        }
    }

    fn parent_changed(&mut self, surface: ToplevelSurface) {
        let parent_surface = with_states(surface.wl_surface(), |states| {
            states
                .data_map
                .get::<XdgToplevelSurfaceData>()
                .and_then(|data| data.lock().ok().and_then(|data| data.parent.clone()))
        });
        let parent_id = parent_surface
            .as_ref()
            .and_then(|parent| self.window_id_for_wl_surface(parent));

        let mut renumber = None;
        {
            let Some(managed) = self
                .windows
                .iter_mut()
                .find(|managed| managed.window.toplevel() == Some(&surface))
            else {
                return;
            };
            managed.parent_id = parent_id;
            managed.dialog = parent_id.is_some();
            // Janelas ainda sem buffer são finalizadas no primeiro commit, que
            // já centraliza o diálogo sobre o pai.
            managed.dialog_needs_center = managed.dialog && !managed.pending_id;
            managed
                .window
                .override_z_index(if managed.dialog { Z_DIALOG } else { Z_WINDOW });

            // Se o cliente só informou o pai depois do primeiro buffer, a
            // janela já consumiu um id de cartão. Move para a faixa de diálogo
            // para não desalinhar os cartões 1..4.
            if managed.dialog && !managed.pending_id && managed.id < DIALOG_ID_BASE {
                renumber = Some(managed.id);
            }
        }

        if let Some(old_id) = renumber {
            let new_id = self.next_dialog_id;
            self.next_dialog_id += 1;
            if let Some(managed) = self.windows.iter_mut().find(|w| w.id == old_id) {
                managed.id = new_id;
            }
            for managed in &mut self.windows {
                if managed.parent_id == Some(old_id) {
                    managed.parent_id = Some(new_id);
                }
            }
            if self.focused_window == Some(old_id) {
                self.focused_window = Some(new_id);
            }
            for id in &mut self.focus_order {
                if *id == old_id {
                    *id = new_id;
                }
            }
            if let Some(move_state) = self.move_state.as_mut() {
                if move_state.window_id == old_id {
                    move_state.window_id = new_id;
                }
            }
            if let Some(resize_state) = self.resize_state.as_mut() {
                if resize_state.window_id == old_id {
                    resize_state.window_id = new_id;
                }
            }
            info!(old_id, new_id, "diálogo renumerado após set_parent");
        }
    }

    fn popup_destroyed(&mut self, _surface: PopupSurface) {
        self.popup_manager.cleanup();
    }

    fn toplevel_destroyed(&mut self, surface: ToplevelSurface) {
        if let Some(shell) = self.shell_window.as_ref() {
            if shell.window.toplevel() == Some(&surface) {
                let shell = self.shell_window.take().expect("shell presente");
                self.space.unmap_elem(&shell.window);
                info!("janela do shell fechada");
                return;
            }
        }

        if let Some(index) = self
            .windows
            .iter()
            .position(|w| w.window.toplevel() == Some(&surface))
        {
            let managed = self.windows.remove(index);
            self.space.unmap_elem(&managed.window);
            self.unregister_foreign_toplevel(managed.id);
            self.stack.retain(|id| *id != managed.id);
            self.focus_order.retain(|id| *id != managed.id);

            // Diálogos órfãos deixam de ser transientes.
            for window in &mut self.windows {
                if window.parent_id == Some(managed.id) {
                    window.parent_id = None;
                    window.dialog = false;
                    window.dialog_needs_center = false;
                    window.window.override_z_index(Z_WINDOW);
                }
            }

            if self.focused_window == Some(managed.id) {
                self.focused_window = None;
                if let Some(next) = self.focus_order.last().copied() {
                    self.focus_window(next);
                }
            }
            info!(id = managed.id, "janela destruída");
            self.publish(IpcMessage::WindowClosed {
                window_id: managed.id,
            });
            self.popup_manager.cleanup();
            self.refresh_overview();
        }
    }
}

impl SelectionHandler for App {
    type SelectionUserData = ();
}

impl DataDeviceHandler for App {
    fn data_device_state(&self) -> &DataDeviceState {
        &self.data_device_state
    }
}

impl ClientDndGrabHandler for App {}

impl ServerDndGrabHandler for App {
    fn send(&mut self, _mime_type: String, _fd: std::os::unix::io::OwnedFd, _seat: Seat<Self>) {}
}

impl ShmHandler for App {
    fn shm_state(&self) -> &ShmState {
        &self.shm_state
    }
}

impl SeatHandler for App {
    type KeyboardFocus = WlSurface;
    type PointerFocus = WlSurface;
    type TouchFocus = WlSurface;

    fn seat_state(&mut self) -> &mut SeatState<Self> {
        &mut self.seat_state
    }

    fn focus_changed(&mut self, _seat: &Seat<Self>, _focused: Option<&WlSurface>) {}

    fn cursor_image(&mut self, _seat: &Seat<Self>, _image: CursorImageStatus) {}
}

impl smithay::wayland::output::OutputHandler for App {}

#[derive(Default)]
struct ClientState {
    compositor_state: CompositorClientState,
}

impl ClientData for ClientState {
    fn initialized(&self, _client_id: ClientId) {}
    fn disconnected(&self, _client_id: ClientId, _reason: DisconnectReason) {}
}

delegate_xdg_shell!(App);
smithay::delegate_xwayland_shell!(App);

impl XWaylandShellHandler for App {
    fn xwayland_shell_state(&mut self) -> &mut XWaylandShellState {
        &mut self.xwayland_shell_state
    }
}
delegate_compositor!(App);
delegate_shm!(App);
delegate_seat!(App);
delegate_data_device!(App);
delegate_output!(App);
delegate_layer_shell!(App);
delegate_foreign_toplevel_list!(App);
delegate_xdg_activation!(App);

impl WlrLayerShellHandler for App {
    fn shell_state(&mut self) -> &mut WlrLayerShellState {
        &mut self.layer_shell_state
    }

    fn new_layer_surface(
        &mut self,
        surface: smithay::wayland::shell::wlr_layer::LayerSurface,
        _output: Option<smithay::reexports::wayland_server::protocol::wl_output::WlOutput>,
        layer: WlrLayer,
        namespace: String,
    ) {
        let layer_surface = DesktopLayerSurface::new(surface, namespace.clone());
        let mut map = layer_map_for_output(&self.output);
        if let Err(error) = map.map_layer(&layer_surface) {
            warn!(?error, namespace, "falha ao mapear layer surface");
            return;
        }
        info!(?layer, namespace, "nova layer surface");
    }

    fn layer_destroyed(&mut self, surface: smithay::wayland::shell::wlr_layer::LayerSurface) {
        let mut map = layer_map_for_output(&self.output);
        let Some(layer) = map
            .layer_for_surface(surface.wl_surface(), WindowSurfaceType::TOPLEVEL)
            .cloned()
        else {
            return;
        };
        map.unmap_layer(&layer);
        if self.layer_focus.as_ref() == Some(&layer) {
            self.layer_focus = None;
        }
    }
}

impl ForeignToplevelListHandler for App {
    fn foreign_toplevel_list_state(&mut self) -> &mut ForeignToplevelListState {
        &mut self.foreign_toplevel_state
    }
}

impl XdgActivationHandler for App {
    fn activation_state(&mut self) -> &mut XdgActivationState {
        &mut self.activation_state
    }

    fn request_activation(
        &mut self,
        token: XdgActivationToken,
        token_data: XdgActivationTokenData,
        surface: WlSurface,
    ) {
        // Tokens velhos não ativam nada: evita roubo de foco por engano.
        if token_data.timestamp.elapsed() < Duration::from_secs(10) {
            if let Some(id) = self
                .windows
                .iter()
                .find(|managed| managed.window.wl_surface().as_deref() == Some(&surface))
                .map(|managed| managed.id)
            {
                info!(window_id = id, "ativação xdg solicitada");
                self.focus_window(id);
            }
        }
        self.activation_state.remove_token(&token);
    }
}

fn run_compositor(
    ipc: mpsc::Receiver<IpcMessage>,
    broadcast: broadcast::Sender<Broadcast>,
) -> Result<()> {
    let mut display: Display<App> = Display::new()?;
    let display_handle = display.handle();

    remove_wayland_socket();
    let listener = ListeningSocket::bind(WAYLAND_SOCKET_NAME)
        .with_context(|| format!("falha ao criar socket Wayland '{WAYLAND_SOCKET_NAME}'"))?;

    let mut state = App::new(&display_handle, ipc, broadcast)?;
    let mut clients = Vec::new();

    // XWayland é opcional: se não estiver instalado ou falhar, o desktop
    // continua funcionando apenas com clientes Wayland.
    let mut x11_loop =
        smithay::reexports::calloop::EventLoop::<App>::try_new().context("calloop")?;
    if env::var("SPATIAL_DECK_XWAYLAND").as_deref() != Ok("0") {
        start_xwayland(&display_handle, &x11_loop.handle());
    }

    let attributes = WinitWindow::default_attributes()
        .with_inner_size(LogicalSize::new(1280.0, 720.0))
        .with_min_inner_size(LogicalSize::new(720.0, 480.0))
        .with_title("Spatial Deck");
    let (mut backend, mut winit) = winit::init_from_attributes::<GlesRenderer>(attributes)
        .map_err(|error| anyhow::anyhow!("falha ao abrir backend winit: {error:?}"))?;

    let start_time = Instant::now();
    let mut should_exit = false;
    let mut pointer_location =
        Point::<f64, smithay::utils::Logical>::from((0.0, 0.0));

    loop {
        let keyboard = state.keyboard.clone();
        let pointer = state.pointer.clone();
        let status = winit.dispatch_new_events(|event| match event {
            WinitEvent::CloseRequested => {
                info!(
                    x = pointer_location.x,
                    y = pointer_location.y,
                    focused = ?state.focused_window,
                    windows = state.windows.len(),
                    "pedido de fechamento recebido do gerenciador de janelas"
                );
                // O KWin manda o pedido de fechamento para o deck (Alt+F4 ou
                // botão × da barra de título). Se houver uma janela interna em
                // foco, fecha essa janela em vez de encerrar o desktop.
                let target = state
                    .window_id_under(pointer_location)
                    .or(state.focused_window);
                if let Some(id) = target {
                    if state.windows.iter().any(|managed| managed.id == id) {
                        info!(window_id = id, "fechando janela interna via pedido de fechamento");
                        state.close_window(id);
                    } else {
                        should_exit = true;
                    }
                } else {
                    should_exit = true;
                }
            }
            WinitEvent::Resized { size, .. } => state.resize_output(size),
            WinitEvent::Input(InputEvent::Keyboard { event }) => {
                keyboard.input::<(), _>(
                    &mut state,
                    event.key_code(),
                    event.state(),
                    SERIAL_COUNTER.next_serial(),
                    0,
                    |state, modifiers, key| {
                        if event.state() != KeyState::Pressed {
                            return FilterResult::Forward;
                        }
                        let sym = key.modified_sym();
                        // Com Shift, `modified_sym` devolve a letra maiúscula
                        // (F, M, Q…). Normaliza para minúscula para que as
                        // variantes Ctrl+Shift dos atalhos também casem.
                        let sym = if (0x41..=0x5a).contains(&sym.raw()) {
                            Keysym::new(sym.raw() + 0x20)
                        } else {
                            sym
                        };
                        let ctrl_alt = modifiers.ctrl && modifiers.alt;
                        let ctrl_shift = modifiers.ctrl && modifiers.shift;

                        // Visão geral. Escape sai; Ctrl+Alt+O alterna (o
                        // Alt+Tab puro é capturado pelo WM hospedeiro).
                        if state.overview.is_some() && sym == Keysym::Escape {
                            info!("atalho de sair da visão geral");
                            state.exit_overview();
                            return FilterResult::Intercept(());
                        }
                        if (ctrl_alt && sym == Keysym::o) || (ctrl_shift && sym == Keysym::o) {
                            info!("atalho de visão geral");
                            state.toggle_overview();
                            return FilterResult::Intercept(());
                        }

                        // Zoom do canvas. Só age quando o ponteiro está sobre
                        // o deck (e não sobre uma janela real), para não
                        // roubar Ctrl+=/Ctrl+- dos aplicativos. Na visão geral
                        // as janelas são miniaturas, então o zoom é sempre
                        // permitido e sai do modo.
                        if modifiers.ctrl
                            && !modifiers.alt
                            && (state.overview.is_some()
                                || state.window_id_under(pointer_location).is_none())
                        {
                            let action = match sym {
                                Keysym::equal | Keysym::plus | Keysym::KP_Add => {
                                    Some(CanvasZoom::In)
                                }
                                Keysym::minus | Keysym::underscore | Keysym::KP_Subtract => {
                                    Some(CanvasZoom::Out)
                                }
                                Keysym::_0 | Keysym::KP_0 => Some(CanvasZoom::Reset),
                                Keysym::f if modifiers.shift => Some(CanvasZoom::Fit),
                                _ => None,
                            };
                            if let Some(action) = action {
                                info!(?action, ?sym, "atalho de zoom");
                                if state.overview.is_some() {
                                    state.exit_overview();
                                }
                                state.publish(IpcMessage::ZoomCanvas { action });
                                return FilterResult::Intercept(());
                            }
                        }

                        // Percorre a pilha de foco. Alt+Tab é o atalho
                        // canônico; as variantes com Ctrl existem porque, ao
                        // rodar aninhado, o WM da sessão hospedeira costuma
                        // capturar Alt+Tab antes de entregá-lo ao deck.
                        if (modifiers.alt && sym == Keysym::Tab)
                            || (ctrl_alt && sym == Keysym::Tab)
                            || (ctrl_shift && sym == Keysym::Tab)
                        {
                            info!(reverse = modifiers.shift, "atalho de foco");
                            state.cycle_focus(modifiers.shift);
                            return FilterResult::Intercept(());
                        }
                        // Fecha a janela focada (ou sob o ponteiro).
                        if (modifiers.alt && sym == Keysym::F4)
                            || (ctrl_alt && (sym == Keysym::w || sym == Keysym::F4))
                            || (ctrl_shift && sym == Keysym::q)
                        {
                            if let Some(id) = state.focused_or_pointer_window() {
                                info!(window_id = id, "atalho de fechar janela");
                                state.close_window(id);
                            }
                            return FilterResult::Intercept(());
                        }
                        // Restaura a janela maximizada. Maximizar pelo atalho
                        // ainda não é suportado.
                        if (modifiers.logo && sym == Keysym::m)
                            || (ctrl_alt && sym == Keysym::m)
                            || (ctrl_shift && sym == Keysym::m)
                        {
                            info!("atalho de restaurar");
                            state.restore_maximized_focused();
                            return FilterResult::Intercept(());
                        }
                        // Alterna tela cheia. Ctrl+Shift+F fica reservado para
                        // "Ajustar" o zoom do canvas.
                        if (modifiers.logo && sym == Keysym::f)
                            || (ctrl_alt && sym == Keysym::f)
                        {
                            info!("atalho de tela cheia");
                            state.toggle_fullscreen_focused();
                            return FilterResult::Intercept(());
                        }
                        // Alt/Super+Q fecha a janela sob o ponteiro (as
                        // janelas reais ficam por cima do cartão, então o
                        // botão × pode ficar coberto).
                        if ((modifiers.alt || modifiers.logo) && sym == Keysym::q)
                            || (ctrl_alt && sym == Keysym::q)
                        {
                            if let Some(id) = state
                                .window_id_under(pointer_location)
                                .or(state.focused_window)
                            {
                                info!(window_id = id, "atalho de fechar janela");
                                state.close_window(id);
                            }
                            return FilterResult::Intercept(());
                        }
                        FilterResult::Forward
                    },
                );
            }
            WinitEvent::Input(InputEvent::PointerMotionAbsolute { event }) => {
                let position = event.position_transformed(state.output_logical_size);
                pointer_location = position;
                state.pointer_screen = position;
                // Redimensionamento em andamento: aplica a nova geometria.
                state.update_resize(position);
                // Arrasto de janela em andamento: reposiciona no canvas.
                state.update_move(position);
                // O hit-test é feito no espaço de mundo (ou na grade do
                // overview), então o foco acompanha o zoom visual das janelas.
                let focus = state.pointer_focus(position);
                let motion_location = focus
                    .as_ref()
                    .map(|(_, _, motion)| *motion)
                    .unwrap_or(position);
                let focus = focus.map(|(surface, origin, _)| (surface, origin));
                pointer.motion(
                    &mut state,
                    focus,
                    &MotionEvent {
                        location: motion_location,
                        serial: SERIAL_COUNTER.next_serial(),
                        time: 0,
                    },
                );
                pointer.frame(&mut state);
            }
            WinitEvent::Input(InputEvent::PointerButton { event }) => {
                let left = event.button_code() == 0x110;

                if event.state() == ButtonState::Pressed {
                    // Clique numa layer acima das janelas: o foco de teclado
                    // vai para ela (on-demand) ou permanece (exclusive).
                    let layer_hit = state.layer_under_pointer(pointer_location);
                    if let Some(layer) = layer_hit.as_ref() {
                        if layer.cached_state().keyboard_interactivity
                            != KeyboardInteractivity::None
                        {
                            state.focus_layer_surface(layer);
                        }
                    }

                    // Na visão geral, o clique esquerdo escolhe a miniatura e
                    // o direito fecha a janela. Cliques no HUD/deck continuam
                    // indo para o shell (inclusive o botão que fecha a visão
                    // geral); cliques no canvas vazio saem do modo.
                    if state.overview.is_some() {
                        if left {
                            if let Some(id) = state.window_id_under(pointer_location) {
                                info!(window_id = id, "janela escolhida na visão geral");
                                state.exit_overview();
                                state.focus_window(id);
                                return;
                            }
                            if !state.is_shell_chrome(pointer_location) {
                                state.exit_overview();
                            }
                        } else if let Some(id) = state.window_id_under(pointer_location) {
                            info!(window_id = id, "fechando janela pela visão geral");
                            state.close_window(id);
                            return;
                        }
                    }

                    // Ferramenta do deck: arrastar sobre uma janela inicia o
                    // redimensionamento pelo canto mais próximo do clique.
                    if left && state.resize_tool {
                        if let Some(id) = state.window_id_under(pointer_location) {
                            if let Some(managed) =
                                state.windows.iter().find(|managed| managed.id == id)
                            {
                                let rect = state.window_screen_rect(managed);
                                let edges = resize_edges_from_point(rect, pointer_location);
                                info!(window_id = id, ?edges, "redimensionar pela ferramenta");
                                state.focus_window(id);
                                state.begin_resize(id, edges, pointer_location, false);
                                return;
                            }
                        }
                    }

                    // Super/Alt + botão esquerdo arrasta a janela sob o
                    // ponteiro, inclusive clientes sem decoração própria.
                    // Ctrl sozinho também move: ao rodar aninhado o WM da
                    // sessão hospedeira captura Alt/Super (e às vezes até
                    // Ctrl+Alt) e move a janela do deck inteira.
                    if left {
                        let modifiers = keyboard.modifier_state();
                        if modifiers.alt || modifiers.logo || modifiers.ctrl {
                            if let Some(id) = state.window_id_under(pointer_location) {
                                state.focus_window(id);
                                state.begin_move(id, pointer_location, false);
                                return;
                            }
                        }

                        // Clique na borda inicia redimensionamento antes de
                        // entregar o botão ao cliente.
                        if let Some((id, edges)) = state.resize_edges_under(pointer_location) {
                            state.focus_window(id);
                            state.begin_resize(id, edges, pointer_location, false);
                            return;
                        }
                    }

                    // Foco de teclado segue o clique: o shell recupera o foco
                    // ao clicar no canvas e o terminal o recebe ao ser clicado.
                    if let Some(id) = state.window_id_under(pointer_location) {
                        state.focus_window(id);
                    } else if layer_hit.is_none() {
                        let focused = state
                            .shell_pointer_focus(pointer_location)
                            .map(|(surface, _, _)| surface);
                        if let Some(surface) = focused {
                            keyboard.set_focus(
                                &mut state,
                                Some(surface),
                                SERIAL_COUNTER.next_serial(),
                            );
                        }
                        state.deactivate_windows();
                    }
                } else if left {
                    // Fim de um redimensionamento iniciado pelo compositor:
                    // o cliente nunca viu o botão pressionado, então engole o
                    // solto. Em `resize_request` o cliente já recebeu o
                    // pressionar e precisa receber o soltar normalmente.
                    if let Some(forward) = state
                        .resize_state
                        .as_ref()
                        .map(|resize_state| resize_state.forward_buttons)
                    {
                        state.end_resize();
                        if !forward {
                            return;
                        }
                    }

                    // Fim de um arrasto iniciado pelo compositor (Super/Alt):
                    // o cliente nunca viu o botão pressionado, então engole o
                    // solto. Em `move_request` o cliente já recebeu o pressionar
                    // e precisa receber o soltar normalmente.
                    let forward = state
                        .move_state
                        .as_ref()
                        .map(|move_state| move_state.forward_buttons);
                    if let Some(forward) = forward {
                        state.end_move();
                        if !forward {
                            return;
                        }
                    }
                }

                pointer.button(
                    &mut state,
                    &ButtonEvent {
                        serial: SERIAL_COUNTER.next_serial(),
                        time: 0,
                        button: event.button_code(),
                        state: event.state(),
                    },
                );
                pointer.frame(&mut state);
            }
            WinitEvent::Input(InputEvent::PointerAxis { event }) => {
                let mut frame = AxisFrame::new(0).source(event.source());
                for axis in [Axis::Horizontal, Axis::Vertical] {
                    let amount = event.amount(axis);
                    let v120 = event.amount_v120(axis);
                    if let Some(amount) = amount {
                        frame = frame.value(axis, amount);
                    } else if let Some(v120) = v120 {
                        // O winit no X11 informa apenas o passo discreto
                        // (`v120`), sem o valor contínuo. O `value` é o único
                        // campo obrigatório do protocolo: sem ele o cliente
                        // Wayland (WebKit) descarta a rolagem.
                        frame = frame.value(axis, v120 / 120.0 * WHEEL_STEP);
                    }
                    if let Some(v120) = v120 {
                        frame = frame.v120(axis, v120 as i32);
                    }
                }
                pointer.axis(&mut state, frame);
                pointer.frame(&mut state);
            }
            _ => {}
        });

        if should_exit || matches!(status, PumpStatus::Exit(_)) {
            info!(
                close_requested = should_exit,
                pump_exit = matches!(status, PumpStatus::Exit(_)),
                "encerrando compositor"
            );
            return Ok(());
        }

        if let Err(error) = x11_loop.dispatch(Some(Duration::ZERO), &mut state) {
            warn!(%error, "eventos do XWayland");
        }

        while let Ok(message) = state.ipc.try_recv() {
            state.handle_ipc(message);
        }

        if let Some(stream) = listener.accept()? {
            let client = display
                .handle()
                .insert_client(stream, Arc::new(ClientState::default()))
                .context("falha ao aceitar cliente Wayland")?;
            clients.push(client);
        }

        display.dispatch_clients(&mut state)?;
        display.flush_clients()?;

        let size = backend.window_size();
        state.resize_output(size);
        let damage = Rectangle::from_size(size);

        let minimalist_theme = matches!(
            state.theme,
            CanvasTheme::Minimalist | CanvasTheme::MinimalistDark
        );

        let minimalist_borders: Vec<Rectangle<i32, smithay::utils::Physical>> =
            if minimalist_theme {
                state
                    .windows
                    .iter()
                    .map(|managed| {
                        let rect = state.window_screen_rect(managed);
                        Rectangle::new(
                            Point::from((rect.loc.x, rect.loc.y)),
                            Size::from((rect.size.w, rect.size.h)),
                        )
                    })
                    .collect()
            } else {
                Vec::new()
            };

        state.flush_cluster_frame();

        {
            let (renderer, mut framebuffer) = backend.bind()?;
            // Coleta manual: o `Space` não escala superfícies, então cada
            // janela é envolvida num elemento que aplica o zoom do canvas.
            let mut shell_elements: Vec<ScaledSurfaceElement<GlesRenderer>> = Vec::new();
            let mut window_raw: Vec<ScaledSurfaceElement<GlesRenderer>> = Vec::new();
            for window in state.space.elements_for_output(&state.output).rev() {
                let is_shell = state
                    .shell_window
                    .as_ref()
                    .map(|shell| &shell.window == window)
                    .unwrap_or(false);
                let (location, zoom) = if is_shell {
                    (Point::from((0, 0)), 1.0)
                } else if let Some(managed) =
                    state.windows.iter().find(|managed| &managed.window == window)
                {
                    state.window_render_transform(managed)
                } else {
                    continue;
                };
                let target = if is_shell {
                    &mut shell_elements
                } else {
                    &mut window_raw
                };

                let location = location.to_physical_precise_round(1.0);
                if (zoom - 1.0).abs() < 1e-4 {
                    target.extend(
                        window
                            .render_elements::<WaylandSurfaceRenderElement<GlesRenderer>>(
                                renderer,
                                location,
                                smithay::utils::Scale::from(1.0),
                                1.0,
                            )
                            .into_iter()
                            .map(|element| ScaledSurfaceElement::new(element, location, 1.0)),
                    );
                } else {
                    target.extend(scaled_window_elements(renderer, window, location, zoom));
                }
            }

            // Layers de clientes externos (barras, OSDs, overlays) ficam fora
            // do canvas: posição e tamanho vêm do `LayerMap`, escala 1:1.
            let mut layer_bottom: Vec<WaylandSurfaceRenderElement<GlesRenderer>> = Vec::new();
            let mut layer_top: Vec<WaylandSurfaceRenderElement<GlesRenderer>> = Vec::new();
            {
                let map = layer_map_for_output(&state.output);
                for wl_layer in [WlrLayer::Background, WlrLayer::Bottom] {
                    for layer in map.layers_on(wl_layer) {
                        if let Some(geometry) = map.layer_geometry(layer) {
                            layer_bottom.extend(layer_surface_elements(renderer, layer, geometry.loc));
                        }
                    }
                }
                for wl_layer in [WlrLayer::Top, WlrLayer::Overlay] {
                    for layer in map.layers_on(wl_layer) {
                        if let Some(geometry) = map.layer_geometry(layer) {
                            layer_top.extend(layer_surface_elements(renderer, layer, geometry.loc));
                        }
                    }
                }
            }

            // Linha invisível sob a barra superior: as janelas são cortadas na
            // borda inferior do HUD, então nunca aparecem por baixo dele.
            let window_crop: smithay::utils::Rectangle<i32, smithay::utils::Physical> =
                match state.hud_bottom {
                    Some(y) => smithay::utils::Rectangle::new(
                        Point::from((0, y)),
                        smithay::utils::Size::from((size.w, (size.h - y).max(0))),
                    ),
                    None => smithay::utils::Rectangle::new(Point::from((0, 0)), size),
                };
            let window_elements: Vec<CropRenderElement<ScaledSurfaceElement<GlesRenderer>>> =
                window_raw
                    .into_iter()
                    .filter_map(|element| {
                        CropRenderElement::from_element(
                            element,
                            smithay::utils::Scale::from(1.0),
                            window_crop,
                        )
                    })
                    .collect();

            // Chrome do shell por cima das janelas: a superfície do shell é
            // recortada em cada área do HUD/deck/gaveta e desenhada por último.
            let mut chrome: Vec<CropRenderElement<WaylandSurfaceRenderElement<GlesRenderer>>> =
                Vec::new();
            if let Some(shell) = state.shell_window.as_ref() {
                for rect in state.chrome_rects.iter().copied() {
                    let crop: smithay::utils::Rectangle<i32, smithay::utils::Physical> =
                        smithay::utils::Rectangle::new(
                            Point::from((rect.loc.x, rect.loc.y)),
                            smithay::utils::Size::from((rect.size.w, rect.size.h)),
                        );
                    let shell_elements: Vec<WaylandSurfaceRenderElement<GlesRenderer>> = shell
                        .window
                        .render_elements(
                            renderer,
                            Point::from((0, 0)),
                            smithay::utils::Scale::from(1.0),
                            1.0,
                        );
                    chrome.extend(shell_elements.into_iter().filter_map(|element| {
                        CropRenderElement::from_element(
                            element,
                            smithay::utils::Scale::from(1.0),
                            crop,
                        )
                    }));
                }
            }

            let mut frame = renderer.render(&mut framebuffer, size, Transform::Flipped180)?;
            let clear_color = match state.theme {
                // Papel do tema Minimalista (#f7f7f2).
                CanvasTheme::Minimalist => Color32F::new(0.969, 0.969, 0.949, 1.0),
                // Fundo negro do Minimalista Negro.
                CanvasTheme::MinimalistDark => Color32F::new(0.0, 0.0, 0.0, 1.0),
                // Fundo Nord (nord0).
                CanvasTheme::Nord => Color32F::new(0.180, 0.204, 0.251, 1.0),
                _ => Color32F::new(0.027, 0.039, 0.067, 1.0),
            };
            frame.clear(clear_color, &[damage])?;
            draw_render_elements(&mut frame, 1.0, &shell_elements, &[damage])?;
            draw_render_elements(&mut frame, 1.0, &layer_bottom, &[damage])?;
            draw_render_elements(&mut frame, 1.0, &window_elements, &[damage])?;
            draw_render_elements(&mut frame, 1.0, &layer_top, &[damage])?;
            draw_render_elements(&mut frame, 1.0, &chrome, &[damage])?;

            if !minimalist_borders.is_empty() {
                let border_color = if state.theme == CanvasTheme::MinimalistDark {
                    minimalist_render::OUTLINE_COLOR_DARK
                } else {
                    minimalist_render::OUTLINE_COLOR
                };
                minimalist_render::draw_minimalist_borders(
                    &mut frame,
                    size,
                    minimalist_borders.iter().copied(),
                    border_color,
                )?;
            }

            let _ = frame.finish()?;

            // O shell também precisa de frame callbacks: o GTK/WebKit só
            // pinta o conteúdo da página depois de receber o próximo frame.
            if let Some(shell) = &state.shell_window {
                shell.window.send_frame(
                    &state.output,
                    start_time.elapsed(),
                    None,
                    |_, _| Some(state.output.clone()),
                );
            }
            for managed in &state.windows {
                managed.window.send_frame(
                    &state.output,
                    start_time.elapsed(),
                    None,
                    |_, _| Some(state.output.clone()),
                );
            }
            {
                let map = layer_map_for_output(&state.output);
                for layer in map.layers() {
                    layer.send_frame(
                        &state.output,
                        start_time.elapsed(),
                        None,
                        |_, _| Some(state.output.clone()),
                    );
                }
            }
        }

        backend.submit(Some(&[damage]))?;
    }
}

fn canvas_screen(
    canvas: CanvasTransform,
    world: Point<f64, smithay::utils::Logical>,
) -> Point<i32, smithay::utils::Logical> {
    Point::from((
        (world.x * canvas.scale + canvas.offset_x).round() as i32,
        (world.y * canvas.scale + canvas.offset_y).round() as i32,
    ))
}

fn canvas_world(
    canvas: CanvasTransform,
    screen: Point<f64, smithay::utils::Logical>,
) -> Point<f64, smithay::utils::Logical> {
    let scale = canvas.scale.max(0.01);
    Point::from((
        (screen.x - canvas.offset_x) / scale,
        (screen.y - canvas.offset_y) / scale,
    ))
}

/// Sobe o XWayland e, quando o servidor estiver pronto, o gerenciador X11.
/// O `DISPLAY` resultante é passado aos aplicativos iniciados pelo deck.
fn start_xwayland(
    display_handle: &DisplayHandle,
    handle: &smithay::reexports::calloop::LoopHandle<'static, App>,
) {
    let (xwayland, client) = match XWayland::spawn(
        display_handle,
        None,
        std::iter::empty::<(String, String)>(),
        true,
        Stdio::null(),
        Stdio::null(),
        |_| (),
    ) {
        Ok(spawned) => spawned,
        Err(error) => {
            warn!(%error, "XWayland indisponível");
            return;
        }
    };

    let loop_handle = handle.clone();
    let result = handle.insert_source(xwayland, move |event, _, state| match event {
        XWaylandEvent::Ready {
            x11_socket,
            display_number,
        } => match X11Wm::start_wm(loop_handle.clone(), x11_socket, client.clone()) {
            Ok(wm) => {
                state.xwm = Some(wm);
                state.xdisplay = Some(display_number);
                info!(display_number, "XWayland pronto");
            }
            Err(error) => warn!(%error, "falha ao iniciar o X11 WM"),
        },
        XWaylandEvent::Error => warn!("XWayland falhou ao iniciar"),
    });
    if let Err(error) = result {
        warn!(%error, "falha ao registrar o XWayland no event loop");
    }
}

fn remove_wayland_socket() {
    if let Ok(runtime_dir) = env::var("XDG_RUNTIME_DIR") {
        let path = Path::new(&runtime_dir).join(WAYLAND_SOCKET_NAME);
        let _ = fs::remove_file(&path);
        let lock = Path::new(&runtime_dir).join(format!("{WAYLAND_SOCKET_NAME}.lock"));
        let _ = fs::remove_file(lock);
    }
}

// ---------------------------------------------------------------------------
// IPC: socket Unix + WebSocket + HTTP estático
// ---------------------------------------------------------------------------

fn start_ipc_runtime(
    tx: mpsc::Sender<IpcMessage>,
    apps: Arc<Vec<DesktopApp>>,
) -> Result<broadcast::Sender<Broadcast>> {
    let web_root = env::var("SPATIAL_DECK_WEB_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .parent()
                .expect("compositor deve estar dentro do workspace")
                .join("web")
        });

    // Atualizações de posição saem do compositor para todos os clientes
    // WebSocket (ex.: quando a janela real é arrastada).
    let (broadcast_tx, _) = broadcast::channel::<Broadcast>(64);
    let broadcast_thread = broadcast_tx.clone();

    std::thread::Builder::new()
        .name("spatial-deck-ipc".into())
        .spawn(move || {
            let runtime = match tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
            {
                Ok(runtime) => runtime,
                Err(error) => {
                    eprintln!("falha ao criar runtime Tokio: {error}");
                    return;
                }
            };

            runtime.block_on(async move {
                if let Err(error) = run_ipc_servers(tx, apps, web_root, broadcast_thread).await {
                    eprintln!("servidor IPC encerrado: {error:#}");
                }
            });
        })
        .context("falha ao criar thread do runtime Tokio")?;

    Ok(broadcast_tx)
}

async fn run_ipc_servers(
    tx: mpsc::Sender<IpcMessage>,
    apps: Arc<Vec<DesktopApp>>,
    web_root: PathBuf,
    broadcast_tx: broadcast::Sender<Broadcast>,
) -> Result<()> {
    let _ = fs::remove_file(IPC_SOCKET);
    let unix = UnixListener::bind(IPC_SOCKET)
        .with_context(|| format!("falha ao abrir socket Unix {IPC_SOCKET}"))?;
    let _ = fs::set_permissions(IPC_SOCKET, fs::Permissions::from_mode(0o600));

    let ws = TcpListener::bind(WS_ADDR)
        .await
        .with_context(|| format!("falha ao abrir WebSocket em {WS_ADDR}"))?;
    let http = TcpListener::bind(HTTP_ADDR)
        .await
        .with_context(|| format!("falha ao abrir HTTP em {HTTP_ADDR}"))?;

    let apps_ws = apps.clone();
    let tx_ws = tx.clone();
    let broadcast_ws = broadcast_tx.clone();
    tokio::spawn(async move {
        loop {
            match ws.accept().await {
                Ok((stream, peer)) => {
                    let tx = tx_ws.clone();
                    let apps = apps_ws.clone();
                    let broadcast_tx = broadcast_ws.clone();
                    tokio::spawn(async move {
                        if let Err(error) = handle_websocket(stream, tx, apps, broadcast_tx).await {
                            debug!(%peer, "WebSocket encerrado: {error:#}");
                        }
                    });
                }
                Err(error) => warn!("erro ao aceitar WebSocket: {error}"),
            }
        }
    });

    let web_root = Arc::new(web_root);
    tokio::spawn(async move {
        loop {
            match http.accept().await {
                Ok((stream, _peer)) => {
                    let root = web_root.clone();
                    tokio::spawn(async move {
                        if let Err(error) = serve_http(stream, root).await {
                            debug!("HTTP encerrado: {error:#}");
                        }
                    });
                }
                Err(error) => warn!("erro ao aceitar HTTP: {error}"),
            }
        }
    });

    loop {
        let (stream, _peer) = unix.accept().await?;
        let tx = tx.clone();
        let apps = apps.clone();
        let broadcast_tx = broadcast_tx.clone();
        tokio::spawn(async move {
            if let Err(error) = handle_unix_client(stream, tx, apps, broadcast_tx).await {
                debug!("cliente Unix encerrado: {error:#}");
            }
        });
    }
}

async fn handle_unix_client(
    stream: tokio::net::UnixStream,
    tx: mpsc::Sender<IpcMessage>,
    apps: Arc<Vec<DesktopApp>>,
    broadcast_tx: broadcast::Sender<Broadcast>,
) -> Result<()> {
    let client_id = next_ipc_client_id();
    let (read_half, mut write_half) = stream.into_split();
    let mut broadcast_rx = broadcast_tx.subscribe();

    let hello = IpcMessage::AppList {
        apps: (*apps).clone(),
    }
    .to_json()?;
    write_half.write_all(hello.as_bytes()).await?;
    write_half.write_all(b"\n").await?;

    let mut lines = BufReader::new(read_half).lines();
    loop {
        tokio::select! {
            line = lines.next_line() => {
                let Some(line) = line? else {
                    break;
                };
                let line = line.trim();
                if line.is_empty() {
                    continue;
                }
                match IpcMessage::from_json(line) {
                    Ok(message) => {
                        // Repassa aos demais clientes (ex.: notificações do
                        // `shell-services` para o frontend) antes de entregar
                        // ao compositor. O remetente não recebe o próprio eco.
                        let _ = broadcast_tx.send((client_id, message.clone()));
                        if tx.send(message).is_err() {
                            break;
                        }
                    }
                    Err(error) => warn!("mensagem IPC Unix inválida: {error}"),
                }
            }
            message = broadcast_rx.recv() => {
                match message {
                    Ok((source, message)) if source != client_id => {
                        let json = message.to_json()?;
                        write_half.write_all(json.as_bytes()).await?;
                        write_half.write_all(b"\n").await?;
                    }
                    Ok(_) => {}
                    Err(broadcast::error::RecvError::Lagged(skipped)) => {
                        debug!(skipped, "atualizações IPC atrasadas no socket Unix");
                    }
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
        }
    }
    Ok(())
}

async fn handle_websocket(
    stream: TcpStream,
    tx: mpsc::Sender<IpcMessage>,
    apps: Arc<Vec<DesktopApp>>,
    broadcast_tx: broadcast::Sender<Broadcast>,
) -> Result<()> {
    let client_id = next_ipc_client_id();
    let websocket = accept_async(stream).await.context("handshake WebSocket")?;
    let (mut sink, mut source) = websocket.split();
    let mut broadcast_rx = broadcast_tx.subscribe();

    let hello = IpcMessage::AppList {
        apps: (*apps).clone(),
    }
    .to_json()?;
    sink.send(Message::Text(hello.into())).await?;

    loop {
        tokio::select! {
            frame = source.next() => {
                let Some(frame) = frame else {
                    break;
                };
                match frame? {
                    Message::Text(text) => match IpcMessage::from_json(text.as_str()) {
                        Ok(message) => {
                            // Mesmo barramento do socket Unix: o frontend
                            // também entrega mensagens ao `shell-services`.
                            let _ = broadcast_tx.send((client_id, message.clone()));
                            if tx.send(message).is_err() {
                                break;
                            }
                        }
                        Err(error) => warn!("mensagem IPC WebSocket inválida: {error}"),
                    },
                    Message::Close(_) => break,
                    Message::Ping(payload) => sink.send(Message::Pong(payload)).await?,
                    _ => {}
                }
            }
            message = broadcast_rx.recv() => {
                match message {
                    Ok((source, message)) if source != client_id => {
                        let json = message.to_json()?;
                        sink.send(Message::Text(json.into())).await?;
                    }
                    Ok(_) => {}
                    Err(broadcast::error::RecvError::Lagged(skipped)) => {
                        debug!(skipped, "atualizações IPC atrasadas");
                    }
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
        }
    }
    Ok(())
}

async fn serve_http(mut stream: TcpStream, web_root: Arc<PathBuf>) -> Result<()> {
    let mut buffer = vec![0_u8; 16 * 1024];
    let read = stream.read(&mut buffer).await?;
    if read == 0 {
        return Ok(());
    }

    let request = String::from_utf8_lossy(&buffer[..read]);
    let target = request
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .unwrap_or("/");
    let target = target.split('?').next().unwrap_or("/");

    if target.split('/').any(|part| part == "..") {
        return write_http_error(&mut stream, 400, "Bad Request").await;
    }

    if let Some(requested) = target.strip_prefix("/icon/") {
        let name = percent_decode(requested);
        return match safe_icon_name(&name).and_then(|name| resolve_icon_path(&name)) {
            Some(path) => write_http_file(&mut stream, &path, true).await,
            None => write_http_error(&mut stream, 404, "Not Found").await,
        };
    }

    let relative = if target == "/" {
        "rust/index.html".to_owned()
    } else {
        target.trim_start_matches('/').to_owned()
    };

    let mut path = web_root.join(&relative);
    if path.is_dir() {
        path.push("index.html");
    }

    if fs::metadata(&path).is_ok() {
        write_http_file(&mut stream, &path, false).await
    } else {
        write_http_error(&mut stream, 404, "Not Found").await
    }
}

async fn write_http_file(stream: &mut TcpStream, path: &Path, cacheable: bool) -> Result<()> {
    let Ok(body) = fs::read(path) else {
        return write_http_error(stream, 404, "Not Found").await;
    };
    let mime = mime_for(path);
    let cache = if cacheable {
        "public, max-age=86400"
    } else {
        "no-store"
    };
    let header = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: {mime}\r\nContent-Length: {}\r\nCache-Control: {cache}\r\nConnection: close\r\nAccess-Control-Allow-Origin: *\r\n\r\n",
        body.len()
    );
    stream.write_all(header.as_bytes()).await?;
    stream.write_all(&body).await?;
    stream.flush().await?;
    Ok(())
}

/// Decodifica `%XX` de um caminho de ícone, preservando bytes inválidos.
fn percent_decode(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut output = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' && index + 2 < bytes.len() {
            let hex = std::str::from_utf8(&bytes[index + 1..index + 3]).ok();
            if let Some(value) = hex.and_then(|hex| u8::from_str_radix(hex, 16).ok()) {
                output.push(value);
                index += 3;
                continue;
            }
        }
        output.push(bytes[index]);
        index += 1;
    }
    String::from_utf8_lossy(&output).into_owned()
}

/// Nome de ícone aceito pelo servidor HTTP. Sem barras nem traversal.
fn safe_icon_name(name: &str) -> Option<String> {
    let name = name.trim();
    if name.is_empty() || name.len() > 200 || name.contains('/') || name.contains('\\') {
        return None;
    }
    if name.contains("..") || name.split('/').any(|part| part == "..") {
        return None;
    }
    if !name
        .chars()
        .all(|character| character.is_alphanumeric() || "._-+".contains(character))
    {
        return None;
    }
    Some(name.to_owned())
}

/// Procura um ícone nos diretórios padrão do freedesktop.
fn resolve_icon_path(name: &str) -> Option<PathBuf> {
    let has_extension = Path::new(name)
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| matches!(extension, "svg" | "png"));
    let base = name.to_owned();

    let mut roots = Vec::new();
    if let Some(home) = dirs::home_dir() {
        roots.push(home.join(".local/share/icons"));
        roots.push(home.join(".icons"));
    }
    if let Ok(data_dirs) = env::var("XDG_DATA_DIRS") {
        for directory in data_dirs.split(':').filter(|part| !part.is_empty()) {
            roots.push(Path::new(directory).join("icons"));
        }
    }
    roots.push(PathBuf::from("/usr/share/icons"));

    let sizes = ["scalable", "512x512", "256x256", "128x128", "64x64", "48x48", "32x32", "24x24"];
    let extensions: &[&str] = if has_extension { &[""] } else { &["svg", "png"] };

    for root in &roots {
        for size in sizes {
            for extension in extensions {
                let file = if extension.is_empty() {
                    base.clone()
                } else {
                    format!("{base}.{extension}")
                };
                for directory in [root.join("hicolor").join(size).join("apps"), root.join(size).join("apps")] {
                    let path = directory.join(&file);
                    if path.is_file() {
                        return Some(path);
                    }
                }
            }
        }
        // Temas instalados (Adwaita, breeze, Papirus…).
        if let Ok(themes) = fs::read_dir(root) {
            for theme in themes.flatten() {
                if !theme.path().is_dir() {
                    continue;
                }
                for size in sizes {
                    for extension in extensions {
                        let file = if extension.is_empty() {
                            base.clone()
                        } else {
                            format!("{base}.{extension}")
                        };
                        let path = theme.path().join(size).join("apps").join(&file);
                        if path.is_file() {
                            return Some(path);
                        }
                    }
                }
            }
        }
    }

    for extension in extensions {
        let file = if extension.is_empty() {
            base.clone()
        } else {
            format!("{base}.{extension}")
        };
        let path = Path::new("/usr/share/pixmaps").join(file);
        if path.is_file() {
            return Some(path);
        }
    }
    None
}

async fn write_http_error(stream: &mut TcpStream, status: u16, reason: &str) -> Result<()> {
    let body = format!("{status} {reason}\n");
    let header = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: text/plain; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    stream.write_all(header.as_bytes()).await?;
    stream.write_all(body.as_bytes()).await?;
    stream.flush().await?;
    Ok(())
}

fn mime_for(path: &Path) -> &'static str {
    match path.extension().and_then(|extension| extension.to_str()) {
        Some("html") => "text/html; charset=utf-8",
        Some("css") => "text/css; charset=utf-8",
        Some("js") | Some("mjs") => "text/javascript; charset=utf-8",
        Some("json") => "application/json; charset=utf-8",
        Some("wasm") => "application/wasm",
        Some("svg") => "image/svg+xml",
        Some("png") => "image/png",
        Some("jpg") | Some("jpeg") => "image/jpeg",
        Some("ico") => "image/x-icon",
        Some("woff2") => "font/woff2",
        _ => "application/octet-stream",
    }
}

// ---------------------------------------------------------------------------
// Varredura de aplicativos .desktop
// ---------------------------------------------------------------------------

fn scan_desktop_apps() -> Vec<DesktopApp> {
    let mut directories = vec![PathBuf::from("/usr/share/applications")];

    if let Some(home) = dirs::home_dir() {
        directories.push(home.join(".local/share/applications"));
    }
    if let Ok(data_dirs) = env::var("XDG_DATA_DIRS") {
        for directory in data_dirs.split(':').filter(|part| !part.is_empty()) {
            directories.push(PathBuf::from(directory).join("applications"));
        }
    }
    if let Ok(data_home) = env::var("XDG_DATA_HOME") {
        directories.push(PathBuf::from(data_home).join("applications"));
    }

    let mut seen = HashSet::new();
    let mut apps = Vec::new();

    for directory in directories {
        let Ok(entries) = fs::read_dir(&directory) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|extension| extension.to_str()) != Some("desktop") {
                continue;
            }
            let Some(app) = parse_desktop_file(&path) else {
                continue;
            };
            let key = format!("{}\u{0}{}", app.name, app.exec);
            if seen.insert(key) {
                apps.push(app);
            }
        }
    }

    apps.sort_by_key(|app| app.name.to_lowercase());
    apps
}

fn parse_desktop_file(path: &Path) -> Option<DesktopApp> {
    let entry = parse_entry(path).ok()?;
    let section = entry.section("Desktop Entry")?;

    if section.attr("Type").first().map(String::as_str) != Some("Application") {
        return None;
    }
    if section
        .attr("NoDisplay")
        .first()
        .is_some_and(|value| value.eq_ignore_ascii_case("true"))
        || section
            .attr("Hidden")
            .first()
            .is_some_and(|value| value.eq_ignore_ascii_case("true"))
    {
        return None;
    }

    let name = section.attr("Name").first()?.trim().to_owned();
    if name.is_empty() {
        return None;
    }
    let exec = section
        .attr("Exec")
        .first()
        .map(|value| clean_exec(value))
        .unwrap_or_default();
    let icon = section
        .attr("Icon")
        .first()
        .cloned()
        .unwrap_or_else(|| "application-x-executable".to_owned());

    Some(DesktopApp { name, exec, icon })
}

fn clean_exec(exec: &str) -> String {
    let mut cleaned = exec.to_owned();
    for field_code in ["%U", "%u", "%F", "%f", "%i", "%c", "%k"] {
        cleaned = cleaned.replace(field_code, "");
    }
    cleaned.split_whitespace().collect::<Vec<_>>().join(" ")
}

// Mantém o `Write` importado para quem usa `std::io::Write` em ferramentas
// internas sem gerar aviso em builds futuros.
#[allow(dead_code)]
fn _assert_write_trait_is_used<W: std::io::Write>(_writer: &mut W) {}

#[cfg(test)]
mod resize_tests {
    use super::*;

    fn world(x: f64, y: f64) -> Point<f64, smithay::utils::Logical> {
        Point::from((x, y))
    }

    fn size(w: i32, h: i32) -> Size<i32, smithay::utils::Logical> {
        Size::from((w, h))
    }

    #[test]
    fn dragging_right_edge_grows_width_only() {
        let edges = ResizeEdges {
            right: true,
            ..ResizeEdges::default()
        };
        let (new_world, new_size) =
            resize_geometry(edges, world(100.0, 50.0), size(400, 300), 120.0, 0.0, 1.0);
        assert_eq!(new_world, world(100.0, 50.0));
        assert_eq!(new_size, size(520, 300));
    }

    #[test]
    fn dragging_left_edge_moves_origin_and_keeps_right_anchor() {
        let edges = ResizeEdges {
            left: true,
            ..ResizeEdges::default()
        };
        let start_world = world(100.0, 50.0);
        let start_size = size(400, 300);
        let (new_world, new_size) =
            resize_geometry(edges, start_world, start_size, 80.0, 0.0, 1.0);
        assert_eq!(new_world, world(180.0, 50.0));
        assert_eq!(new_size, size(320, 300));
        // A borda direita permanece fixa: (x + w) não muda.
        assert_eq!(new_world.x + new_size.w as f64, 500.0);
    }

    #[test]
    fn dragging_top_edge_moves_origin_and_keeps_bottom_anchor() {
        let edges = ResizeEdges {
            top: true,
            ..ResizeEdges::default()
        };
        let (new_world, new_size) =
            resize_geometry(edges, world(10.0, 20.0), size(300, 200), 0.0, 40.0, 1.0);
        assert_eq!(new_world, world(10.0, 60.0));
        assert_eq!(new_size, size(300, 160));
        assert_eq!(new_world.y + new_size.h as f64, 220.0);
    }

    #[test]
    fn bottom_right_corner_scales_both_axes() {
        let edges = ResizeEdges {
            bottom: true,
            right: true,
            ..ResizeEdges::default()
        };
        let (new_world, new_size) =
            resize_geometry(edges, world(0.0, 0.0), size(300, 200), 50.0, 25.0, 1.0);
        assert_eq!(new_world, world(0.0, 0.0));
        assert_eq!(new_size, size(350, 225));
    }

    #[test]
    fn minimum_size_keeps_opposite_edge_fixed() {
        let edges = ResizeEdges {
            left: true,
            top: true,
            ..ResizeEdges::default()
        };
        let start_world = world(100.0, 100.0);
        let start_size = size(400, 300);
        // Arrasta muito além do mínimo: 400 - 390 = 10 -> 240; 300 - 290 = 10 -> 160.
        let (new_world, new_size) =
            resize_geometry(edges, start_world, start_size, 390.0, 290.0, 1.0);
        assert_eq!(new_size, size(MIN_WINDOW_SIZE.0, MIN_WINDOW_SIZE.1));
        assert_eq!(
            new_world.x + new_size.w as f64,
            start_world.x + start_size.w as f64
        );
        assert_eq!(
            new_world.y + new_size.h as f64,
            start_world.y + start_size.h as f64
        );
    }

    #[test]
    fn screen_delta_is_divided_by_canvas_scale_for_world_origin() {
        let edges = ResizeEdges {
            left: true,
            ..ResizeEdges::default()
        };
        // Escala 2.0: 100 px de tela equivalem a 50 unidades de mundo.
        let (new_world, new_size) =
            resize_geometry(edges, world(10.0, 0.0), size(400, 300), 100.0, 0.0, 2.0);
        assert_eq!(new_world, world(60.0, 0.0));
        assert_eq!(new_size, size(300, 300));
    }

    #[test]
    fn zero_scale_is_clamped_to_avoid_division_by_zero() {
        let edges = ResizeEdges {
            left: true,
            ..ResizeEdges::default()
        };
        let (new_world, _) =
            resize_geometry(edges, world(0.0, 0.0), size(400, 300), 100.0, 0.0, 0.0);
        assert!(new_world.x.is_finite());
    }

    #[test]
    fn no_edges_leaves_geometry_untouched() {
        let (new_world, new_size) = resize_geometry(
            ResizeEdges::default(),
            world(7.0, 9.0),
            size(320, 240),
            50.0,
            -30.0,
            1.0,
        );
        assert_eq!(new_world, world(7.0, 9.0));
        assert_eq!(new_size, size(320, 240));
    }

    #[test]
    fn overview_without_windows_has_no_cells() {
        let cells = overview_cells(size(1920, 1080), &[]);
        assert!(cells.is_empty());
    }

    #[test]
    fn overview_cells_stay_inside_the_usable_area() {
        let viewport = size(1920, 1080);
        let windows: Vec<(u32, Size<i32, smithay::utils::Logical>)> = (1..=6)
            .map(|id| (id, size(640, 360)))
            .collect();
        let cells = overview_cells(viewport, &windows);
        assert_eq!(cells.len(), windows.len());

        let left = OVERVIEW_MARGIN_X;
        let right = viewport.w - OVERVIEW_MARGIN_X;
        let top = OVERVIEW_TOP;
        let bottom = viewport.h - OVERVIEW_BOTTOM;
        for (_, rect) in &cells {
            assert!(rect.loc.x >= left, "célula começa antes da margem esquerda");
            assert!(rect.loc.y >= top, "célula começa acima da área útil");
            assert!(
                rect.loc.x + rect.size.w <= right,
                "célula passa da margem direita"
            );
            assert!(
                rect.loc.y + rect.size.h <= bottom,
                "célula passa da borda inferior"
            );
        }
    }

    #[test]
    fn overview_cells_do_not_overlap() {
        let windows: Vec<(u32, Size<i32, smithay::utils::Logical>)> = (1..=5)
            .map(|id| (id, size(800, 600)))
            .collect();
        let cells = overview_cells(size(1600, 900), &windows);
        for (index, (_, a)) in cells.iter().enumerate() {
            for (_, b) in cells.iter().skip(index + 1) {
                let separated = a.loc.x + a.size.w <= b.loc.x
                    || b.loc.x + b.size.w <= a.loc.x
                    || a.loc.y + a.size.h <= b.loc.y
                    || b.loc.y + b.size.h <= a.loc.y;
                assert!(separated, "miniaturas se sobrepõem: {a:?} e {b:?}");
            }
        }
    }

    #[test]
    fn resize_tool_picks_nearest_corner() {
        let rect = Rectangle::new(Point::from((100, 50)), size(400, 300));
        let cases = [
            (world(150.0, 100.0), (true, false, true, false)),
            (world(350.0, 100.0), (true, false, false, true)),
            (world(150.0, 250.0), (false, true, true, false)),
            (world(350.0, 250.0), (false, true, false, true)),
        ];
        for (point, expected) in cases {
            let edges = resize_edges_from_point(rect, point);
            assert_eq!(
                (edges.top, edges.bottom, edges.left, edges.right),
                expected
            );
        }
    }

    #[test]
    fn x11_resize_edges_map_to_compositor_edges() {
        for (edge, expected) in [
            (X11ResizeEdge::Top, (true, false, false, false)),
            (X11ResizeEdge::Bottom, (false, true, false, false)),
            (X11ResizeEdge::TopLeft, (true, false, true, false)),
            (X11ResizeEdge::BottomRight, (false, true, false, true)),
        ] {
            let edges = ResizeEdges::from_x11(edge);
            assert_eq!(
                (edges.top, edges.bottom, edges.left, edges.right),
                expected
            );
        }
    }

    #[test]
    fn hit_rect_includes_csd_extents_above_and_left() {
        // Alacritty com CSD: a barra de título fica fora do conteúdo.
        let rect = App::window_hit_rect(
            world(218.0, 88.0),
            size(900, 600),
            Rectangle::new(Point::from((-44, -79)), size(988, 688)),
        );
        assert_eq!(rect.loc, world(174.0, 9.0));
        assert_eq!(rect.size, size(988, 688).to_f64());
        assert!(rect.contains(world(218.0, 20.0)), "barra de título deve receber clique");
    }

    #[test]
    fn hit_rect_falls_back_to_content_size_without_bbox() {
        let rect = App::window_hit_rect(
            world(10.0, 20.0),
            size(400, 300),
            Rectangle::new(Point::from((0, 0)), size(0, 0)),
        );
        assert_eq!(rect.loc, world(10.0, 20.0));
        assert_eq!(rect.size, size(400, 300).to_f64());
    }

    #[test]
    fn hud_hides_pointer_area_above_its_bottom_line() {
        assert!(App::hidden_under_hud(world(100.0, 59.0), Some(60)));
        assert!(!App::hidden_under_hud(world(100.0, 60.0), Some(60)));
        assert!(!App::hidden_under_hud(world(100.0, 59.0), None));
    }

    #[test]
    fn icon_names_reject_traversal_and_control_characters() {
        assert_eq!(
            safe_icon_name("org.gnome.Software").as_deref(),
            Some("org.gnome.Software")
        );
        assert_eq!(safe_icon_name("firefox").as_deref(), Some("firefox"));
        assert!(safe_icon_name("../../etc/passwd").is_none());
        assert!(safe_icon_name("a/b").is_none());
        assert!(safe_icon_name("icone\nquebrado").is_none());
        assert!(safe_icon_name("").is_none());
    }

    #[test]
    fn icon_urls_are_percent_decoded() {
        assert_eq!(percent_decode("org.gnome.Terminal"), "org.gnome.Terminal");
        assert_eq!(percent_decode("a%2Eb%20c"), "a.b c");
        assert_eq!(percent_decode("sem%ZZescape"), "sem%ZZescape");
    }

    #[test]
    fn thumbnail_preserves_aspect_ratio_and_centers() {
        let cell = Rectangle::new(Point::from((10, 20)), size(400, 400));
        let rect = fit_thumbnail(size(800, 400), cell);
        assert_eq!(rect.size, size(400, 200));
        assert_eq!(rect.loc, Point::from((10, 120)));

        // Janela alta: a altura limita a escala e sobra espaço nas laterais.
        let rect = fit_thumbnail(size(200, 400), cell);
        assert_eq!(rect.size, size(200, 400));
        assert_eq!(rect.loc, Point::from((110, 20)));
    }
}
