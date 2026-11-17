//! Elemento de render que aplica o zoom do canvas às superfícies reais.
//!
//! O `Space` do Smithay posiciona as janelas, mas a geometria de um
//! `WaylandSurfaceRenderElement` é sempre derivada do tamanho do buffer do
//! cliente. Para o zoom do canvas ser visual (e não apenas um pan), este
//! módulo envolve cada elemento e sobrescreve `geometry()` com o fator de
//! escala do canvas. O `src()` continua apontando para o buffer inteiro, então
//! o renderer estica a textura para o retângulo menor/maior. O deslocamento de
//! subsuperfícies e popups é escalado em relação à origem da janela.

use smithay::{
    backend::renderer::{
        element::{
            surface::{render_elements_from_surface_tree, WaylandSurfaceRenderElement},
            Element, Id, Kind, RenderElement, UnderlyingStorage,
        },
        utils::{CommitCounter, OpaqueRegions},
        ImportAll, Renderer, Texture,
    },
    desktop::{PopupManager, Window},
    utils::{Buffer as BufferCoords, Physical, Point, Rectangle, Scale, Size, Transform},
    wayland::seat::WaylandFocus,
};

/// Envolve um elemento de superfície aplicando um fator de zoom uniforme.
///
/// `origin` é a posição da janela na tela. O deslocamento de cada elemento
/// (subsuperfície ou popup) é escalado em relação a essa origem, para que o
/// zoom não desloque menus e subsuperfícies.
pub struct ScaledSurfaceElement<R: Renderer> {
    inner: WaylandSurfaceRenderElement<R>,
    origin: Point<i32, Physical>,
    zoom: f64,
}

impl<R: Renderer> ScaledSurfaceElement<R> {
    pub fn new(
        inner: WaylandSurfaceRenderElement<R>,
        origin: Point<i32, Physical>,
        zoom: f64,
    ) -> Self {
        Self {
            inner,
            origin,
            zoom: zoom.clamp(0.01, 16.0),
        }
    }

    fn is_identity(&self) -> bool {
        (self.zoom - 1.0).abs() < 1e-6
    }
}

impl<R: Renderer + ImportAll> Element for ScaledSurfaceElement<R> {
    fn id(&self) -> &Id {
        self.inner.id()
    }

    fn current_commit(&self) -> CommitCounter {
        self.inner.current_commit()
    }

    fn src(&self) -> Rectangle<f64, BufferCoords> {
        self.inner.src()
    }

    fn transform(&self) -> Transform {
        self.inner.transform()
    }

    fn geometry(&self, scale: Scale<f64>) -> Rectangle<i32, Physical> {
        let geometry = self.inner.geometry(scale);
        if self.is_identity() {
            return geometry;
        }

        let offset = geometry.loc - self.origin;
        let offset = Point::from((
            (offset.x as f64 * self.zoom).round() as i32,
            (offset.y as f64 * self.zoom).round() as i32,
        ));
        let width = ((geometry.size.w as f64) * self.zoom).round() as i32;
        let height = ((geometry.size.h as f64) * self.zoom).round() as i32;
        Rectangle::new(
            self.origin + offset,
            Size::from((width.max(1), height.max(1))),
        )
    }

    fn opaque_regions(&self, scale: Scale<f64>) -> OpaqueRegions<i32, Physical> {
        // As regiões opacas do elemento interno estão em coordenadas locais
        // sem escala. Com zoom != 1 elas não podem ser usadas para podar
        // elementos; devolver vazio é conservador e evita sumiço de janelas.
        if self.is_identity() {
            self.inner.opaque_regions(scale)
        } else {
            OpaqueRegions::default()
        }
    }

    fn alpha(&self) -> f32 {
        self.inner.alpha()
    }

    fn kind(&self) -> Kind {
        self.inner.kind()
    }
}

impl<R> RenderElement<R> for ScaledSurfaceElement<R>
where
    R: Renderer + ImportAll,
    R::TextureId: Texture + 'static,
{
    fn draw(
        &self,
        frame: &mut R::Frame<'_, '_>,
        src: Rectangle<f64, BufferCoords>,
        dst: Rectangle<i32, Physical>,
        damage: &[Rectangle<i32, Physical>],
        opaque_regions: &[Rectangle<i32, Physical>],
    ) -> Result<(), R::Error> {
        // `dst` já chega com o tamanho escalado; o dano também está nas
        // coordenadas locais do destino, então basta repassar.
        self.inner
            .draw(frame, src, dst, damage, opaque_regions)
    }

    fn underlying_storage(&self, renderer: &mut R) -> Option<UnderlyingStorage<'_>> {
        self.inner.underlying_storage(renderer)
    }
}

/// Constrói os elementos de uma janela (superfície principal + popups)
/// aplicando o zoom do canvas. A origem da janela é comum a todos os
/// elementos; o wrapper escala tamanho e deslocamento em relação a ela, o que
/// mantém menus e subsuperfícies ancorados quando o canvas tem zoom != 100%.
pub fn scaled_window_elements<R>(
    renderer: &mut R,
    window: &Window,
    location: Point<i32, Physical>,
    zoom: f64,
) -> Vec<ScaledSurfaceElement<R>>
where
    R: Renderer + ImportAll,
    R::TextureId: Clone + 'static,
{
    let Some(surface) = window.wl_surface().map(|surface| surface.into_owned()) else {
        return Vec::new();
    };

    let mut elements = Vec::new();
    let window_geometry = window.geometry();

    for (popup, popup_offset) in PopupManager::popups_for_surface(&surface) {
        let offset: Point<i32, Physical> =
            (window_geometry.loc + popup_offset - popup.geometry().loc)
                .to_physical_precise_round(1.0);
        let popup_elements: Vec<WaylandSurfaceRenderElement<R>> =
            render_elements_from_surface_tree(
                renderer,
                popup.wl_surface(),
                location + offset,
                Scale::from(1.0),
                1.0,
                Kind::Unspecified,
            );
        elements.extend(
            popup_elements
                .into_iter()
                .map(|element| ScaledSurfaceElement::new(element, location, zoom)),
        );
    }

    let surface_elements: Vec<WaylandSurfaceRenderElement<R>> = render_elements_from_surface_tree(
        renderer,
        &surface,
        location,
        Scale::from(1.0),
        1.0,
        Kind::Unspecified,
    );
    elements.extend(
        surface_elements
            .into_iter()
            .map(|element| ScaledSurfaceElement::new(element, location, zoom)),
    );

    elements
}
