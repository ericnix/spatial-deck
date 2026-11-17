//! Tema Minimalista: modo claro monocromático e papel milimetrado.
//!
//! O CSS é injetado em um `<style>` próprio para manter todas as regras
//! monocromáticas em um único lugar. A grade também é desenhada no canvas,
//! com pan/zoom, usando a mesma proporção de papel milimetrado (1 mm fino,
//! 5 mm grosso) das regras CSS de fallback.

use wasm_bindgen::JsValue;
use web_sys::{CanvasRenderingContext2d, Document};

const STYLE_ID: &str = "spatial-deck-minimalist-theme";
const PAPER: &str = "#f7f7f2";
const GRID_MINOR: &str = "rgba(0, 0, 0, 0.075)";
const GRID_MAJOR: &str = "rgba(0, 0, 0, 0.17)";

/// Cores do papel milimetrado.
pub struct GraphPaperPalette {
    pub paper: &'static str,
    pub minor: &'static str,
    pub major: &'static str,
}

/// Papel claro do tema Minimalista.
pub const LIGHT_PALETTE: GraphPaperPalette = GraphPaperPalette {
    paper: PAPER,
    minor: GRID_MINOR,
    major: GRID_MAJOR,
};

/// 1 mm em pixels CSS (96 dpi).
const MM_IN_CSS_PX: f64 = 96.0 / 25.4;

/// Regras monocromáticas do modo claro.
pub const MINIMALIST_CSS: &str = r#"
html[data-canvas-theme="minimalist"] {
  --bg-0: #f7f7f2;
  --ink: #000000;
  --muted: #555555;
  --line: #000000;
  --accent: #000000;
  --accent-2: #000000;
  --accent-ink: #ffffff;
  --hud-bg: rgba(255, 255, 255, 0.94);
  --bg-glow-1: #ffffff;
  --bg-glow-2: #eeeeea;
  --glow-border: #000000;
  --glow-shadow: none;
  --glow-bg: transparent;
  --chip-bg: #ffffff;
  --chip-bg-hover: #eeeeea;
  --chip-line-hover: #000000;
  --card: #ffffff;
  --card-line: #000000;
  --paper-mm: 3.7795px;
}

html[data-canvas-theme="minimalist"] body {
  color: #000000;
  background-color: #f7f7f2;
  background-image:
    repeating-linear-gradient(to right, rgba(0, 0, 0, .065) 0 1px, transparent 1px var(--paper-mm)),
    repeating-linear-gradient(to bottom, rgba(0, 0, 0, .065) 0 1px, transparent 1px var(--paper-mm)),
    repeating-linear-gradient(to right, rgba(0, 0, 0, .16) 0 1px, transparent 1px calc(var(--paper-mm) * 5)),
    repeating-linear-gradient(to bottom, rgba(0, 0, 0, .16) 0 1px, transparent 1px calc(var(--paper-mm) * 5)),
    radial-gradient(circle at 50% 0%, #ffffff 0%, transparent 62%);
}

html[data-canvas-theme="minimalist"] #grid {
  background: transparent;
}

html[data-canvas-theme="minimalist"][data-grid="off"] body {
  background-image: none;
}

html[data-canvas-theme="minimalist"] .window-card {
  border: 1px solid #000000;
  border-radius: 0;
  background: #ffffff;
  box-shadow: none;
  backdrop-filter: none;
  -webkit-backdrop-filter: none;
  transition: none;
}

html[data-canvas-theme="minimalist"] .window-card:hover {
  border-color: #000000;
  box-shadow: 0 0 0 1px #000000;
}

html[data-canvas-theme="minimalist"] .window-card__bar {
  border-bottom: 1px solid #000000;
  background: #ffffff;
  color: #555555;
}

html[data-canvas-theme="minimalist"] .window-card__bar b,
html[data-canvas-theme="minimalist"] .window-card__body h2,
html[data-canvas-theme="minimalist"] .brand,
html[data-canvas-theme="minimalist"] .zoom b {
  color: #000000;
}

html[data-canvas-theme="minimalist"] .dots i,
html[data-canvas-theme="minimalist"] .nodes li span,
html[data-canvas-theme="minimalist"] .swatches i {
  background: #000000;
  opacity: 1;
}

html[data-canvas-theme="minimalist"] .dots i:nth-child(2),
html[data-canvas-theme="minimalist"] .dots i:nth-child(3),
html[data-canvas-theme="minimalist"] .swatches i:nth-child(2),
html[data-canvas-theme="minimalist"] .swatches i:nth-child(3),
html[data-canvas-theme="minimalist"] .swatches i:nth-child(4) {
  background: #555555;
}

html[data-canvas-theme="minimalist"] .window-card__body pre,
html[data-canvas-theme="minimalist"] .badge,
html[data-canvas-theme="minimalist"] .app {
  border: 1px solid #000000;
  border-radius: 0;
  background: #ffffff;
  color: #000000;
}

html[data-canvas-theme="minimalist"] .window-card__body pre {
  box-shadow: none;
}

html[data-canvas-theme="minimalist"] #hud,
html[data-canvas-theme="minimalist"] #apps,
html[data-canvas-theme="minimalist"] #help {
  border: 1px solid #000000;
  border-radius: 0;
  background: rgba(255, 255, 255, .94);
  box-shadow: none;
  backdrop-filter: none;
  -webkit-backdrop-filter: none;
}

html[data-canvas-theme="minimalist"] #active-window-glow {
  border: 2px solid #000000;
  border-radius: 0;
  background: transparent;
  box-shadow: none;
  transition: none;
}

html[data-canvas-theme="minimalist"] .theme-chip {
  transition: none;
  border: 1px solid #000000;
  border-radius: 0;
  background: #ffffff;
  color: #000000;
}

html[data-canvas-theme="minimalist"] .theme-chip:hover {
  background: #eeeeea;
  color: #000000;
}

html[data-canvas-theme="minimalist"] .theme-chip[aria-pressed="true"] {
  border-color: #000000;
  background: #000000;
  color: #ffffff;
}

html[data-canvas-theme="minimalist"] .app:hover {
  border-color: #000000;
  background: #eeeeea;
}

html[data-canvas-theme="minimalist"] .app__icon {
  border-radius: 0;
  background: #000000;
  color: #ffffff;
}

/* Deck, gaveta de aplicativos e ferramentas de desenho. */
html[data-canvas-theme="minimalist"] #deck {
  filter: none;
}

html[data-canvas-theme="minimalist"] #app-drawer {
  border: 1px solid #000000;
  border-radius: 0;
  background: rgba(255, 255, 255, .96);
  box-shadow: none;
  backdrop-filter: none;
  -webkit-backdrop-filter: none;
}

html[data-canvas-theme="minimalist"] .deck__panel {
  background: rgba(255, 255, 255, .96);
  backdrop-filter: none;
  -webkit-backdrop-filter: none;
}

html[data-canvas-theme="minimalist"] .deck__frame {
  background: #000000;
}

html[data-canvas-theme="minimalist"] .tool {
  transition: none;
  border-radius: 0;
  color: #000000;
}

html[data-canvas-theme="minimalist"] .tool:hover {
  border-color: #000000;
  background: #eeeeea;
  color: #000000;
}

html[data-canvas-theme="minimalist"] .tool[aria-pressed="true"] {
  border-color: #000000;
  background: #000000;
  color: #ffffff;
}

html[data-canvas-theme="minimalist"] .tool--danger:hover {
  color: #000000;
}

html[data-canvas-theme="minimalist"] .swatch,
html[data-canvas-theme="minimalist"] .size,
html[data-canvas-theme="minimalist"] .apps-btn,
html[data-canvas-theme="minimalist"] #app-search {
  transition: none;
  border: 1px solid #000000;
  border-radius: 0;
  background: #ffffff;
  color: #000000;
}

html[data-canvas-theme="minimalist"] .apps-btn b {
  border-radius: 0;
  background: #000000;
  color: #ffffff;
}

html[data-canvas-theme="minimalist"] .apps-btn[aria-expanded="true"] {
  border-color: #000000;
  background: #000000;
  color: #ffffff;
}

html[data-canvas-theme="minimalist"] .apps-btn[aria-expanded="true"] b {
  background: #ffffff;
  color: #000000;
}

html[data-canvas-theme="minimalist"] .app-tile {
  border: 1px solid #000000;
  border-radius: 0;
  background: #ffffff;
  color: #000000;
}

html[data-canvas-theme="minimalist"] .app-tile:hover {
  background: #eeeeea;
}

html[data-canvas-theme="minimalist"] .app-tile__icon {
  border-radius: 0;
  background: #000000;
  color: #ffffff;
}

html[data-canvas-theme="minimalist"] .app-drawer__head,
html[data-canvas-theme="minimalist"] .app-drawer__foot {
  border-color: #000000;
}

html[data-canvas-theme="minimalist"] .draw-text-editor {
  border: 1px dashed #000000;
  border-radius: 0;
  background: #ffffff;
  color: #000000;
  box-shadow: none;
}

html[data-canvas-theme="minimalist"] #toast {
  border: 1px solid #000000;
  border-radius: 0;
  background: #ffffff;
  color: #000000;
  box-shadow: none;
  backdrop-filter: none;
  -webkit-backdrop-filter: none;
}
"#;

/// Ativa/desativa a folha de estilo injetada.
pub fn set_active(document: &Document, active: bool) -> Result<(), JsValue> {
    if active {
        inject_css(document)
    } else {
        remove_css(document)
    }
}

fn inject_css(document: &Document) -> Result<(), JsValue> {
    if document.get_element_by_id(STYLE_ID).is_some() {
        return Ok(());
    }

    let style = document.create_element("style")?;
    style.set_attribute("id", STYLE_ID)?;
    style.set_text_content(Some(MINIMALIST_CSS));

    let head = document
        .head()
        .ok_or_else(|| JsValue::from_str("elemento <head> ausente"))?;
    head.append_child(&style)?;
    Ok(())
}

fn remove_css(document: &Document) -> Result<(), JsValue> {
    if let Some(style) = document.get_element_by_id(STYLE_ID) {
        if let Some(parent) = style.parent_node() {
            parent.remove_child(&style)?;
        }
    }
    Ok(())
}

/// Desenha o papel milimetrado no canvas, acompanhando pan e zoom.
pub fn draw_graph_paper(
    context: &CanvasRenderingContext2d,
    width: f64,
    height: f64,
    offset_x: f64,
    offset_y: f64,
    scale: f64,
) -> Result<(), JsValue> {
    draw_graph_paper_with_palette(
        context,
        width,
        height,
        offset_x,
        offset_y,
        scale,
        &LIGHT_PALETTE,
    )
}

/// Desenha o papel milimetrado com uma paleta de cores arbitrária.
pub fn draw_graph_paper_with_palette(
    context: &CanvasRenderingContext2d,
    width: f64,
    height: f64,
    offset_x: f64,
    offset_y: f64,
    scale: f64,
    palette: &GraphPaperPalette,
) -> Result<(), JsValue> {
    context.set_fill_style_str(palette.paper);
    context.fill_rect(0.0, 0.0, width, height);

    let step = millimeter_step(scale);
    context.set_line_width(1.0);

    context.set_stroke_style_str(palette.minor);
    context.begin_path();
    add_lines(context, width, height, offset_x, offset_y, step);
    context.stroke();

    context.set_stroke_style_str(palette.major);
    context.begin_path();
    add_lines(context, width, height, offset_x, offset_y, step * 5.0);
    context.stroke();

    Ok(())
}

/// Mantém a grade legível em qualquer zoom, preservando a hierarquia 1:5.
fn millimeter_step(scale: f64) -> f64 {
    let mut step = MM_IN_CSS_PX * scale.max(0.01);

    while step < 2.5 {
        step *= 5.0;
    }
    while step > 120.0 {
        step /= 5.0;
    }

    step
}

fn add_lines(
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn millimeter_step_keeps_lines_visible() {
        for scale in [0.15, 0.5, 1.0, 2.0, 6.0] {
            let step = millimeter_step(scale);
            assert!((2.5..=120.0).contains(&step), "step fora da faixa: {step}");
        }
    }

    #[test]
    fn css_contains_light_monochrome_rules() {
        assert!(MINIMALIST_CSS.contains("#f7f7f2"));
        assert!(MINIMALIST_CSS.contains("#000000"));
        assert!(MINIMALIST_CSS.contains("var(--paper-mm)"));
    }
}
