//! Ferramentas de desenho do canvas infinito.
//!
//! A camada de desenho é um `<svg>` transparente dentro de `#viewport`.
//! O grupo `#draw-world` recebe a mesma transformação de pan/zoom de
//! `#world`, então formas, textos e imagens vivem em coordenadas de mundo.
//!
//! Formas são elementos SVG simples (`path`, `line`, `rect`, `ellipse`,
//! `text` e `image`). A seleção é desenhada num grupo separado, em
//! coordenadas de tela, para manter o contorno nítido em qualquer zoom.

use std::cell::RefCell;

use wasm_bindgen::{closure::Closure, prelude::*, JsCast};
use web_sys::{
    Blob, CanvasRenderingContext2d, Document, DomRect, Element, Event, FileReader, HtmlCanvasElement,
    HtmlImageElement, HtmlInputElement, HtmlTextAreaElement, KeyboardEvent, MouseEvent,
    PointerEvent,
};

use crate::web::show_toast;

const SVG_NS: &str = "http://www.w3.org/2000/svg";
const DEFAULT_COLOR: &str = "#5eead4";
const DEFAULT_SIZE: f64 = 3.0;
const MAX_HISTORY: usize = 60;
const IMAGE_W: f64 = 480.0;
const IMAGE_H: f64 = 360.0;
/// Maior lado aceito depois da recompressão (px).
const IMAGE_MAX_DIM: f64 = 1600.0;
/// Imagens nativas abaixo disso entram sem recompressão (preserva alfa).
const IMAGE_RAW_LIMIT: usize = 600_000;
/// Teto do data URL recompressado; acima disso tenta de novo menor.
const IMAGE_MAX_BYTES: usize = 1_600_000;
/// Arquivos maiores que isso são recusados antes da leitura.
const IMAGE_FILE_LIMIT: f64 = 40_000_000.0;
const DEFAULT_FONT: &str =
    "ui-sans-serif, system-ui, -apple-system, \"Segoe UI\", Roboto, Arial, sans-serif";
const LINE_RATIO: f64 = 1.25;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tool {
    Select,
    Pan,
    Pen,
    Line,
    Rect,
    Ellipse,
    Text,
    Eraser,
    Image,
}

impl Tool {
    fn as_str(self) -> &'static str {
        match self {
            Self::Select => "select",
            Self::Pan => "pan",
            Self::Pen => "pen",
            Self::Line => "line",
            Self::Rect => "rect",
            Self::Ellipse => "ellipse",
            Self::Text => "text",
            Self::Eraser => "eraser",
            Self::Image => "image",
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct View {
    scale: f64,
    offset_x: f64,
    offset_y: f64,
    width: f64,
    height: f64,
}

impl Default for View {
    fn default() -> Self {
        Self {
            scale: 1.0,
            offset_x: 0.0,
            offset_y: 0.0,
            width: 1280.0,
            height: 720.0,
        }
    }
}

struct SelectedShape {
    id: String,
    base_x: f64,
    base_y: f64,
}

enum Gesture {
    Pen {
        element: Element,
        points: Vec<(f64, f64)>,
    },
    Line {
        element: Element,
    },
    Rect {
        element: Element,
        from: (f64, f64),
    },
    Ellipse {
        element: Element,
        from: (f64, f64),
    },
    Move {
        from: (f64, f64),
        items: Vec<SelectedShape>,
        dirty: bool,
    },
    Erase {
        dirty: bool,
    },
}

struct TextEditor {
    target: Element,
    world_x: f64,
    world_y: f64,
    font_size: f64,
    is_new: bool,
    original_html: String,
}

struct DrawingState {
    tool: Tool,
    color: String,
    size: f64,
    view: View,
    selected: Vec<String>,
    gesture: Option<Gesture>,
    next_id: u64,
    history: Vec<String>,
    history_index: usize,
    text_editor: Option<TextEditor>,
    pending_image_point: Option<(f64, f64)>,
    grid_visible: bool,
    /// Data URL de cada imagem, por id. O histórico guarda só o id, não o
    /// base64 — senão cada snapshot duplicaria todos os bytes.
    images: std::collections::HashMap<String, String>,
    next_image_id: u64,
}

impl Default for DrawingState {
    fn default() -> Self {
        Self {
            tool: Tool::Pen,
            color: DEFAULT_COLOR.to_owned(),
            size: DEFAULT_SIZE,
            view: View::default(),
            selected: Vec::new(),
            gesture: None,
            next_id: 1,
            history: Vec::new(),
            history_index: 0,
            text_editor: None,
            pending_image_point: None,
            grid_visible: true,
            images: std::collections::HashMap::new(),
            next_image_id: 1,
        }
    }
}

thread_local! {
    static STATE: RefCell<DrawingState> = RefCell::new(DrawingState::default());
}

// ---------------------------------------------------------------------------
// Init
// ---------------------------------------------------------------------------

pub fn init(document: &Document) -> Result<(), JsValue> {
    setup_pointer_events(document)?;
    setup_tool_buttons(document)?;
    setup_style_controls(document)?;
    setup_image_input(document)?;
    setup_text_editor(document)?;
    setup_keyboard(document)?;
    set_tool(document, Tool::Pen)?;
    commit_history(document)?;
    Ok(())
}

fn setup_pointer_events(document: &Document) -> Result<(), JsValue> {
    let layer = by_id(document, "draw-layer")?;

    let doc = document.clone();
    let down = Closure::<dyn FnMut(PointerEvent)>::new(move |event: PointerEvent| {
        let _ = on_pointer_down(&doc, &event);
    });
    layer.add_event_listener_with_callback("pointerdown", down.as_ref().unchecked_ref())?;
    down.forget();

    let doc = document.clone();
    let moved = Closure::<dyn FnMut(PointerEvent)>::new(move |event: PointerEvent| {
        let _ = on_pointer_move(&doc, &event);
    });
    layer.add_event_listener_with_callback("pointermove", moved.as_ref().unchecked_ref())?;
    moved.forget();

    let doc = document.clone();
    let up = Closure::<dyn FnMut(PointerEvent)>::new(move |event: PointerEvent| {
        let _ = on_pointer_up(&doc, &event);
    });
    layer.add_event_listener_with_callback("pointerup", up.as_ref().unchecked_ref())?;
    up.forget();

    let doc = document.clone();
    let cancel = Closure::<dyn FnMut(PointerEvent)>::new(move |event: PointerEvent| {
        let _ = on_pointer_cancel(&doc, &event);
    });
    layer.add_event_listener_with_callback("pointercancel", cancel.as_ref().unchecked_ref())?;
    cancel.forget();

    let doc = document.clone();
    let dbl = Closure::<dyn FnMut(MouseEvent)>::new(move |event: MouseEvent| {
        let _ = on_double_click(&doc, &event);
    });
    layer.add_event_listener_with_callback("dblclick", dbl.as_ref().unchecked_ref())?;
    dbl.forget();

    Ok(())
}

fn setup_tool_buttons(document: &Document) -> Result<(), JsValue> {
    for tool in [
        Tool::Select,
        Tool::Pan,
        Tool::Pen,
        Tool::Line,
        Tool::Rect,
        Tool::Ellipse,
        Tool::Text,
        Tool::Eraser,
    ] {
        let Ok(button) = by_id(document, &format!("tool-{}", tool.as_str())) else {
            continue;
        };
        let doc = document.clone();
        let callback = Closure::<dyn FnMut()>::new(move || {
            let _ = set_tool(&doc, tool);
        });
        button.add_event_listener_with_callback("click", callback.as_ref().unchecked_ref())?;
        callback.forget();
    }

    if let Ok(button) = by_id(document, "tool-image") {
        let doc = document.clone();
        let callback = Closure::<dyn FnMut()>::new(move || {
            let _ = set_tool(&doc, Tool::Image);
            show_toast(&doc, "Clique no canvas para posicionar a imagem.");
        });
        button.add_event_listener_with_callback("click", callback.as_ref().unchecked_ref())?;
        callback.forget();
    }

    attach_action(document, "action-undo", |document| undo(document))?;
    attach_action(document, "action-redo", |document| redo(document))?;
    attach_action(document, "action-clear", |document| clear_canvas(document))?;
    attach_action(document, "action-grid", |document| toggle_grid(document))?;
    Ok(())
}

fn attach_action<F>(document: &Document, id: &str, action: F) -> Result<(), JsValue>
where
    F: Fn(&Document) -> Result<(), JsValue> + 'static,
{
    let Ok(button) = by_id(document, id) else {
        return Ok(());
    };
    let doc = document.clone();
    let callback = Closure::<dyn FnMut()>::new(move || {
        let _ = action(&doc);
    });
    button.add_event_listener_with_callback("click", callback.as_ref().unchecked_ref())?;
    callback.forget();
    Ok(())
}

fn setup_style_controls(document: &Document) -> Result<(), JsValue> {
    if let Ok(input) = by_id(document, "stroke-color") {
        let input: HtmlInputElement = input.dyn_into()?;
        let doc = document.clone();
        let input_for_input = input.clone();
        let on_input = Closure::<dyn FnMut(Event)>::new(move |_event: Event| {
            let color = input_for_input.value();
            STATE.with(|state| state.borrow_mut().color = color.clone());
            let _ = update_swatch(&doc, &color);
            let _ = apply_style_to_selection(&doc);
        });
        input.add_event_listener_with_callback("input", on_input.as_ref().unchecked_ref())?;
        on_input.forget();

        let doc = document.clone();
        let on_change = Closure::<dyn FnMut(Event)>::new(move |_event: Event| {
            let _ = apply_style_to_selection(&doc);
            let _ = commit_history(&doc);
        });
        input.add_event_listener_with_callback("change", on_change.as_ref().unchecked_ref())?;
        on_change.forget();
    }

    if let Ok(input) = by_id(document, "stroke-size") {
        let input: HtmlInputElement = input.dyn_into()?;
        let doc = document.clone();
        let input_for_input = input.clone();
        let on_input = Closure::<dyn FnMut(Event)>::new(move |_event: Event| {
            let size = input_for_input.value().parse::<f64>().unwrap_or(DEFAULT_SIZE);
            STATE.with(|state| state.borrow_mut().size = size);
            if let Ok(output) = by_id(&doc, "stroke-size-out") {
                let _ = output.set_inner_html(&format!("{size:.0}"));
            }
            let _ = apply_style_to_selection(&doc);
        });
        input.add_event_listener_with_callback("input", on_input.as_ref().unchecked_ref())?;
        on_input.forget();

        let doc = document.clone();
        let on_change = Closure::<dyn FnMut(Event)>::new(move |_event: Event| {
            let _ = apply_style_to_selection(&doc);
            let _ = commit_history(&doc);
        });
        input.add_event_listener_with_callback("change", on_change.as_ref().unchecked_ref())?;
        on_change.forget();
    }

    update_swatch(document, DEFAULT_COLOR)?;
    if let Ok(output) = by_id(document, "stroke-size-out") {
        output.set_inner_html(&format!("{DEFAULT_SIZE:.0}"));
    }
    Ok(())
}

fn update_swatch(document: &Document, color: &str) -> Result<(), JsValue> {
    if let Ok(swatch) = by_id(document, "stroke-swatch") {
        swatch.set_attribute("style", &format!("background:{color};"))?;
    }
    Ok(())
}

fn setup_image_input(document: &Document) -> Result<(), JsValue> {
    let input: HtmlInputElement = by_id(document, "image-input")?.dyn_into()?;
    let doc = document.clone();
    let input_for_change = input.clone();
    let on_change = Closure::<dyn FnMut(Event)>::new(move |_event: Event| {
        let _ = handle_image_files(&doc, &input_for_change);
    });
    input.set_onchange(Some(on_change.as_ref().unchecked_ref()));
    on_change.forget();
    Ok(())
}

fn setup_text_editor(document: &Document) -> Result<(), JsValue> {
    let editor: HtmlTextAreaElement = by_id(document, "draw-text-editor")?.dyn_into()?;

    let doc = document.clone();
    let keydown = Closure::<dyn FnMut(KeyboardEvent)>::new(move |event: KeyboardEvent| {
        if event.key() == "Escape" {
            event.prevent_default();
            event.stop_propagation();
            let _ = finish_text_editor(&doc, false);
        } else if event.key() == "Enter" && !event.shift_key() {
            event.prevent_default();
            event.stop_propagation();
            let _ = finish_text_editor(&doc, true);
        }
    });
    editor.set_onkeydown(Some(keydown.as_ref().unchecked_ref()));
    keydown.forget();

    let doc = document.clone();
    let blur = Closure::<dyn FnMut(Event)>::new(move |_event: Event| {
        let _ = finish_text_editor(&doc, true);
    });
    editor.set_onblur(Some(blur.as_ref().unchecked_ref()));
    blur.forget();

    let doc = document.clone();
    let input = Closure::<dyn FnMut(Event)>::new(move |_event: Event| {
        let _ = layout_text_editor(&doc);
    });
    editor.set_oninput(Some(input.as_ref().unchecked_ref()));
    input.forget();
    Ok(())
}

fn setup_keyboard(document: &Document) -> Result<(), JsValue> {
    let Some(window) = web_sys::window() else {
        return Ok(());
    };
    let doc = document.clone();
    let keydown = Closure::<dyn FnMut(KeyboardEvent)>::new(move |event: KeyboardEvent| {
        let target = event
            .target()
            .and_then(|target| target.dyn_into::<Element>().ok());
        let tag = target
            .as_ref()
            .map(|element| element.tag_name().to_lowercase())
            .unwrap_or_default();
        if tag == "input" || tag == "textarea" {
            return;
        }

        if event.ctrl_key() || event.meta_key() {
            match event.key().to_lowercase().as_str() {
                "z" => {
                    event.prevent_default();
                    if event.shift_key() {
                        let _ = redo(&doc);
                    } else {
                        let _ = undo(&doc);
                    }
                }
                "y" => {
                    event.prevent_default();
                    let _ = redo(&doc);
                }
                _ => {}
            }
            return;
        }

        match event.key().to_lowercase().as_str() {
            "v" => {
                event.prevent_default();
                let _ = set_tool(&doc, Tool::Select);
            }
            "h" => {
                event.prevent_default();
                let _ = set_tool(&doc, Tool::Pan);
            }
            "p" => {
                event.prevent_default();
                let _ = set_tool(&doc, Tool::Pen);
            }
            "l" => {
                event.prevent_default();
                let _ = set_tool(&doc, Tool::Line);
            }
            "r" => {
                event.prevent_default();
                let _ = set_tool(&doc, Tool::Rect);
            }
            "o" => {
                event.prevent_default();
                let _ = set_tool(&doc, Tool::Ellipse);
            }
            "t" => {
                event.prevent_default();
                let _ = set_tool(&doc, Tool::Text);
            }
            "e" => {
                event.prevent_default();
                let _ = set_tool(&doc, Tool::Eraser);
            }
            "i" => {
                event.prevent_default();
                let _ = set_tool(&doc, Tool::Image);
                show_toast(&doc, "Clique no canvas para posicionar a imagem.");
            }
            "g" => {
                event.prevent_default();
                let _ = toggle_grid(&doc);
            }
            "delete" | "backspace" => {
                event.prevent_default();
                let _ = delete_selection(&doc);
            }
            "escape" => {
                let _ = finish_text_editor(&doc, false);
                let _ = clear_selection(&doc);
                crate::web::close_app_drawer(&doc);
            }
            _ => {}
        }
    });
    window.add_event_listener_with_callback("keydown", keydown.as_ref().unchecked_ref())?;
    keydown.forget();
    Ok(())
}

// ---------------------------------------------------------------------------
// View / transformação
// ---------------------------------------------------------------------------

pub fn set_view(scale: f64, offset_x: f64, offset_y: f64, width: f64, height: f64) {
    let Some(document) = web_sys::window().and_then(|window| window.document()) else {
        return;
    };

    STATE.with(|state| {
        state.borrow_mut().view = View {
            scale: scale.max(0.01),
            offset_x,
            offset_y,
            width: width.max(1.0),
            height: height.max(1.0),
        };
    });

    if let Ok(world) = draw_world(&document) {
        let _ = world.set_attribute(
            "transform",
            &format!(
                "translate({} {}) scale({})",
                fmt(offset_x),
                fmt(offset_y),
                fmt(scale.max(0.01))
            ),
        );
    }
    if let Ok(layer) = by_id(&document, "draw-layer") {
        let _ = layer.set_attribute(
            "viewBox",
            &format!("0 0 {} {}", width.max(1.0), height.max(1.0)),
        );
    }
    let _ = update_overlay(&document);
    let _ = layout_text_editor(&document);
}

pub fn set_tool(document: &Document, tool: Tool) -> Result<(), JsValue> {
    let _ = finish_text_editor(document, true);
    STATE.with(|state| state.borrow_mut().tool = tool);

    if let Ok(layer) = by_id(document, "draw-layer") {
        layer.set_attribute("data-tool", tool.as_str())?;
    }

    for candidate in [
        Tool::Select,
        Tool::Pan,
        Tool::Pen,
        Tool::Line,
        Tool::Rect,
        Tool::Ellipse,
        Tool::Text,
        Tool::Eraser,
        Tool::Image,
    ] {
        if let Ok(button) = by_id(document, &format!("tool-{}", candidate.as_str())) {
            let pressed = if candidate == tool { "true" } else { "false" };
            button.set_attribute("aria-pressed", pressed)?;
        }
    }

    if tool != Tool::Select {
        clear_selection(document)?;
    }
    crate::web::close_app_drawer(document);
    crate::web::disable_resize_tool(document);
    Ok(())
}

// ---------------------------------------------------------------------------
// Ponteiro
// ---------------------------------------------------------------------------

fn on_pointer_down(document: &Document, event: &PointerEvent) -> Result<(), JsValue> {
    if event.button() != 0 {
        return Ok(());
    }

    let tool = STATE.with(|state| state.borrow().tool);
    if tool == Tool::Pan {
        return Ok(());
    }

    let (screen_x, screen_y) = screen_point(document, event.client_x() as f64, event.client_y() as f64)?;
    let world = screen_to_world(screen_x, screen_y);

    match tool {
        Tool::Select => {
            let Some(shape) = shape_from_event(event) else {
                return Ok(());
            };
            let Some(id) = shape.get_attribute("data-shape-id") else {
                return Ok(());
            };
            event.prevent_default();
            event.stop_propagation();
            select_shape(document, &id, event.shift_key())?;
            start_move(document, world)?;
            let layer = by_id(document, "draw-layer")?;
            let _ = layer.set_pointer_capture(event.pointer_id());
        }
        Tool::Eraser => {
            event.prevent_default();
            event.stop_propagation();
            let layer = by_id(document, "draw-layer")?;
            let _ = layer.set_pointer_capture(event.pointer_id());
            let erased = erase_at(document, event.client_x() as f64, event.client_y() as f64)?;
            STATE.with(|state| {
                state.borrow_mut().gesture = Some(Gesture::Erase { dirty: erased });
            });
            if erased {
                let _ = update_overlay(document);
            }
        }
        Tool::Text => {
            event.prevent_default();
            event.stop_propagation();
            if let Some(shape) = shape_from_event(event) {
                if shape.get_attribute("data-kind").as_deref() == Some("text") {
                    begin_text_edit(document, &shape)?;
                    return Ok(());
                }
            }
            begin_new_text(document, world)?;
        }
        Tool::Image => {
            event.prevent_default();
            event.stop_propagation();
            open_image_picker(document, Some(world))?;
        }
        Tool::Pen => {
            event.prevent_default();
            event.stop_propagation();
            let layer = by_id(document, "draw-layer")?;
            let _ = layer.set_pointer_capture(event.pointer_id());
            create_pen(document, world)?;
        }
        Tool::Line => {
            event.prevent_default();
            event.stop_propagation();
            let layer = by_id(document, "draw-layer")?;
            let _ = layer.set_pointer_capture(event.pointer_id());
            create_line(document, world)?;
        }
        Tool::Rect => {
            event.prevent_default();
            event.stop_propagation();
            let layer = by_id(document, "draw-layer")?;
            let _ = layer.set_pointer_capture(event.pointer_id());
            create_rect(document, world)?;
        }
        Tool::Ellipse => {
            event.prevent_default();
            event.stop_propagation();
            let layer = by_id(document, "draw-layer")?;
            let _ = layer.set_pointer_capture(event.pointer_id());
            create_ellipse(document, world)?;
        }
        Tool::Pan => {}
    }
    Ok(())
}

fn on_pointer_move(document: &Document, event: &PointerEvent) -> Result<(), JsValue> {
    let has_gesture = STATE.with(|state| state.borrow().gesture.is_some());
    if !has_gesture {
        return Ok(());
    }

    event.prevent_default();
    event.stop_propagation();

    let (screen_x, screen_y) = screen_point(document, event.client_x() as f64, event.client_y() as f64)?;
    let world = screen_to_world(screen_x, screen_y);

    let erasing = STATE.with(|state| matches!(state.borrow().gesture, Some(Gesture::Erase { .. })));
    if erasing {
        let erased = erase_at(document, event.client_x() as f64, event.client_y() as f64)?;
        if erased {
            STATE.with(|state| {
                if let Some(Gesture::Erase { dirty }) = &mut state.borrow_mut().gesture {
                    *dirty = true;
                }
            });
            let _ = update_overlay(document);
        }
        return Ok(());
    }

    let mut moved_selection = false;
    STATE.with(|state| {
        let mut state = state.borrow_mut();
        let scale = state.view.scale;
        match state.gesture.as_mut() {
            Some(Gesture::Pen { element, points }) => {
                if let Some((last_x, last_y)) = points.last().copied() {
                    let dx = world.0 - last_x;
                    let dy = world.1 - last_y;
                    if (dx * dx + dy * dy).sqrt() < 1.2 / scale {
                        return;
                    }
                }
                points.push(world);
                let _ = element.set_attribute("d", &path_from(points));
            }
            Some(Gesture::Line { element, .. }) => {
                let _ = element.set_attribute("x2", &fmt(world.0));
                let _ = element.set_attribute("y2", &fmt(world.1));
            }
            Some(Gesture::Rect { element, from }) => {
                let (mut dx, mut dy) = (world.0 - from.0, world.1 - from.1);
                if event.shift_key() {
                    let size = dx.abs().max(dy.abs());
                    dx = if dx < 0.0 { -size } else { size };
                    dy = if dy < 0.0 { -size } else { size };
                }
                let _ = element.set_attribute("x", &fmt(from.0.min(from.0 + dx)));
                let _ = element.set_attribute("y", &fmt(from.1.min(from.1 + dy)));
                let _ = element.set_attribute("width", &fmt(dx.abs()));
                let _ = element.set_attribute("height", &fmt(dy.abs()));
            }
            Some(Gesture::Ellipse { element, from }) => {
                let (mut dx, mut dy) = (world.0 - from.0, world.1 - from.1);
                if event.shift_key() {
                    let size = dx.abs().max(dy.abs());
                    dx = if dx < 0.0 { -size } else { size };
                    dy = if dy < 0.0 { -size } else { size };
                }
                let _ = element.set_attribute("cx", &fmt(from.0 + dx / 2.0));
                let _ = element.set_attribute("cy", &fmt(from.1 + dy / 2.0));
                let _ = element.set_attribute("rx", &fmt(dx.abs() / 2.0));
                let _ = element.set_attribute("ry", &fmt(dy.abs() / 2.0));
            }
            Some(Gesture::Move { from, items, dirty }) => {
                let dx = world.0 - from.0;
                let dy = world.1 - from.1;
                if dx.abs() > 0.5 / scale || dy.abs() > 0.5 / scale {
                    *dirty = true;
                }
                for item in items.iter() {
                    if let Ok(Some(shape)) = find_shape(document, &item.id) {
                        let tx = item.base_x + dx;
                        let ty = item.base_y + dy;
                        let _ = shape.set_attribute("data-tx", &fmt(tx));
                        let _ = shape.set_attribute("data-ty", &fmt(ty));
                        let _ = shape.set_attribute(
                            "transform",
                            &format!("translate({} {})", fmt(tx), fmt(ty)),
                        );
                    }
                }
                moved_selection = true;
            }
            Some(Gesture::Erase { .. }) | None => {}
        }
    });

    if moved_selection {
        update_overlay(document)?;
    }
    Ok(())
}

fn on_pointer_up(document: &Document, event: &PointerEvent) -> Result<(), JsValue> {
    if let Ok(layer) = by_id(document, "draw-layer") {
        let _ = layer.release_pointer_capture(event.pointer_id());
    }
    let gesture = STATE.with(|state| state.borrow_mut().gesture.take());
    if let Some(gesture) = gesture {
        finish_gesture(document, gesture)?;
    }
    Ok(())
}

fn on_pointer_cancel(document: &Document, event: &PointerEvent) -> Result<(), JsValue> {
    if let Ok(layer) = by_id(document, "draw-layer") {
        let _ = layer.release_pointer_capture(event.pointer_id());
    }
    let gesture = STATE.with(|state| state.borrow_mut().gesture.take());
    if let Some(gesture) = gesture {
        match gesture {
            Gesture::Pen { element, .. }
            | Gesture::Line { element, .. }
            | Gesture::Rect { element, .. }
            | Gesture::Ellipse { element, .. } => {
                let _ = element.remove();
            }
            Gesture::Move { .. } | Gesture::Erase { .. } => {}
        }
    }
    update_overlay(document)?;
    Ok(())
}

fn on_double_click(document: &Document, event: &MouseEvent) -> Result<(), JsValue> {
    let Some(shape) = event
        .target()
        .and_then(|target| target.dyn_into::<Element>().ok())
        .and_then(|element| element.closest("text[data-shape]").ok().flatten())
    else {
        return Ok(());
    };
    event.prevent_default();
    event.stop_propagation();
    begin_text_edit(document, &shape)
}

fn finish_gesture(document: &Document, gesture: Gesture) -> Result<(), JsValue> {
    match gesture {
        Gesture::Pen { element, points } => {
            if points.is_empty() {
                let _ = element.remove();
            } else {
                if points.len() == 1 {
                    let point = points[0];
                    let _ = element.set_attribute("d", &path_from(&[point, point]));
                }
                commit_history(document)?;
            }
        }
        Gesture::Line { element, .. } => {
            let length = (attr_num(&element, "x2") - attr_num(&element, "x1"))
                .hypot(attr_num(&element, "y2") - attr_num(&element, "y1"));
            if length < 2.0 {
                let _ = element.remove();
            } else {
                commit_history(document)?;
            }
        }
        Gesture::Rect { element, .. } => {
            if attr_num(&element, "width") < 1.5 && attr_num(&element, "height") < 1.5 {
                let _ = element.remove();
            } else {
                commit_history(document)?;
            }
        }
        Gesture::Ellipse { element, .. } => {
            if attr_num(&element, "rx") < 1.0 && attr_num(&element, "ry") < 1.0 {
                let _ = element.remove();
            } else {
                commit_history(document)?;
            }
        }
        Gesture::Move { dirty, .. } => {
            if dirty {
                commit_history(document)?;
            }
        }
        Gesture::Erase { dirty } => {
            if dirty {
                commit_history(document)?;
            }
        }
    }
    update_overlay(document)?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Formas
// ---------------------------------------------------------------------------

fn create_shape(document: &Document, tag: &str) -> Result<Element, JsValue> {
    let id = STATE.with(|state| {
        let mut state = state.borrow_mut();
        let id = state.next_id;
        state.next_id += 1;
        id.to_string()
    });

    let element = document.create_element_ns(Some(SVG_NS), tag)?;
    element.set_attribute("data-shape", "1")?;
    element.set_attribute("data-shape-id", &id)?;
    element.set_attribute("data-tx", "0")?;
    element.set_attribute("data-ty", "0")?;
    element.set_attribute("pointer-events", "all")?;
    draw_world(document)?.append_child(&element)?;
    Ok(element)
}

fn create_pen(document: &Document, world: (f64, f64)) -> Result<(), JsValue> {
    let (color, size) = current_style();
    let element = create_shape(document, "path")?;
    element.set_attribute("data-kind", "pen")?;
    apply_stroke(&element, &color, size)?;
    element.set_attribute("d", &path_from(&[world]))?;
    STATE.with(|state| {
        state.borrow_mut().gesture = Some(Gesture::Pen {
            element,
            points: vec![world],
        });
    });
    Ok(())
}

fn create_line(document: &Document, world: (f64, f64)) -> Result<(), JsValue> {
    let (color, size) = current_style();
    let element = create_shape(document, "line")?;
    element.set_attribute("data-kind", "line")?;
    apply_stroke(&element, &color, size)?;
    element.set_attribute("x1", &fmt(world.0))?;
    element.set_attribute("y1", &fmt(world.1))?;
    element.set_attribute("x2", &fmt(world.0))?;
    element.set_attribute("y2", &fmt(world.1))?;
    STATE.with(|state| {
        state.borrow_mut().gesture = Some(Gesture::Line { element });
    });
    Ok(())
}

fn create_rect(document: &Document, world: (f64, f64)) -> Result<(), JsValue> {
    let (color, size) = current_style();
    let element = create_shape(document, "rect")?;
    element.set_attribute("data-kind", "rect")?;
    apply_stroke(&element, &color, size)?;
    element.set_attribute("x", &fmt(world.0))?;
    element.set_attribute("y", &fmt(world.1))?;
    element.set_attribute("width", "0")?;
    element.set_attribute("height", "0")?;
    STATE.with(|state| {
        state.borrow_mut().gesture = Some(Gesture::Rect {
            element,
            from: world,
        });
    });
    Ok(())
}

fn create_ellipse(document: &Document, world: (f64, f64)) -> Result<(), JsValue> {
    let (color, size) = current_style();
    let element = create_shape(document, "ellipse")?;
    element.set_attribute("data-kind", "ellipse")?;
    apply_stroke(&element, &color, size)?;
    element.set_attribute("cx", &fmt(world.0))?;
    element.set_attribute("cy", &fmt(world.1))?;
    element.set_attribute("rx", "0")?;
    element.set_attribute("ry", "0")?;
    STATE.with(|state| {
        state.borrow_mut().gesture = Some(Gesture::Ellipse {
            element,
            from: world,
        });
    });
    Ok(())
}

fn apply_stroke(element: &Element, color: &str, size: f64) -> Result<(), JsValue> {
    element.set_attribute("fill", "none")?;
    element.set_attribute("stroke", color)?;
    element.set_attribute("stroke-width", &fmt(size))?;
    element.set_attribute("stroke-linecap", "round")?;
    element.set_attribute("stroke-linejoin", "round")?;
    Ok(())
}

fn current_style() -> (String, f64) {
    STATE.with(|state| {
        let state = state.borrow();
        (state.color.clone(), state.size)
    })
}

fn path_from(points: &[(f64, f64)]) -> String {
    match points {
        [] => String::new(),
        [point] => format!("M{} {} L{} {}", fmt(point.0), fmt(point.1), fmt(point.0), fmt(point.1)),
        [first, second] => format!(
            "M{} {} L{} {}",
            fmt(first.0),
            fmt(first.1),
            fmt(second.0),
            fmt(second.1)
        ),
        _ => {
            let mut path = format!("M{} {}", fmt(points[0].0), fmt(points[0].1));
            for index in 1..points.len() - 1 {
                let point = points[index];
                let next = points[index + 1];
                path.push_str(&format!(
                    " Q{} {} {} {}",
                    fmt(point.0),
                    fmt(point.1),
                    fmt((point.0 + next.0) / 2.0),
                    fmt((point.1 + next.1) / 2.0)
                ));
            }
            let last = points[points.len() - 1];
            path.push_str(&format!(" L{} {}", fmt(last.0), fmt(last.1)));
            path
        }
    }
}

// ---------------------------------------------------------------------------
// Seleção / histórico
// ---------------------------------------------------------------------------

fn select_shape(document: &Document, id: &str, additive: bool) -> Result<(), JsValue> {
    STATE.with(|state| {
        let mut state = state.borrow_mut();
        if additive {
            if let Some(index) = state.selected.iter().position(|selected| selected == id) {
                state.selected.remove(index);
            } else {
                state.selected.push(id.to_owned());
            }
        } else if !state.selected.iter().any(|selected| selected == id) {
            state.selected.clear();
            state.selected.push(id.to_owned());
        }
    });
    update_overlay(document)
}

fn start_move(document: &Document, world: (f64, f64)) -> Result<(), JsValue> {
    let ids = STATE.with(|state| state.borrow().selected.clone());
    let mut items = Vec::new();
    for id in ids {
        if let Some(shape) = find_shape(document, &id)? {
            items.push(SelectedShape {
                id,
                base_x: attr_num(&shape, "data-tx"),
                base_y: attr_num(&shape, "data-ty"),
            });
        }
    }
    if !items.is_empty() {
        STATE.with(|state| {
            state.borrow_mut().gesture = Some(Gesture::Move {
                from: world,
                items,
                dirty: false,
            });
        });
    }
    Ok(())
}

fn clear_selection(document: &Document) -> Result<(), JsValue> {
    STATE.with(|state| state.borrow_mut().selected.clear());
    update_overlay(document)
}

fn delete_selection(document: &Document) -> Result<(), JsValue> {
    let ids = STATE.with(|state| std::mem::take(&mut state.borrow_mut().selected));
    if ids.is_empty() {
        return Ok(());
    }
    for id in ids {
        if let Some(shape) = find_shape(document, &id)? {
            let _ = shape.remove();
        }
    }
    update_overlay(document)?;
    commit_history(document)?;
    Ok(())
}

fn undo(document: &Document) -> Result<(), JsValue> {
    let _ = finish_text_editor(document, true);
    let html = STATE.with(|state| {
        let mut state = state.borrow_mut();
        if state.history_index == 0 {
            return None;
        }
        state.history_index -= 1;
        state.history.get(state.history_index).cloned()
    });

    if let Some(html) = html {
        apply_history_html(document, &html)?;
        show_toast(document, "Desfeito.");
    } else {
        show_toast(document, "Nada para desfazer.");
    }
    Ok(())
}

fn redo(document: &Document) -> Result<(), JsValue> {
    let _ = finish_text_editor(document, true);
    let html = STATE.with(|state| {
        let mut state = state.borrow_mut();
        if state.history_index + 1 >= state.history.len() {
            return None;
        }
        state.history_index += 1;
        state.history.get(state.history_index).cloned()
    });

    if let Some(html) = html {
        apply_history_html(document, &html)?;
        show_toast(document, "Refeito.");
    } else {
        show_toast(document, "Nada para refazer.");
    }
    Ok(())
}

fn clear_canvas(document: &Document) -> Result<(), JsValue> {
    let _ = finish_text_editor(document, true);
    let world = draw_world(document)?;
    if world.first_child().is_none() {
        show_toast(document, "O canvas já está vazio.");
        return Ok(());
    }
    let Some(window) = web_sys::window() else {
        return Ok(());
    };
    if !window.confirm_with_message("Apagar todo o desenho e as imagens do canvas?")? {
        return Ok(());
    }
    world.set_inner_html("");
    clear_selection(document)?;
    commit_history(document)?;
    show_toast(document, "Canvas limpo.");
    Ok(())
}

fn commit_history(document: &Document) -> Result<(), JsValue> {
    let html = serialize_world(document)?;
    STATE.with(|state| {
        let mut state = state.borrow_mut();
        if state.history.get(state.history_index) == Some(&html) {
            return;
        }
        let next_index = state.history_index + 1;
        if next_index < state.history.len() {
            state.history.truncate(next_index);
        }
        state.history.push(html);
        if state.history.len() > MAX_HISTORY {
            state.history.remove(0);
        }
        state.history_index = state.history.len().saturating_sub(1);
    });
    Ok(())
}

/// Serializa o mundo sem os data URLs das imagens: o snapshot guarda apenas
/// `data-image-id` e o base64 continua em `STATE.images`. Sem isso, cada ação
/// duplicaria todos os bytes de cada imagem no histórico (60 snapshots).
fn serialize_world(document: &Document) -> Result<String, JsValue> {
    let world = draw_world(document)?;
    let nodes = world.query_selector_all("image[data-image-id]")?;
    let mut saved: Vec<(Element, Option<String>)> = Vec::new();
    for index in 0..nodes.length() {
        let Some(node) = nodes.item(index) else {
            continue;
        };
        let Ok(element) = node.dyn_into::<Element>() else {
            continue;
        };
        let href = element.get_attribute("href");
        let _ = element.set_attribute("href", "");
        saved.push((element, href));
    }

    let html = world.inner_html();
    for (element, href) in saved {
        if let Some(href) = href {
            let _ = element.set_attribute("href", &href);
        }
    }
    Ok(html)
}

/// Restaura um snapshot e devolve os `href` das imagens a partir do mapa.
fn apply_history_html(document: &Document, html: &str) -> Result<(), JsValue> {
    let world = draw_world(document)?;
    world.set_inner_html(html);

    let nodes = world.query_selector_all("image[data-image-id]")?;
    for index in 0..nodes.length() {
        let Some(node) = nodes.item(index) else {
            continue;
        };
        let Ok(element) = node.dyn_into::<Element>() else {
            continue;
        };
        let Some(id) = element.get_attribute("data-image-id") else {
            continue;
        };
        let data = STATE.with(|state| state.borrow().images.get(&id).cloned());
        if let Some(data) = data {
            let _ = element.set_attribute("href", &data);
        }
    }
    clear_selection(document)?;
    Ok(())
}

fn update_overlay(document: &Document) -> Result<(), JsValue> {
    let overlay = by_id(document, "draw-overlay")?;
    overlay.set_inner_html("");

    let selected = STATE.with(|state| state.borrow().selected.clone());
    if selected.is_empty() {
        return Ok(());
    }

    let layer_rect = by_id(document, "draw-layer")?.get_bounding_client_rect();
    for id in selected {
        let Some(shape) = find_shape(document, &id)? else {
            continue;
        };
        let rect = shape.get_bounding_client_rect();
        let pad = 5.0;
        let selection = document.create_element_ns(Some(SVG_NS), "rect")?;
        selection.set_attribute("class", "draw-selection")?;
        selection.set_attribute("x", &fmt(rect.left() - layer_rect.left() - pad))?;
        selection.set_attribute("y", &fmt(rect.top() - layer_rect.top() - pad))?;
        selection.set_attribute("width", &fmt((rect.width() + pad * 2.0).max(1.0)))?;
        selection.set_attribute("height", &fmt((rect.height() + pad * 2.0).max(1.0)))?;
        overlay.append_child(&selection)?;
    }
    Ok(())
}

fn erase_at(document: &Document, client_x: f64, client_y: f64) -> Result<bool, JsValue> {
    let Some(target) = document.element_from_point(client_x as f32, client_y as f32) else {
        return Ok(false);
    };
    let Some(shape) = target.closest("[data-shape]").ok().flatten() else {
        return Ok(false);
    };
    let id = shape.get_attribute("data-shape-id").unwrap_or_default();
    if !id.is_empty() {
        STATE.with(|state| {
            state
                .borrow_mut()
                .selected
                .retain(|selected| selected != &id);
        });
    }
    let _ = shape.remove();
    Ok(true)
}

fn toggle_grid(document: &Document) -> Result<(), JsValue> {
    let visible = STATE.with(|state| {
        let mut state = state.borrow_mut();
        state.grid_visible = !state.grid_visible;
        state.grid_visible
    });
    let grid = by_id(document, "grid")?;
    if visible {
        grid.remove_attribute("style")?;
    } else {
        grid.set_attribute("style", "display:none;")?;
    }
    if let Some(root) = document.document_element() {
        if visible {
            let _ = root.remove_attribute("data-grid");
        } else {
            let _ = root.set_attribute("data-grid", "off");
        }
    }
    if let Ok(button) = by_id(document, "action-grid") {
        button.set_attribute("aria-pressed", if visible { "true" } else { "false" })?;
    }
    show_toast(
        document,
        if visible {
            "Grade visível."
        } else {
            "Grade oculta."
        },
    );
    Ok(())
}

fn apply_style_to_selection(document: &Document) -> Result<(), JsValue> {
    let (color, size) = current_style();
    let ids = STATE.with(|state| state.borrow().selected.clone());
    for id in ids {
        let Some(shape) = find_shape(document, &id)? else {
            continue;
        };
        match shape.get_attribute("data-kind").as_deref() {
            Some("text") => {
                shape.set_attribute("fill", &color)?;
                shape.set_attribute("font-size", &fmt(font_for(size)))?;
            }
            Some("image") => {}
            _ => {
                shape.set_attribute("stroke", &color)?;
                shape.set_attribute("stroke-width", &fmt(size))?;
            }
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Texto
// ---------------------------------------------------------------------------

fn begin_new_text(document: &Document, world: (f64, f64)) -> Result<(), JsValue> {
    let _ = finish_text_editor(document, true);
    let (color, size) = current_style();
    let font_size = font_for(size);
    let element = create_shape(document, "text")?;
    element.set_attribute("data-kind", "text")?;
    element.set_attribute("x", &fmt(world.0))?;
    element.set_attribute("y", &fmt(world.1))?;
    element.set_attribute("fill", &color)?;
    element.set_attribute("font-size", &fmt(font_size))?;
    element.set_attribute("font-family", DEFAULT_FONT)?;
    element.set_attribute("dominant-baseline", "middle")?;
    element.set_attribute("data-text", "")?;
    element.set_attribute("data-editing", "1")?;

    STATE.with(|state| {
        state.borrow_mut().text_editor = Some(TextEditor {
            target: element,
            world_x: world.0,
            world_y: world.1,
            font_size,
            is_new: true,
            original_html: String::new(),
        });
    });
    open_text_editor(document, "")?;
    Ok(())
}

fn begin_text_edit(document: &Document, target: &Element) -> Result<(), JsValue> {
    let _ = finish_text_editor(document, true);
    let (tx, ty) = transform_offset(target);
    let x = attr_num(target, "x") + tx;
    let y = attr_num(target, "y") + ty;
    let font_size = attr_num(target, "font-size").max(14.0);
    let value = target
        .get_attribute("data-text")
        .unwrap_or_else(|| target.text_content().unwrap_or_default());
    let original_html = target.inner_html();
    target.set_attribute("data-editing", "1")?;

    STATE.with(|state| {
        state.borrow_mut().text_editor = Some(TextEditor {
            target: target.clone(),
            world_x: x,
            world_y: y,
            font_size,
            is_new: false,
            original_html,
        });
    });
    open_text_editor(document, &value)?;
    Ok(())
}

fn open_text_editor(document: &Document, value: &str) -> Result<(), JsValue> {
    let editor: HtmlTextAreaElement = by_id(document, "draw-text-editor")?.dyn_into()?;
    editor.set_value(value);
    let _ = editor.remove_attribute("hidden");
    layout_text_editor(document)?;
    let _ = editor.focus();
    let length = value.encode_utf16().count() as u32;
    let _ = editor.set_selection_range(length, length);
    Ok(())
}

fn finish_text_editor(document: &Document, save: bool) -> Result<(), JsValue> {
    let Some(editor_state) = STATE.with(|state| state.borrow_mut().text_editor.take()) else {
        return Ok(());
    };

    let editor: HtmlTextAreaElement = by_id(document, "draw-text-editor")?.dyn_into()?;
    let value = editor.value().replace('\r', "");
    let _ = editor.blur();
    editor.set_value("");
    editor.set_attribute("hidden", "")?;

    let target = editor_state.target;
    let connected = target.parent_node().is_some();
    if connected {
        let _ = target.remove_attribute("data-editing");
        if !save {
            if editor_state.is_new {
                let _ = target.remove();
            } else {
                target.set_inner_html(&editor_state.original_html);
            }
        } else if value.trim().is_empty() {
            let _ = target.remove();
        } else {
            write_text_content(&target, &value, editor_state.font_size)?;
        }
    }

    commit_history(document)?;
    update_overlay(document)?;
    Ok(())
}

fn write_text_content(target: &Element, value: &str, font_size: f64) -> Result<(), JsValue> {
    let x = target.get_attribute("x").unwrap_or_else(|| "0".to_owned());
    let mut html = String::new();
    for (index, line) in value.split('\n').enumerate() {
        html.push_str("<tspan x=\"");
        html.push_str(&escape_xml(&x));
        html.push('"');
        if index > 0 {
            html.push_str(&format!(" dy=\"{}\"", fmt(font_size * LINE_RATIO)));
        }
        html.push('>');
        html.push_str(&escape_xml(line));
        html.push_str("</tspan>");
    }
    target.set_inner_html(&html);
    target.set_attribute("data-text", value)?;
    Ok(())
}

fn layout_text_editor(document: &Document) -> Result<(), JsValue> {
    let Some((view, world_x, world_y, font_size, color)) = STATE.with(|state| {
        let state = state.borrow();
        state.text_editor.as_ref().map(|editor| {
            (
                state.view,
                editor.world_x,
                editor.world_y,
                editor.font_size,
                editor
                    .target
                    .get_attribute("fill")
                    .unwrap_or_else(|| state.color.clone()),
            )
        })
    }) else {
        return Ok(());
    };

    let editor: HtmlTextAreaElement = by_id(document, "draw-text-editor")?.dyn_into()?;
    let x = world_x * view.scale + view.offset_x;
    let y = world_y * view.scale + view.offset_y;
    let font_px = (font_size * view.scale).max(8.0);
    let lines = editor.value().split('\n').count().max(1) as f64;
    let height = (font_px * LINE_RATIO * lines + 8.0).max(font_px + 8.0);
    editor.set_attribute(
        "style",
        &format!(
            "left:{}px;top:{}px;font-size:{}px;line-height:{};height:{}px;min-width:{}px;color:{};",
            fmt(x),
            fmt(y - font_px * 0.7),
            fmt(font_px),
            LINE_RATIO,
            fmt(height),
            fmt((font_px * 4.0).max(120.0)),
            color
        ),
    )?;
    Ok(())
}

fn font_for(size: f64) -> f64 {
    (size * 7.0).max(14.0)
}

// ---------------------------------------------------------------------------
// Imagens
// ---------------------------------------------------------------------------

pub fn open_image_picker(
    document: &Document,
    point: Option<(f64, f64)>,
) -> Result<(), JsValue> {
    STATE.with(|state| state.borrow_mut().pending_image_point = point);
    let input: HtmlInputElement = by_id(document, "image-input")?.dyn_into()?;
    input.set_value("");
    input.click();
    Ok(())
}

fn handle_image_files(document: &Document, input: &HtmlInputElement) -> Result<(), JsValue> {
    let Some(files) = input.files() else {
        return Ok(());
    };
    if files.length() == 0 {
        return Ok(());
    }

    let base = STATE
        .with(|state| state.borrow_mut().pending_image_point.take())
        .unwrap_or_else(view_center);

    for index in 0..files.length().min(8) {
        let Some(file) = files.get(index) else {
            continue;
        };
        let kind = file.type_();
        if !kind.is_empty() && !kind.starts_with("image/") {
            continue;
        }
        if file.size() > IMAGE_FILE_LIMIT {
            show_toast(
                document,
                "Imagem muito grande (acima de 40 MB); escolha um arquivo menor.",
            );
            continue;
        }

        let reader = FileReader::new()?;
        let reader_for_load = reader.clone();
        let document = document.clone();
        let world_x = base.0 + index as f64 * 30.0;
        let world_y = base.1 + index as f64 * 30.0;
        let onload = Closure::<dyn FnMut()>::new(move || {
            if let Ok(result) = reader_for_load.result() {
                if let Some(data_url) = result.as_string() {
                    // Decodifica e reduz antes de entrar no DOM: uma foto de
                    // 12 MP vira ~300 KB em vez de ~10 MB de base64.
                    fit_image(&document, data_url, world_x, world_y);
                }
            }
        });
        reader.set_onload(Some(onload.as_ref().unchecked_ref()));
        onload.forget();

        let blob: &Blob = file.as_ref();
        let _ = reader.read_as_data_url(blob);
    }

    input.set_value("");
    Ok(())
}

/// Carrega a imagem, reduz se necessário e insere no canvas. Imagens pequenas
/// entram como vieram (preserva transparência); fotos grandes viram JPEG.
fn fit_image(document: &Document, data_url: String, world_x: f64, world_y: f64) {
    let Ok(image) = HtmlImageElement::new() else {
        show_toast(document, "Não foi possível preparar a imagem.");
        return;
    };

    let document_for_load = document.clone();
    let image_for_load = image.clone();
    let data_for_load = data_url.clone();
    let onload = Closure::<dyn FnMut()>::new(move || {
        let width = image_for_load.natural_width() as f64;
        let height = image_for_load.natural_height() as f64;
        // Só o tamanho do base64 importa: dimensões grandes com arquivo
        // pequeno (PNG chapado) não justificam recomprimir para JPEG.
        let small = data_for_load.len() <= IMAGE_RAW_LIMIT;
        let data = if small {
            Some(data_for_load.clone())
        } else {
            compress_image(&document_for_load, &image_for_load, width, height)
        };
        match data {
            Some(data) => {
                let _ = insert_image(&document_for_load, &data, world_x, world_y);
            }
            None => {
                show_toast(
                    &document_for_load,
                    "Não foi possível reduzir a imagem; tente outro arquivo.",
                );
            }
        }
    });
    image.set_onload(Some(onload.as_ref().unchecked_ref()));
    onload.forget();

    let document_for_error = document.clone();
    let onerror = Closure::<dyn FnMut()>::new(move || {
        show_toast(&document_for_error, "Formato de imagem não suportado.");
    });
    image.set_onerror(Some(onerror.as_ref().unchecked_ref()));
    onerror.forget();

    image.set_src(&data_url);
}

/// Desenha a imagem num canvas com o maior lado em 1600 px e exporta JPEG.
/// Se ainda passar do teto, reduz mais e baixa a qualidade.
fn compress_image(
    document: &Document,
    image: &HtmlImageElement,
    width: f64,
    height: f64,
) -> Option<String> {
    if width <= 0.0 || height <= 0.0 {
        return None;
    }
    let scale = (IMAGE_MAX_DIM / width.max(height)).min(1.0);
    let mut target_w = (width * scale).round().max(1.0);
    let mut target_h = (height * scale).round().max(1.0);
    let canvas = document
        .create_element("canvas")
        .ok()?
        .dyn_into::<HtmlCanvasElement>()
        .ok()?;
    let mut quality = 0.85;

    for attempt in 0..3 {
        canvas.set_width(target_w.max(1.0) as u32);
        canvas.set_height(target_h.max(1.0) as u32);
        let context = canvas
            .get_context("2d")
            .ok()??
            .dyn_into::<CanvasRenderingContext2d>()
            .ok()?;
        // JPEG não tem alfa: fundo branco evita áreas pretas.
        context.set_fill_style_str("#ffffff");
        context.fill_rect(0.0, 0.0, target_w, target_h);
        context
            .draw_image_with_html_image_element_and_dw_and_dh(
                image, 0.0, 0.0, target_w, target_h,
            )
            .ok()?;
        let options = js_sys::Object::new();
        let _ = js_sys::Reflect::set(
            &options,
            &JsValue::from_str("quality"),
            &JsValue::from_f64(quality),
        );
        let data = canvas
            .to_data_url_with_type_and_encoder_options("image/jpeg", &options)
            .ok()?;
        if data.len() <= IMAGE_MAX_BYTES || attempt == 2 {
            return Some(data);
        }
        target_w *= 0.75;
        target_h *= 0.75;
        quality -= 0.12;
    }
    None
}

fn insert_image(
    document: &Document,
    data_url: &str,
    world_x: f64,
    world_y: f64,
) -> Result<(), JsValue> {
    let image_id = STATE.with(|state| {
        let mut state = state.borrow_mut();
        let id = state.next_image_id;
        state.next_image_id += 1;
        state.images.insert(id.to_string(), data_url.to_owned());
        id.to_string()
    });

    let element = create_shape(document, "image")?;
    element.set_attribute("data-kind", "image")?;
    element.set_attribute("data-image-id", &image_id)?;
    element.set_attribute("x", &fmt(world_x - IMAGE_W / 2.0))?;
    element.set_attribute("y", &fmt(world_y - IMAGE_H / 2.0))?;
    element.set_attribute("width", &fmt(IMAGE_W))?;
    element.set_attribute("height", &fmt(IMAGE_H))?;
    element.set_attribute("preserveAspectRatio", "xMidYMid meet")?;
    element.set_attribute("href", data_url)?;
    commit_history(document)?;
    show_toast(document, "Imagem adicionada ao canvas.");
    Ok(())
}

fn view_center() -> (f64, f64) {
    let view = STATE.with(|state| state.borrow().view);
    (
        (view.width / 2.0 - view.offset_x) / view.scale,
        (view.height / 2.0 - view.offset_y) / view.scale,
    )
}

// ---------------------------------------------------------------------------
// Utilidades
// ---------------------------------------------------------------------------

fn by_id(document: &Document, id: &str) -> Result<Element, JsValue> {
    document
        .get_element_by_id(id)
        .ok_or_else(|| JsValue::from_str(&format!("elemento #{id} ausente")))
}

fn draw_world(document: &Document) -> Result<Element, JsValue> {
    by_id(document, "draw-world")
}

fn find_shape(document: &Document, id: &str) -> Result<Option<Element>, JsValue> {
    draw_world(document)?.query_selector(&format!("[data-shape-id=\"{id}\"]"))
}

fn shape_from_event(event: &PointerEvent) -> Option<Element> {
    event
        .target()?
        .dyn_into::<Element>()
        .ok()?
        .closest("[data-shape]")
        .ok()
        .flatten()
}

fn screen_point(
    document: &Document,
    client_x: f64,
    client_y: f64,
) -> Result<(f64, f64), JsValue> {
    let rect: DomRect = by_id(document, "draw-layer")?.get_bounding_client_rect();
    Ok((client_x - rect.left(), client_y - rect.top()))
}

fn screen_to_world(screen_x: f64, screen_y: f64) -> (f64, f64) {
    let view = STATE.with(|state| state.borrow().view);
    (
        (screen_x - view.offset_x) / view.scale,
        (screen_y - view.offset_y) / view.scale,
    )
}

fn transform_offset(element: &Element) -> (f64, f64) {
    (
        attr_num(element, "data-tx"),
        attr_num(element, "data-ty"),
    )
}

fn attr_num(element: &Element, name: &str) -> f64 {
    element
        .get_attribute(name)
        .and_then(|value| value.parse::<f64>().ok())
        .unwrap_or(0.0)
}

fn fmt(value: f64) -> String {
    format!("{value:.2}")
}

fn escape_xml(input: &str) -> String {
    input
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}
