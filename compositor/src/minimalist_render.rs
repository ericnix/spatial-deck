//! Renderizador de bordas dos temas Minimalistas.
//!
//! O tema claro usa contornos pretos sólidos e o tema negro usa contornos
//! brancos sólidos, sem sombras nem gradientes. As bordas são desenhadas
//! diretamente no frame OpenGL, depois das superfícies das janelas, para
//! ficarem sempre visíveis.

use smithay::{
    backend::renderer::{Color32F, Frame},
    utils::{Physical, Point, Rectangle, Size},
};

/// Cor sólida dos contornos do Minimalista claro (`#000000`).
pub const OUTLINE_COLOR: Color32F = Color32F::new(0.0, 0.0, 0.0, 1.0);

/// Cor sólida dos contornos do Minimalista Negro (`#ffffff`).
pub const OUTLINE_COLOR_DARK: Color32F = Color32F::new(1.0, 1.0, 1.0, 1.0);

/// Espessura do contorno em pixels físicos.
pub const OUTLINE_THICKNESS: i32 = 2;

/// Desenha um contorno sólido ao redor de cada janela.
pub fn draw_minimalist_borders<F>(
    frame: &mut F,
    output_size: Size<i32, Physical>,
    window_rects: impl IntoIterator<Item = Rectangle<i32, Physical>>,
    color: Color32F,
) -> Result<(), F::Error>
where
    F: Frame,
{
    let output = Rectangle::from_size(output_size);

    for rect in window_rects {
        if rect.size.w <= 0 || rect.size.h <= 0 {
            continue;
        }

        for border in border_rects(rect, OUTLINE_THICKNESS) {
            if let Some(visible) = border.intersection(output) {
                // `draw_solid` recebe o alvo completo e a região danificada.
                // Assim a borda é recortada pelas dimensões do output.
                frame.draw_solid(output, &[visible], color)?;
            }
        }
    }

    Ok(())
}

/// Quatro retângulos sólidos que formam o contorno da janela.
fn border_rects(
    rect: Rectangle<i32, Physical>,
    thickness: i32,
) -> [Rectangle<i32, Physical>; 4] {
    let top_h = thickness.max(1).min(rect.size.h);
    let side_w = thickness.max(1).min(rect.size.w);
    let side_y = rect.loc.y + top_h;
    let side_h = (rect.size.h - top_h * 2).max(0);
    let bottom_y = rect.loc.y + rect.size.h - top_h;
    let right_x = rect.loc.x + rect.size.w - side_w;

    [
        Rectangle::new(rect.loc, Size::from((rect.size.w, top_h))),
        Rectangle::new(
            Point::from((rect.loc.x, bottom_y)),
            Size::from((rect.size.w, top_h)),
        ),
        Rectangle::new(
            Point::from((rect.loc.x, side_y)),
            Size::from((side_w, side_h)),
        ),
        Rectangle::new(
            Point::from((right_x, side_y)),
            Size::from((side_w, side_h)),
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn border_rects_form_a_solid_frame() {
        let rect = Rectangle::<i32, Physical>::new(
            Point::from((10, 20)),
            Size::from((100, 50)),
        );
        let borders = border_rects(rect, 2);

        assert_eq!(
            borders[0],
            Rectangle::new(Point::from((10, 20)), Size::from((100, 2)))
        );
        assert_eq!(
            borders[1],
            Rectangle::new(Point::from((10, 68)), Size::from((100, 2)))
        );
        assert_eq!(
            borders[2],
            Rectangle::new(Point::from((10, 22)), Size::from((2, 46)))
        );
        assert_eq!(
            borders[3],
            Rectangle::new(Point::from((108, 22)), Size::from((2, 46)))
        );
    }
}
