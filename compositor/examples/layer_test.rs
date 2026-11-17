//! Cliente mínimo `wlr-layer-shell` para validar o suporte do compositor.
//!
//! Roda aninhado no Spatial Deck (`WAYLAND_DISPLAY=spatial-deck`), cria uma
//! superfície na camada `top`, ancorada no canto superior esquerdo, e pinta um
//! retângulo sólido via `wl_shm`. Serve como teste de aceite da Etapa 1:
//!
//! ```sh
//! WAYLAND_DISPLAY=spatial-deck cargo run -p compositor --example layer-test
//! ```
//!
//! Imprime `CONFIGURED <largura> <altura>` quando o configure chega e fica vivo
//! por `LAYER_TEST_SECONDS` (padrão 20) segundos.

use std::{
    fs::{File, OpenOptions},
    io::Write,
    os::fd::AsFd,
    time::{Duration, Instant},
};

use wayland_client::{
    globals::{registry_queue_init, GlobalListContents},
    protocol::{wl_buffer, wl_compositor, wl_registry, wl_shm, wl_shm_pool, wl_surface},
    Connection, Dispatch, Proxy, QueueHandle,
};
use wayland_protocols::ext::foreign_toplevel_list::v1::client::{
    ext_foreign_toplevel_handle_v1, ext_foreign_toplevel_list_v1,
};
use wayland_protocols_wlr::layer_shell::v1::client::{
    zwlr_layer_shell_v1, zwlr_layer_surface_v1,
};

struct State {
    shm: wl_shm::WlShm,
    surface: wl_surface::WlSurface,
    pool: Option<wl_shm_pool::WlShmPool>,
    buffer: Option<wl_buffer::WlBuffer>,
    /// Mantém o fd do shm aberto enquanto o compositor importa o buffer.
    file: Option<File>,
    configured: bool,
}

fn solid_pixels(width: i32, height: i32) -> Vec<u8> {
    // ARGB8888 pouco-endian: azul opaco do tema padrão.
    let mut bytes = Vec::with_capacity((width * height * 4) as usize);
    for _ in 0..(width * height) {
        bytes.extend_from_slice(&[0xd9, 0x90, 0x4a, 0xff]);
    }
    bytes
}

fn draw(state: &mut State, qh: &QueueHandle<State>, width: i32, height: i32) {
    if width <= 0 || height <= 0 {
        return;
    }
    let stride = width * 4;
    let size = (stride * height) as usize;

    let path = std::env::temp_dir().join(format!("layer-test-{}.bin", std::process::id()));
    // O compositor faz mmap(PROT_READ) do fd: precisa de leitura + escrita.
    let mut file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(true)
        .open(&path)
        .expect("criar buffer");
    file.write_all(&solid_pixels(width, height))
        .expect("preencher buffer");

    let pool = state.shm.create_pool(file.as_fd(), size as i32, qh, ());
    let buffer = pool.create_buffer(
        0,
        width,
        height,
        stride,
        wl_shm::Format::Argb8888,
        qh,
        (),
    );
    state.surface.attach(Some(&buffer), 0, 0);
    state.surface.damage(0, 0, width, height);
    state.surface.commit();

    state.pool = Some(pool);
    state.buffer = Some(buffer);
    // O fd precisa continuar aberto: o compositor importa o pool de forma
    // assíncrona, depois que a requisição é enviada no próximo flush.
    state.file = Some(file);
    let _ = std::fs::remove_file(path);
}

impl Dispatch<wl_registry::WlRegistry, GlobalListContents> for State {
    fn event(
        _state: &mut Self,
        _proxy: &wl_registry::WlRegistry,
        _event: wl_registry::Event,
        _data: &GlobalListContents,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<wl_compositor::WlCompositor, ()> for State {
    fn event(
        _state: &mut Self,
        _proxy: &wl_compositor::WlCompositor,
        _event: wl_compositor::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<wl_shm::WlShm, ()> for State {
    fn event(
        _state: &mut Self,
        _proxy: &wl_shm::WlShm,
        _event: wl_shm::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<wl_shm_pool::WlShmPool, ()> for State {
    fn event(
        _state: &mut Self,
        _proxy: &wl_shm_pool::WlShmPool,
        _event: wl_shm_pool::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<wl_buffer::WlBuffer, ()> for State {
    fn event(
        _state: &mut Self,
        _proxy: &wl_buffer::WlBuffer,
        _event: wl_buffer::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<wl_surface::WlSurface, ()> for State {
    fn event(
        _state: &mut Self,
        _proxy: &wl_surface::WlSurface,
        _event: wl_surface::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<zwlr_layer_shell_v1::ZwlrLayerShellV1, ()> for State {
    fn event(
        _state: &mut Self,
        _proxy: &zwlr_layer_shell_v1::ZwlrLayerShellV1,
        _event: zwlr_layer_shell_v1::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<zwlr_layer_surface_v1::ZwlrLayerSurfaceV1, ()> for State {
    fn event(
        state: &mut Self,
        proxy: &zwlr_layer_surface_v1::ZwlrLayerSurfaceV1,
        event: zwlr_layer_surface_v1::Event,
        _data: &(),
        _conn: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        match event {
            zwlr_layer_surface_v1::Event::Configure {
                serial,
                width,
                height,
            } => {
                proxy.ack_configure(serial);
                let (width, height) = (width as i32, height as i32);
                draw(state, qh, width, height);
                state.configured = true;
                // O teste lê esta linha para saber que o protocolo funcionou.
                println!("CONFIGURED {width} {height}");
            }
            zwlr_layer_surface_v1::Event::Closed => {
                println!("CLOSED");
                std::process::exit(0);
            }
            _ => {}
        }
    }
}

/// Modo `--toplevels`: lista as janelas expostas por
/// `ext-foreign-toplevel-list-v1` e imprime `TOPLEVEL <app_id> <título>`.
struct ListState {
    entries: std::collections::HashMap<u32, (String, String)>,
    done: usize,
}

impl ListState {
    fn print(&self, id: u32) {
        if let Some((app_id, title)) = self.entries.get(&id) {
            println!("TOPLEVEL app_id={app_id:?} title={title:?}");
        }
    }
}

impl Dispatch<wl_registry::WlRegistry, GlobalListContents> for ListState {
    fn event(
        _state: &mut Self,
        _proxy: &wl_registry::WlRegistry,
        _event: wl_registry::Event,
        _data: &GlobalListContents,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<ext_foreign_toplevel_list_v1::ExtForeignToplevelListV1, ()> for ListState {
    fn event(
        state: &mut Self,
        _proxy: &ext_foreign_toplevel_list_v1::ExtForeignToplevelListV1,
        event: ext_foreign_toplevel_list_v1::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        if let ext_foreign_toplevel_list_v1::Event::Toplevel { toplevel } = event {
            state
                .entries
                .insert(toplevel.id().protocol_id(), (String::new(), String::new()));
        }
    }

    wayland_client::event_created_child!(ListState, ext_foreign_toplevel_list_v1::ExtForeignToplevelListV1, [
        ext_foreign_toplevel_list_v1::EVT_TOPLEVEL_OPCODE =>
            (ext_foreign_toplevel_handle_v1::ExtForeignToplevelHandleV1, ())
    ]);
}

impl Dispatch<ext_foreign_toplevel_handle_v1::ExtForeignToplevelHandleV1, ()> for ListState {
    fn event(
        state: &mut Self,
        _proxy: &ext_foreign_toplevel_handle_v1::ExtForeignToplevelHandleV1,
        event: ext_foreign_toplevel_handle_v1::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        use ext_foreign_toplevel_handle_v1::Event;
        let id = _proxy.id().protocol_id();
        match event {
            Event::Title { title } => {
                if let Some(entry) = state.entries.get_mut(&id) {
                    entry.1 = title;
                }
            }
            Event::AppId { app_id } => {
                if let Some(entry) = state.entries.get_mut(&id) {
                    entry.0 = app_id;
                }
            }
            Event::Done => {
                state.done += 1;
                state.print(id);
            }
            Event::Closed => {
                state.entries.remove(&id);
            }
            _ => {}
        }
    }
}

fn list_toplevels() {
    let conn = Connection::connect_to_env().expect("conectar no WAYLAND_DISPLAY");
    let (globals, mut queue) = registry_queue_init::<ListState>(&conn).expect("registry");
    let qh = queue.handle();
    let list: ext_foreign_toplevel_list_v1::ExtForeignToplevelListV1 = globals
        .bind(&qh, 1..=1, ())
        .expect("ext_foreign_toplevel_list_v1");

    let mut state = ListState {
        entries: std::collections::HashMap::new(),
        done: 0,
    };

    // Duas rodadas: a primeira recebe os handles já existentes, a segunda
    // espera os eventos title/app_id/done de cada um.
    queue.roundtrip(&mut state).expect("roundtrip 1");
    queue.roundtrip(&mut state).expect("roundtrip 2");

    let total = state.entries.len();
    println!("TOPLEVELS {total}");
    list.stop();
    let _ = conn.flush();
    if total == 0 {
        eprintln!("nenhum toplevel exposto");
        std::process::exit(2);
    }
}

fn main() {
    if std::env::args().any(|arg| arg == "--toplevels") {
        list_toplevels();
        return;
    }

    let seconds: u64 = std::env::var("LAYER_TEST_SECONDS")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(20);

    let conn = Connection::connect_to_env().expect("conectar no WAYLAND_DISPLAY");
    let (globals, mut queue) = registry_queue_init::<State>(&conn).expect("registry");
    let qh = queue.handle();

    let compositor: wl_compositor::WlCompositor = globals
        .bind(&qh, 4..=6, ())
        .expect("wl_compositor");
    let shm: wl_shm::WlShm = globals.bind(&qh, 1..=1, ()).expect("wl_shm");
    let layer_shell: zwlr_layer_shell_v1::ZwlrLayerShellV1 = globals
        .bind(&qh, 1..=4, ())
        .expect("zwlr_layer_shell_v1");

    let surface = compositor.create_surface(&qh, ());
    let layer_surface = layer_shell.get_layer_surface(
        &surface,
        None,
        zwlr_layer_shell_v1::Layer::Top,
        "layer-test".to_string(),
        &qh,
        (),
    );
    layer_surface.set_size(260, 120);
    layer_surface.set_anchor(
        zwlr_layer_surface_v1::Anchor::Top | zwlr_layer_surface_v1::Anchor::Left,
    );
    // top, right, bottom, left: abaixo do HUD do shell.
    layer_surface.set_margin(100, 0, 0, 24);
    layer_surface.set_keyboard_interactivity(
        zwlr_layer_surface_v1::KeyboardInteractivity::OnDemand,
    );
    surface.commit();

    let mut state = State {
        shm,
        surface,
        pool: None,
        buffer: None,
        file: None,
        configured: false,
    };

    let deadline = Instant::now() + Duration::from_secs(seconds);
    while Instant::now() < deadline {
        // `roundtrip` devolve o controle mesmo sem eventos e propaga erros de
        // protocolo; `blocking_dispatch` poderia ficar preso no fim do teste.
        queue.roundtrip(&mut state).expect("roundtrip do event queue");
        std::thread::sleep(Duration::from_millis(100));
    }

    if state.configured {
        println!("OK");
    } else {
        eprintln!("configure não chegou em {seconds}s");
        std::process::exit(2);
    }
}
