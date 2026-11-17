//! Tema Minimalista Negro: modo escuro monocromático.
//!
//! Mantém a mesma linguagem do Minimalista claro — contornos sólidos, sem
//! sombras e papel milimetrado — mas inverte a paleta: fundo `#000000`,
//! texto/bordas `#ffffff` e grid branco de baixa opacidade.

use wasm_bindgen::JsValue;
use web_sys::{CanvasRenderingContext2d, Document};

use crate::minimalist_theme::{draw_graph_paper_with_palette, GraphPaperPalette};

const STYLE_ID: &str = "spatial-deck-minimalist-dark-theme";
const DARK_PALETTE: GraphPaperPalette = GraphPaperPalette {
    paper: "#000000",
    minor: "rgba(255, 255, 255, 0.075)",
    major: "rgba(255, 255, 255, 0.18)",
};

/// Regras monocromáticas do modo escuro.
pub const MINIMALIST_DARK_CSS: &str = r#"
html[data-canvas-theme="minimalist-dark"] {
  --bg-0: #000000;
  --ink: #ffffff;
  --muted: #b3b3b3;
  --line: #ffffff;
  --accent: #ffffff;
  --accent-2: #ffffff;
  --accent-ink: #000000;
  --hud-bg: rgba(0, 0, 0, 0.94);
  --bg-glow-1: #111111;
  --bg-glow-2: #050505;
  --glow-border: #ffffff;
  --glow-shadow: none;
  --glow-bg: transparent;
  --chip-bg: #000000;
  --chip-bg-hover: #151515;
  --chip-line-hover: #ffffff;
  --card: #000000;
  --card-line: #ffffff;
  --paper-mm: 3.7795px;
}

html[data-canvas-theme="minimalist-dark"] body {
  color: #ffffff;
  background-color: #000000;
  background-image:
    repeating-linear-gradient(to right, rgba(255, 255, 255, .065) 0 1px, transparent 1px var(--paper-mm)),
    repeating-linear-gradient(to bottom, rgba(255, 255, 255, .065) 0 1px, transparent 1px var(--paper-mm)),
    repeating-linear-gradient(to right, rgba(255, 255, 255, .16) 0 1px, transparent 1px calc(var(--paper-mm) * 5)),
    repeating-linear-gradient(to bottom, rgba(255, 255, 255, .16) 0 1px, transparent 1px calc(var(--paper-mm) * 5)),
    radial-gradient(circle at 50% 0%, #0a0a0a 0%, transparent 62%);
}

html[data-canvas-theme="minimalist-dark"] #grid {
  background: transparent;
}

html[data-canvas-theme="minimalist-dark"][data-grid="off"] body {
  background-image: none;
}

html[data-canvas-theme="minimalist-dark"] .window-card {
  border: 1px solid #ffffff;
  border-radius: 0;
  background: #000000;
  box-shadow: none;
  backdrop-filter: none;
  -webkit-backdrop-filter: none;
  transition: none;
}

html[data-canvas-theme="minimalist-dark"] .window-card:hover {
  border-color: #ffffff;
  box-shadow: 0 0 0 1px #ffffff;
}

html[data-canvas-theme="minimalist-dark"] .window-card__bar {
  border-bottom: 1px solid #ffffff;
  background: #000000;
  color: #b3b3b3;
}

html[data-canvas-theme="minimalist-dark"] .window-card__bar b,
html[data-canvas-theme="minimalist-dark"] .window-card__body h2,
html[data-canvas-theme="minimalist-dark"] .brand,
html[data-canvas-theme="minimalist-dark"] .zoom b {
  color: #ffffff;
}

html[data-canvas-theme="minimalist-dark"] .dots i,
html[data-canvas-theme="minimalist-dark"] .nodes li span,
html[data-canvas-theme="minimalist-dark"] .swatches i {
  background: #ffffff;
  opacity: 1;
}

html[data-canvas-theme="minimalist-dark"] .dots i:nth-child(2),
html[data-canvas-theme="minimalist-dark"] .dots i:nth-child(3),
html[data-canvas-theme="minimalist-dark"] .swatches i:nth-child(2),
html[data-canvas-theme="minimalist-dark"] .swatches i:nth-child(3),
html[data-canvas-theme="minimalist-dark"] .swatches i:nth-child(4) {
  background: #b3b3b3;
}

html[data-canvas-theme="minimalist-dark"] .window-card__body pre,
html[data-canvas-theme="minimalist-dark"] .badge,
html[data-canvas-theme="minimalist-dark"] .app {
  border: 1px solid #ffffff;
  border-radius: 0;
  background: #000000;
  color: #ffffff;
}

html[data-canvas-theme="minimalist-dark"] .window-card__body pre {
  box-shadow: none;
}

html[data-canvas-theme="minimalist-dark"] #hud,
html[data-canvas-theme="minimalist-dark"] #apps,
html[data-canvas-theme="minimalist-dark"] #help {
  border: 1px solid #ffffff;
  border-radius: 0;
  background: rgba(0, 0, 0, .94);
  box-shadow: none;
  backdrop-filter: none;
  -webkit-backdrop-filter: none;
}

html[data-canvas-theme="minimalist-dark"] #active-window-glow {
  border: 2px solid #ffffff;
  border-radius: 0;
  background: transparent;
  box-shadow: none;
  transition: none;
}

html[data-canvas-theme="minimalist-dark"] .theme-chip {
  transition: none;
  border: 1px solid #ffffff;
  border-radius: 0;
  background: #000000;
  color: #ffffff;
}

html[data-canvas-theme="minimalist-dark"] .theme-chip:hover {
  background: #151515;
  color: #ffffff;
}

html[data-canvas-theme="minimalist-dark"] .theme-chip[aria-pressed="true"] {
  border-color: #ffffff;
  background: #ffffff;
  color: #000000;
}

html[data-canvas-theme="minimalist-dark"] .app:hover {
  border-color: #ffffff;
  background: #151515;
}

html[data-canvas-theme="minimalist-dark"] .app__icon {
  border-radius: 0;
  background: #ffffff;
  color: #000000;
}

/* Deck, gaveta de aplicativos e ferramentas de desenho. */
html[data-canvas-theme="minimalist-dark"] #deck {
  filter: none;
}

html[data-canvas-theme="minimalist-dark"] #app-drawer {
  border: 1px solid #ffffff;
  border-radius: 0;
  background: rgba(0, 0, 0, .96);
  box-shadow: none;
  backdrop-filter: none;
  -webkit-backdrop-filter: none;
}

html[data-canvas-theme="minimalist-dark"] .deck__panel {
  background: rgba(0, 0, 0, .96);
  backdrop-filter: none;
  -webkit-backdrop-filter: none;
}

html[data-canvas-theme="minimalist-dark"] .deck__frame {
  background: #ffffff;
}

html[data-canvas-theme="minimalist-dark"] .tool {
  transition: none;
  border-radius: 0;
  color: #ffffff;
}

html[data-canvas-theme="minimalist-dark"] .tool:hover {
  border-color: #ffffff;
  background: #151515;
  color: #ffffff;
}

html[data-canvas-theme="minimalist-dark"] .tool[aria-pressed="true"] {
  border-color: #ffffff;
  background: #ffffff;
  color: #000000;
}

html[data-canvas-theme="minimalist-dark"] .tool--danger:hover {
  color: #ffffff;
}

html[data-canvas-theme="minimalist-dark"] .swatch,
html[data-canvas-theme="minimalist-dark"] .size,
html[data-canvas-theme="minimalist-dark"] .apps-btn,
html[data-canvas-theme="minimalist-dark"] #app-search {
  transition: none;
  border: 1px solid #ffffff;
  border-radius: 0;
  background: #000000;
  color: #ffffff;
}

html[data-canvas-theme="minimalist-dark"] .apps-btn b {
  border-radius: 0;
  background: #ffffff;
  color: #000000;
}

html[data-canvas-theme="minimalist-dark"] .apps-btn[aria-expanded="true"] {
  border-color: #ffffff;
  background: #ffffff;
  color: #000000;
}

html[data-canvas-theme="minimalist-dark"] .apps-btn[aria-expanded="true"] b {
  background: #000000;
  color: #ffffff;
}

html[data-canvas-theme="minimalist-dark"] .app-tile {
  border: 1px solid #ffffff;
  border-radius: 0;
  background: #000000;
  color: #ffffff;
}

html[data-canvas-theme="minimalist-dark"] .app-tile:hover {
  background: #151515;
}

html[data-canvas-theme="minimalist-dark"] .app-tile__icon {
  border-radius: 0;
  background: #ffffff;
  color: #000000;
}

html[data-canvas-theme="minimalist-dark"] .app-drawer__head,
html[data-canvas-theme="minimalist-dark"] .app-drawer__foot {
  border-color: #ffffff;
}

html[data-canvas-theme="minimalist-dark"] .draw-text-editor {
  border: 1px dashed #ffffff;
  border-radius: 0;
  background: #000000;
  color: #ffffff;
  box-shadow: none;
}

html[data-canvas-theme="minimalist-dark"] #toast {
  border: 1px solid #ffffff;
  border-radius: 0;
  background: #000000;
  color: #ffffff;
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
    style.set_text_content(Some(MINIMALIST_DARK_CSS));

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

/// Desenha o papel milimetrado branco sobre fundo negro.
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
        &DARK_PALETTE,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn css_contains_black_and_white_monochrome_rules() {
        assert!(MINIMALIST_DARK_CSS.contains("#000000"));
        assert!(MINIMALIST_DARK_CSS.contains("#ffffff"));
        assert!(MINIMALIST_DARK_CSS.contains("var(--paper-mm)"));
    }
}
