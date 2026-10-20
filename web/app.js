/* =========================================================
   Canva Infinito SVG
   - Canvas infinito (pan/zoom) desenhando em SVG
   - Ferramentas: caneta, linha, retângulo, elipse, texto,
     seleção/mover e borracha
   - Deck trapezoidal na parte inferior (ferramentas, estilo, ações)
   ========================================================= */
(() => {
  'use strict';

  const NS = 'http://www.w3.org/2000/svg';
  const $ = (sel, root = document) => root.querySelector(sel);
  const $$ = (sel, root = document) => [...root.querySelectorAll(sel)];

  /* ----------------------- DOM ----------------------- */
  const board    = $('#board');
  const stage    = $('#stage');
  const viewport = $('#viewport');
  const layer    = $('#layer');
  const overlay  = $('#overlay');

  const pMinor     = $('#p-minor');
  const pMajor     = $('#p-major');
  const pDot       = $('#p-dot');
  const pMajorPath = $('#p-major-path');
  const gridLines  = $('#grid-lines');
  const gridDots   = $('#grid-dots');

  const zoomValue  = $('#zoom-value');
  const coordX     = $('#coords-x');
  const coordY     = $('#coords-y');
  const toastEl    = $('#toast');
  const colorInput = $('#color');
  const sizeInput  = $('#size');
  const sizeOut    = $('#size-out');
  const swatchChip = $('.swatch__chip');

  /* ----------------------- constantes / estado ----------------------- */
  const MIN_K = 0.12;
  const MAX_K = 8;
  const BASE_STEP = 40;          // passo base da grade em unidades de mundo
  const MAX_HISTORY = 90;

  const view = { x: 0, y: 0, k: 1 };                 // transform: screen = world * k + (x, y)
  const tool = { name: 'pen', color: '#7dd3fc', size: 3, grid: true, space: false };

  const selection = new Set();
  const pointers = new Map();     // pointerId -> {x,y} (coordenadas de cliente)

  let gesture = null;             // gesto em andamento (desenho, pan, mover, apagar)
  let pinch = null;               // estado do pinch (dois dedos)
  let renderQueued = false;

  const history = { stack: [], index: -1 };

  /* ----------------------- utilidades ----------------------- */
  const clamp = (v, a, b) => Math.min(b, Math.max(a, v));
  const r2 = n => Math.round(n * 100) / 100;
  const mod = (n, m) => ((n % m) + m) % m;
  const dist = (a, b) => Math.hypot(a.x - b.x, a.y - b.y);

  function localPoint(e) {
    const r = board.getBoundingClientRect();
    return { x: e.clientX - r.left, y: e.clientY - r.top };
  }
  const screenToWorld = p => ({ x: (p.x - view.x) / view.k, y: (p.y - view.y) / view.k });

  /* ----------------------- view (pan / zoom) ----------------------- */
  function zoomAt(sx, sy, factor) {
    const k = clamp(view.k * factor, MIN_K, MAX_K);
    if (k === view.k) return;
    const wx = (sx - view.x) / view.k;
    const wy = (sy - view.y) / view.k;
    view.k = k;
    view.x = sx - wx * k;
    view.y = sy - wy * k;
    scheduleRender();
  }

  function resetView() {
    view.k = 1;
    view.x = board.clientWidth / 2;
    view.y = board.clientHeight / 2;
    scheduleRender();
  }

  function fitView() {
    if (!layer.children.length) { resetView(); toast('Nada desenhado ainda — enquadrado na origem.'); return; }
    const b = layer.getBBox();
    const pad = 90;
    const vw = board.clientWidth;
    const vh = board.clientHeight;
    const k = clamp(Math.min((vw - pad * 2) / Math.max(b.width, 1), (vh - pad * 2) / Math.max(b.height, 1)), MIN_K, MAX_K);
    view.k = k;
    view.x = vw / 2 - (b.x + b.width / 2) * k;
    view.y = vh / 2 - (b.y + b.height / 2) * k;
    scheduleRender();
  }

  /* ----------------------- render ----------------------- */
  function scheduleRender() {
    if (renderQueued) return;
    renderQueued = true;
    requestAnimationFrame(() => { renderQueued = false; render(); });
  }

  function render() {
    viewport.setAttribute('transform', `translate(${r2(view.x)} ${r2(view.y)}) scale(${view.k})`);

    // passo adaptativo: mantém a grade legível em qualquer zoom
    let step = BASE_STEP;
    while (step * view.k < 16 && step > 0.6) step /= 4;
    while (step * view.k > 220) step *= 4;

    const cell = step * view.k;            // tamanho da célula em px de tela
    const major = cell * 4;
    const half = cell / 2;

    // pontos: definir centro do círculo e alinhar o tile com a origem do mundo
    pMinor.setAttribute('width', cell);
    pMinor.setAttribute('height', cell);
    pDot.setAttribute('cx', half);
    pDot.setAttribute('cy', half);
    pMinor.setAttribute('patternTransform', `translate(${r2(mod(view.x - half, cell))} ${r2(mod(view.y - half, cell))})`);

    // linhas maiores
    pMajor.setAttribute('width', major);
    pMajor.setAttribute('height', major);
    pMajorPath.setAttribute('d', `M0 0 H${r2(major)} M0 0 V${r2(major)}`);
    pMajor.setAttribute('patternTransform', `translate(${r2(mod(view.x, major))} ${r2(mod(view.y, major))})`);

    zoomValue.textContent = Math.round(view.k * 100) + '%';
    drawOverlay();
    editor.layout();
  }

  /* ----------------------- histórico ----------------------- */
  function snapshot() { return layer.innerHTML; }

  function commit() {
    const snap = snapshot();
    if (history.stack[history.index] === snap) return;   // nada mudou: não polui o histórico
    history.stack.length = history.index + 1;
    history.stack.push(snap);
    if (history.stack.length > MAX_HISTORY) history.stack.shift();
    history.index = history.stack.length - 1;
  }

  function restore(index) {
    const next = history.stack[index];
    if (next === undefined) return;
    history.index = index;
    layer.innerHTML = next;
    selection.forEach(el => el.removeAttribute && el.removeAttribute('data-selected'));
    selection.clear();
    drawOverlay();
    scheduleRender();
  }

  const undo = () => { if (history.index > 0) restore(history.index - 1); else toast('Nada para desfazer.'); };
  const redo = () => { if (history.index < history.stack.length - 1) restore(history.index + 1); else toast('Nada para refazer.'); };

  /* ----------------------- formas ----------------------- */
  function attrs(el, map) { for (const k in map) el.setAttribute(k, map[k]); return el; }

  function strokeAttrs() {
    return {
      fill: 'none',
      stroke: tool.color,
      'stroke-width': tool.size,
      'stroke-linecap': 'round',
      'stroke-linejoin': 'round',
    };
  }

  function makeShape(tag, map) {
    const el = document.createElementNS(NS, tag);
    attrs(el, map);
    el.setAttribute('data-shape', '1');
    layer.appendChild(el);
    return el;
  }

  // curva suavizada por pontos médios (freehand)
  function pathFrom(pts) {
    if (!pts.length) return '';
    if (pts.length === 1) return `M${r2(pts[0].x)} ${r2(pts[0].y)} L${r2(pts[0].x)} ${r2(pts[0].y)}`;
    if (pts.length === 2) return `M${r2(pts[0].x)} ${r2(pts[0].y)} L${r2(pts[1].x)} ${r2(pts[1].y)}`;
    let d = `M${r2(pts[0].x)} ${r2(pts[0].y)}`;
    for (let i = 1; i < pts.length - 1; i++) {
      const mx = (pts[i].x + pts[i + 1].x) / 2;
      const my = (pts[i].y + pts[i + 1].y) / 2;
      d += ` Q${r2(pts[i].x)} ${r2(pts[i].y)} ${r2(mx)} ${r2(my)}`;
    }
    const last = pts[pts.length - 1];
    return d + ` L${r2(last.x)} ${r2(last.y)}`;
  }

  /* ----------------------- texto (editor embutido) ----------------------- */
  /* Não usamos window.prompt(): ele é bloqueado em navegadores embarcados
     (Electron/webview) e falha em silêncio. O texto é editado no próprio
     canvas, com um <textarea> sobreposto alinhado ao ponto clicado.        */

  const DEFAULT_FONT = 'ui-sans-serif, system-ui, "Segoe UI", Roboto, Arial, sans-serif';
  const LINE_RATIO = 1.25;                       // altura de linha (relativa ao font-size)
  const fontFor = () => Math.max(14, tool.size * 7);

  // lê o conteúdo do <text> (uma linha por <tspan>)
  function textOf(el) {
    const spans = [...el.querySelectorAll('tspan')];
    return spans.length ? spans.map(s => s.textContent).join('\n') : (el.textContent || '');
  }

  // escreve o conteúdo: uma linha por <tspan>, com x fixo para preservar o alinhamento
  function applyText(el, value, fontSize) {
    const x = el.getAttribute('x') || 0;
    while (el.firstChild) el.removeChild(el.firstChild);
    value.replace(/\r/g, '').split('\n').forEach((line, i) => {
      const tspan = document.createElementNS(NS, 'tspan');
      tspan.setAttribute('x', x);
      if (i > 0) tspan.setAttribute('dy', r2(fontSize * LINE_RATIO));
      tspan.textContent = line;
      el.appendChild(tspan);
    });
  }

  // posição no mundo considerando o deslocamento (arraste) do elemento
  function worldAnchor(el) {
    const t = readTranslate(el);
    return { x: (+el.getAttribute('x') || 0) + t.x, y: (+el.getAttribute('y') || 0) + t.y };
  }

  const editor = (() => {
    const ta = document.createElement('textarea');
    ta.className = 'texteditor';
    ta.rows = 1;
    ta.spellcheck = false;
    ta.placeholder = 'Digite…';
    ta.setAttribute('aria-label', 'Editar texto');

    let open = false;
    let target = null;
    let anchor = { x: 0, y: 0 };
    let fontSize = 21;
    let isNew = false;
    let original = '';

    function layout() {
      if (!open || !target || !target.isConnected) return;
      const k = view.k;
      const line = fontSize * LINE_RATIO * k;
      const pad = 2;
      ta.style.left = `${anchor.x * k + view.x - pad}px`;
      ta.style.top = `${anchor.y * k + view.y - line / 2 - pad}px`;
      ta.style.fontSize = `${fontSize * k}px`;
      ta.style.color = target.getAttribute('fill') || tool.color;
      ta.style.minWidth = `${Math.max(90, fontSize * k * 4)}px`;
      ta.style.height = 'auto';
      ta.style.height = `${Math.max(ta.scrollHeight, line + pad * 2)}px`;
    }

    function begin(el, world, created) {
      if (open) close(true);
      target = el;
      anchor = { x: world.x, y: world.y };
      fontSize = +el.getAttribute('font-size') || fontFor();
      isNew = !!created;
      original = textOf(el);
      open = true;

      // esconde o <text> real: durante a edição o usuário vê só o textarea
      el.setAttribute('data-editing', '1');
      ta.value = original;
      stage.appendChild(ta);
      layout();
      ta.focus({ preventScroll: true });
      ta.setSelectionRange(ta.value.length, ta.value.length);
    }

    function close(save = true) {
      if (!open) return;
      open = false;

      const el = target;
      const value = save ? ta.value.replace(/\r/g, '') : original;
      target = null;
      if (ta.parentNode) ta.parentNode.removeChild(ta);

      if (el && el.isConnected) {
        el.removeAttribute('data-editing');
        if (!save) {
          if (isNew) el.remove();
          else applyText(el, original, fontSize);
        } else if (value.trim() === '') {
          el.remove();
        } else {
          applyText(el, value, fontSize);
          el.setAttribute('fill', el.getAttribute('fill') || tool.color);
        }
      }

      isNew = false;
      commit();                       // histórico (ignora se nada mudou)
      drawOverlay();
    }

    ta.addEventListener('input', () => { layout(); });
    ta.addEventListener('keydown', e => {
      e.stopPropagation();
      if (e.key === 'Escape') { e.preventDefault(); close(false); toast('Texto cancelado.'); }
      else if (e.key === 'Enter' && !e.shiftKey) { e.preventDefault(); close(true); }
    });
    ta.addEventListener('blur', () => close(true));

    // Conclui a edição ao clicar em qualquer lugar fora do textarea.
    // Não dependemos só do "blur": ele não dispara de forma confiável quando
    // a janela perde o foco (navegador embarcado, troca de aba, etc.).
    document.addEventListener('pointerdown', e => {
      if (open && e.target !== ta && !ta.contains(e.target)) close(true);
    }, true);

    return {
      node: ta,
      isOpen: () => open,
      layout,
      close,
      beginNew(world) {
        const el = makeShape('text', {
          x: r2(world.x), y: r2(world.y),
          fill: tool.color,
          'font-size': fontFor(),
          'font-family': DEFAULT_FONT,
          'dominant-baseline': 'middle',
          'xml:space': 'preserve',
        });
        begin(el, world, true);
        return el;
      },
      edit(el) { begin(el, worldAnchor(el), false); },
    };
  })();

  /* ----------------------- seleção ----------------------- */
  function syncSelection() {
    layer.querySelectorAll('[data-selected="true"]').forEach(el => {
      if (!selection.has(el)) el.removeAttribute('data-selected');
    });
    selection.forEach(el => el.setAttribute('data-selected', 'true'));
    drawOverlay();
  }

  function drawOverlay() {
    while (overlay.firstChild) overlay.removeChild(overlay.firstChild);
    if (!selection.size) return;
    const k = view.k;
    selection.forEach(el => {
      if (!el.isConnected) return;
      let b;
      try { b = el.getBBox(); } catch (_) { return; }
      const pad = 5 / k;
      const rect = document.createElementNS(NS, 'rect');
      attrs(rect, {
        x: b.x - pad, y: b.y - pad,
        width: Math.max(b.width + pad * 2, 1 / k),
        height: Math.max(b.height + pad * 2, 1 / k),
        fill: 'none',
        stroke: '#5eead4',
        'stroke-width': 1 / k,
        'stroke-dasharray': `${6 / k} ${4 / k}`,
        'pointer-events': 'none',
      });
      overlay.appendChild(rect);
    });
  }

  function readTranslate(el) {
    const m = /translate\(\s*(-?[\d.]+)[ ,]+(-?[\d.]+)\s*\)/.exec(el.getAttribute('transform') || '');
    return m ? { x: +m[1], y: +m[2] } : { x: 0, y: 0 };
  }

  function startMove(w) {
    gesture = {
      kind: 'move',
      from: w,
      dirty: false,
      items: [...selection].map(el => ({ el, base: readTranslate(el) })),
    };
  }

  function deleteSelection() {
    if (!selection.size) return;
    selection.forEach(el => el.remove());
    selection.clear();
    overlay.innerHTML = '';
    commit();
  }

  /* ----------------------- interação (ponteiro) ----------------------- */
  function onPointerDown(e) {
    if (e.button === 2) return;               // botão direito: menu do sistema
    try { board.setPointerCapture(e.pointerId); } catch (_) { /* ponteiro não capturável */ }
    pointers.set(e.pointerId, { x: e.clientX, y: e.clientY });

    if (pointers.size === 2) { beginPinch(); return; }
    if (pointers.size > 2) return;

    const p = localPoint(e);
    const w = screenToWorld(p);
    const wantsPan = e.button === 1 || tool.name === 'pan' || tool.space;

    if (editor.isOpen()) editor.close(true);   // clicar fora conclui a edição de texto

    if (wantsPan) {
      gesture = { kind: 'pan', cx: e.clientX, cy: e.clientY, ox: view.x, oy: view.y };
      return;
    }

    switch (tool.name) {
      case 'select': {
        const target = e.target.closest && e.target.closest('#layer [data-shape]');
        if (target) {
          if (e.shiftKey) {
            selection.has(target) ? selection.delete(target) : selection.add(target);
          } else if (!selection.has(target)) {
            selection.clear();
            selection.add(target);
          }
          syncSelection();
          if (selection.size) startMove(w);
        } else {
          if (!e.shiftKey) { selection.clear(); syncSelection(); }
          gesture = { kind: 'pan', cx: e.clientX, cy: e.clientY, ox: view.x, oy: view.y };
        }
        break;
      }
      case 'eraser':
        gesture = { kind: 'erase', dirty: false };
        eraseAt(e.clientX, e.clientY);
        break;
      case 'pen':
        gesture = { kind: 'pen', el: makeShape('path', { ...strokeAttrs(), d: pathFrom([w]) }), pts: [w], dirty: true };
        break;
      case 'line':
        gesture = { kind: 'line', el: makeShape('line', { ...strokeAttrs(), x1: r2(w.x), y1: r2(w.y), x2: r2(w.x), y2: r2(w.y) }), from: w, dirty: true };
        break;
      case 'rect':
        gesture = { kind: 'rect', el: makeShape('rect', { ...strokeAttrs(), x: r2(w.x), y: r2(w.y), width: 0, height: 0 }), from: w, dirty: true };
        break;
      case 'ellipse':
        gesture = { kind: 'ellipse', el: makeShape('ellipse', { ...strokeAttrs(), cx: r2(w.x), cy: r2(w.y), rx: 0, ry: 0 }), from: w, dirty: true };
        break;
      case 'text': {
        const onText = e.target.closest && e.target.closest('#layer text[data-shape]');
        if (onText) editor.edit(onText);
        else editor.beginNew(w);
        break;
      }
    }
  }

  function onPointerMove(e) {
    if (pointers.has(e.pointerId)) pointers.set(e.pointerId, { x: e.clientX, y: e.clientY });

    const p = localPoint(e);
    const w = screenToWorld(p);
    coordX.textContent = Math.round(w.x);
    coordY.textContent = Math.round(w.y);

    if (pinch) { updatePinch(); return; }
    if (!gesture) return;

    switch (gesture.kind) {
      case 'pan':
        view.x = gesture.ox + (e.clientX - gesture.cx);
        view.y = gesture.oy + (e.clientY - gesture.cy);
        scheduleRender();
        break;

      case 'pen':
        if (gesture.pts.length > 1 && dist(gesture.pts[gesture.pts.length - 1], w) < 1.2 / view.k) break;
        gesture.pts.push(w);
        gesture.el.setAttribute('d', pathFrom(gesture.pts));
        break;

      case 'line':
        gesture.el.setAttribute('x2', r2(w.x));
        gesture.el.setAttribute('y2', r2(w.y));
        break;

      case 'rect': {
        let dx = w.x - gesture.from.x;
        let dy = w.y - gesture.from.y;
        if (e.shiftKey) {                       // shift = quadrado
          const s = Math.max(Math.abs(dx), Math.abs(dy));
          dx = Math.sign(dx || 1) * s;
          dy = Math.sign(dy || 1) * s;
        }
        gesture.el.setAttribute('x', r2(Math.min(gesture.from.x, gesture.from.x + dx)));
        gesture.el.setAttribute('y', r2(Math.min(gesture.from.y, gesture.from.y + dy)));
        gesture.el.setAttribute('width', r2(Math.abs(dx)));
        gesture.el.setAttribute('height', r2(Math.abs(dy)));
        break;
      }

      case 'ellipse': {
        let dx = w.x - gesture.from.x;
        let dy = w.y - gesture.from.y;
        if (e.shiftKey) {                       // shift = círculo
          const s = Math.max(Math.abs(dx), Math.abs(dy));
          dx = Math.sign(dx || 1) * s;
          dy = Math.sign(dy || 1) * s;
        }
        gesture.el.setAttribute('cx', r2(gesture.from.x + dx / 2));
        gesture.el.setAttribute('cy', r2(gesture.from.y + dy / 2));
        gesture.el.setAttribute('rx', r2(Math.abs(dx) / 2));
        gesture.el.setAttribute('ry', r2(Math.abs(dy) / 2));
        break;
      }

      case 'move': {
        const dx = w.x - gesture.from.x;
        const dy = w.y - gesture.from.y;
        if (Math.abs(dx) > 0.5 / view.k || Math.abs(dy) > 0.5 / view.k) gesture.dirty = true;
        gesture.items.forEach(({ el, base }) => {
          el.setAttribute('transform', `translate(${r2(base.x + dx)} ${r2(base.y + dy)})`);
        });
        drawOverlay();
        break;
      }

      case 'erase':
        eraseAt(e.clientX, e.clientY);
        break;
    }
  }

  function onPointerUp(e) {
    pointers.delete(e.pointerId);
    try {
      if (board.hasPointerCapture && board.hasPointerCapture(e.pointerId)) board.releasePointerCapture(e.pointerId);
    } catch (_) { /* ignora ponteiros já liberados */ }
    if (pointers.size < 2) pinch = null;
    if (!gesture) return;

    const g = gesture;
    gesture = null;

    // formas criadas "sem arrastar" não devem virar lixo geométrico
    if (g.kind === 'pen' && g.pts.length === 1) {
      g.el.setAttribute('d', pathFrom([g.pts[0], g.pts[0]]));
    }
    if (g.kind === 'rect' || g.kind === 'ellipse') {
      const zero = g.kind === 'rect'
        ? (+g.el.getAttribute('width') < 1.5 && +g.el.getAttribute('height') < 1.5)
        : (+g.el.getAttribute('rx') < 1 && +g.el.getAttribute('ry') < 1);
      if (zero) { g.el.remove(); g.dirty = false; }
    }
    if (g.kind === 'line') {
      const d = Math.hypot(+g.el.getAttribute('x2') - +g.el.getAttribute('x1'), +g.el.getAttribute('y2') - +g.el.getAttribute('y1'));
      if (d < 2) { g.el.remove(); g.dirty = false; }
    }

    if (g.dirty) commit();
    else if (g.kind === 'move') drawOverlay();
  }

  function cancelGesture() {
    if (gesture && gesture.el && (gesture.kind === 'pen' || gesture.kind === 'line' || gesture.kind === 'rect' || gesture.kind === 'ellipse')) {
      gesture.el.remove();
    }
    gesture = null;
  }

  function eraseAt(cx, cy) {
    const el = document.elementFromPoint(cx, cy);
    const shape = el && el.closest ? el.closest('#layer [data-shape]') : null;
    if (!shape) return;
    selection.delete(shape);
    shape.remove();
    if (gesture) gesture.dirty = true;
    drawOverlay();
  }

  /* ----------------------- pinch (dois dedos) ----------------------- */
  function beginPinch() {
    const [a, b] = [...pointers.values()];
    pinch = { d: dist(a, b), k: view.k, cx: (a.x + b.x) / 2, cy: (a.y + b.y) / 2, vx: view.x, vy: view.y };
    cancelGesture();
  }

  function updatePinch() {
    const pts = [...pointers.values()];
    if (pts.length < 2) return;
    const [a, b] = pts;
    const rect = board.getBoundingClientRect();
    const cx = (a.x + b.x) / 2;
    const cy = (a.y + b.y) / 2;
    const k = clamp(pinch.k * (dist(a, b) / Math.max(pinch.d, 1)), MIN_K, MAX_K);

    // ponto do mundo que estava sob o centro inicial deve ficar sob o centro atual
    const sx = pinch.cx - rect.left;
    const sy = pinch.cy - rect.top;
    const wx = (sx - pinch.vx) / pinch.k;
    const wy = (sy - pinch.vy) / pinch.k;

    view.k = k;
    view.x = (cx - rect.left) - wx * k;
    view.y = (cy - rect.top) - wy * k;
    scheduleRender();
  }

  /* ----------------------- roda do mouse / trackpad ----------------------- */
  board.addEventListener('wheel', e => {
    e.preventDefault();
    const mult = e.deltaMode === 1 ? 16 : 1;
    if (e.ctrlKey || e.metaKey) {                       // zoom (pinch de trackpad chega como ctrl+wheel)
      const p = localPoint(e);
      zoomAt(p.x, p.y, Math.exp(-e.deltaY * mult * 0.0025));
    } else {                                            // rolar = navegar
      view.x -= e.deltaX * mult;
      view.y -= e.deltaY * mult;
      scheduleRender();
    }
  }, { passive: false });

  board.addEventListener('pointerdown', onPointerDown);
  board.addEventListener('pointermove', onPointerMove);
  board.addEventListener('pointerup', onPointerUp);
  board.addEventListener('pointercancel', onPointerUp);
  board.addEventListener('dblclick', e => {
    const el = e.target.closest && e.target.closest('#layer text[data-shape]');
    if (el) { e.preventDefault(); editor.edit(el); }
  });
  board.addEventListener('contextmenu', e => { if (tool.name === 'eraser') e.preventDefault(); });

  /* ----------------------- teclado ----------------------- */
  window.addEventListener('keydown', e => {
    const tag = (e.target.tagName || '').toLowerCase();
    if (tag === 'input' || tag === 'textarea' || e.target.isContentEditable) return;

    if (e.code === 'Space' && !e.repeat) {
      tool.space = true;
      board.classList.add('is-pan');
      e.preventDefault();
      return;
    }

    const mod = e.ctrlKey || e.metaKey;
    if (mod) {
      const k = e.key.toLowerCase();
      if (k === 'z') { e.preventDefault(); e.shiftKey ? redo() : undo(); }
      if (k === 'y') { e.preventDefault(); redo(); }
      if (k === '0') { e.preventDefault(); resetView(); }
      return;
    }

    switch (e.key.toLowerCase()) {
      case 'v': setTool('select'); break;
      case 'h': setTool('pan'); break;
      case 'p': setTool('pen'); break;
      case 'l': setTool('line'); break;
      case 'r': setTool('rect'); break;
      case 'o': setTool('ellipse'); break;
      case 't': setTool('text'); break;
      case 'e': setTool('eraser'); break;
      case 'g': toggleGrid(); break;
      case '+': case '=': zoomAt(board.clientWidth / 2, board.clientHeight / 2, 1.2); break;
      case '-': case '_': zoomAt(board.clientWidth / 2, board.clientHeight / 2, 1 / 1.2); break;
      case 'delete': case 'backspace': e.preventDefault(); deleteSelection(); break;
      case 'escape':
        selection.clear();
        syncSelection();
        break;
    }
  });

  window.addEventListener('keyup', e => {
    if (e.code === 'Space') {
      tool.space = false;
      board.classList.remove('is-pan');
    }
  });

  /* ----------------------- ferramentas / ações ----------------------- */
  function setTool(name) {
    if (editor.isOpen()) editor.close(true);
    tool.name = name;
    board.dataset.tool = name;
    $$('#deck .tool[data-tool]').forEach(b => b.setAttribute('aria-pressed', String(b.dataset.tool === name)));
    if (name !== 'select') { selection.clear(); syncSelection(); }
  }

  function toggleGrid(force) {
    tool.grid = typeof force === 'boolean' ? force : !tool.grid;
    gridLines.style.display = tool.grid ? '' : 'none';
    gridDots.style.display = tool.grid ? '' : 'none';
    const btn = $('.chip[data-action="grid"]');
    if (btn) btn.setAttribute('aria-pressed', String(tool.grid));
  }

  function clearBoard() {
    if (!layer.children.length) { toast('O quadro já está vazio.'); return; }
    if (!window.confirm('Apagar todo o desenho?')) return;
    layer.innerHTML = '';
    selection.clear();
    overlay.innerHTML = '';
    commit();
    toast('Quadro limpo.');
  }

  function exportSVG() {
    if (!layer.children.length) { toast('Nada para exportar ainda.'); return; }
    let b;
    try { b = layer.getBBox(); } catch (_) { b = { x: 0, y: 0, width: 200, height: 200 }; }
    const pad = 40;
    const x = r2(b.x - pad);
    const y = r2(b.y - pad);
    const w = Math.max(r2(b.width + pad * 2), 10);
    const h = Math.max(r2(b.height + pad * 2), 10);

    const svg = `<?xml version="1.0" encoding="UTF-8"?>
<svg xmlns="${NS}" viewBox="${x} ${y} ${w} ${h}" width="${Math.round(w)}" height="${Math.round(h)}">
  <rect x="${x}" y="${y}" width="${w}" height="${h}" fill="#0b0f16"/>
  ${layer.innerHTML}
</svg>`;

    const blob = new Blob([svg], { type: 'image/svg+xml;charset=utf-8' });
    const url = URL.createObjectURL(blob);
    const a = document.createElement('a');
    a.href = url;
    a.download = 'desenho.svg';
    document.body.appendChild(a);
    a.click();
    a.remove();
    setTimeout(() => URL.revokeObjectURL(url), 1500);
    toast('SVG exportado.');
  }

  function runAction(action) {
    if (editor.isOpen()) editor.close(true);
    switch (action) {
      case 'undo': undo(); break;
      case 'redo': redo(); break;
      case 'clear': clearBoard(); break;
      case 'export': exportSVG(); break;
      case 'grid': toggleGrid(); break;
      case 'zoom-in': zoomAt(board.clientWidth / 2, board.clientHeight / 2, 1.25); break;
      case 'zoom-out': zoomAt(board.clientWidth / 2, board.clientHeight / 2, 1 / 1.25); break;
      case 'zoom-reset': resetView(); break;
      case 'zoom-fit': fitView(); break;
    }
  }

  $$('#deck .tool[data-tool]').forEach(btn =>
    btn.addEventListener('click', () => setTool(btn.dataset.tool)));

  $$('[data-action]').forEach(btn =>
    btn.addEventListener('click', () => runAction(btn.dataset.action)));

  colorInput.addEventListener('input', () => {
    tool.color = colorInput.value;
    swatchChip.style.background = tool.color;
    // aplica a cor/espessura em elementos selecionados
    selection.forEach(el => el.setAttribute(el.tagName === 'text' ? 'fill' : 'stroke', tool.color));
    if (selection.size) commit();
  });

  sizeInput.addEventListener('input', () => {
    tool.size = +sizeInput.value;
    sizeOut.textContent = sizeInput.value;
    selection.forEach(el => el.setAttribute('stroke-width', tool.size));
    if (selection.size) commit();
  });

  /* ----------------------- toast ----------------------- */
  let toastTimer = 0;
  function toast(msg) {
    toastEl.textContent = msg;
    toastEl.classList.add('show');
    clearTimeout(toastTimer);
    toastTimer = setTimeout(() => toastEl.classList.remove('show'), 2200);
  }

  /* ----------------------- init ----------------------- */
  function boot() {
    resetView();
    render();
    toggleGrid(true);
    history.stack = [snapshot()];
    history.index = 0;
    setTool('pen');

    // exemplo inicial (apague com o botão da lixeira)
    const hint = makeShape('text', {
      x: -120, y: -140,
      fill: 'rgba(140,175,235,.75)',
      'font-size': 22,
      'font-family': 'ui-sans-serif, system-ui, Segoe UI, Arial, sans-serif',
      'text-anchor': 'middle',
    });
    hint.textContent = 'Desenhe aqui ✏️';
    commit();
  }

  window.addEventListener('resize', scheduleRender);
  boot();
})();
