/*
 * canvas.js — o canvas infinito do Padrão.
 *
 * Responsabilidades:
 *  • câmera (pan/zoom) do mundo, sincronizada com o compositor via bridge;
 *  • desenhar etiquetas/contornos das janelas reais (que ficam acima);
 *  • notas, imagens e vídeos posicionados no mundo, com persistência.
 *
 * Coordenadas:
 *   tela = (mundo - view.xy) * zoom
 */
(function () {
  'use strict';

  const $ = (sel) => document.querySelector(sel);
  const world = $('#world');
  const grid = $('#grid');
  const itemsLayer = $('#items');
  const winsLayer = $('#wins');
  const zoomChip = $('#zoom-chip');
  const hint = $('#hint');
  const ctxMenu = $('#ctx');
  const toasts = $('#toasts');

  const S = {
    view: { x: -260, y: -160, z: 1 },
    items: [],
    wins: new Map(),
    iconBase: '',
    sel: null,
    zTop: 10,
    pendingPick: null,
    menuWorld: null,
    menuCtxItem: null,
  };

  /* ---------------------------- utilidades --------------------------- */
  const clamp = (v, a, b) => Math.min(b, Math.max(a, v));
  const uid = () => 'i' + Date.now().toString(36) + Math.random().toString(36).slice(2, 6);

  function toast(msg) {
    const el = document.createElement('div');
    el.className = 'toast';
    el.textContent = msg;
    toasts.appendChild(el);
    setTimeout(() => {
      el.style.transition = 'opacity .4s';
      el.style.opacity = '0';
      setTimeout(() => el.remove(), 420);
    }, 2600);
  }

  function screenToWorld(sx, sy) {
    return { x: sx / S.view.z + S.view.x, y: sy / S.view.z + S.view.y };
  }

  /* ----------------------------- câmera ------------------------------ */
  function applyView() {
    const { x, y, z } = S.view;
    world.style.transform =
      `translate(${(-x * z).toFixed(2)}px, ${(-y * z).toFixed(2)}px) scale(${z})`;
    const fine = 24 * z, major = 120 * z;
    grid.style.backgroundSize =
      `${fine}px ${fine}px, ${fine}px ${fine}px, ${major}px ${major}px, ${major}px ${major}px`;
    grid.style.backgroundPosition = `${-x * z}px ${-y * z}px`;
    zoomChip.textContent = Math.round(z * 100) + '%';
  }

  function sendView() {
    Padrao.send({ t: 'view', x: S.view.x, y: S.view.y, z: S.view.z });
    scheduleSave();
  }

  function zoomAt(sx, sy, factor) {
    const before = screenToWorld(sx, sy);
    S.view.z = clamp(S.view.z * factor, 0.15, 4);
    S.view.x = before.x - sx / S.view.z;
    S.view.y = before.y - sy / S.view.z;
    applyView();
    sendView();
  }

  function setZoom(z) {
    const cx = innerWidth / 2, cy = innerHeight / 2;
    zoomAt(cx, cy, z / S.view.z);
  }

  /* roda do mouse: pan comum, pan horizontal com Shift, zoom com Ctrl */
  addEventListener('wheel', (e) => {
    e.preventDefault();
    hintFade();
    if (e.ctrlKey || e.metaKey) {
      zoomAt(e.clientX, e.clientY, Math.exp(-e.deltaY * 0.0022));
      return;
    }
    if (e.shiftKey) {
      S.view.x += (e.deltaY || e.deltaX) / S.view.z;
    } else {
      S.view.x += e.deltaX / S.view.z;
      S.view.y += e.deltaY / S.view.z;
    }
    applyView();
    sendView();
  }, { passive: false });

  /* -------------------------- pan arrastando ------------------------- */
  let panning = null;
  addEventListener('pointerdown', (e) => {
    if (e.button === 1 || (e.button === 0 && isCanvasBg(e.target))) {
      if (e.button === 0 && isCanvasBg(e.target) && e.detail === 2) return;
      panning = { sx: e.clientX, sy: e.clientY, vx: S.view.x, vy: S.view.y, moved: false };
      document.body.style.cursor = 'grabbing';
      hideMenu();
      if (e.button === 0 && isCanvasBg(e.target)) select(null);
    }
  });
  addEventListener('pointermove', (e) => {
    if (!panning) return;
    const dx = e.clientX - panning.sx, dy = e.clientY - panning.sy;
    if (Math.abs(dx) + Math.abs(dy) > 3) { panning.moved = true; hintFade(); }
    S.view.x = panning.vx - dx / S.view.z;
    S.view.y = panning.vy - dy / S.view.z;
    applyView();
  });
  addEventListener('pointerup', () => {
    if (!panning) return;
    const moved = panning.moved;
    panning = null;
    document.body.style.cursor = '';
    if (moved) sendView();
  });

  function isCanvasBg(t) {
    return t === document.body || t === grid || t.id === 'vignette' ||
      t === world || t.id === 'wins' || t.id === 'items' || t.id === 'origin';
  }

  /* --------------------------- teclado -------------------------------- */
  addEventListener('keydown', (e) => {
    const editing = document.activeElement && document.activeElement.isContentEditable;
    if (editing) return;
    const step = 90;
    switch (e.key) {
      case 'ArrowUp': S.view.y -= step / S.view.z; applyView(); sendView(); break;
      case 'ArrowDown': S.view.y += step / S.view.z; applyView(); sendView(); break;
      case 'ArrowLeft': S.view.x -= step / S.view.z; applyView(); sendView(); break;
      case 'ArrowRight': S.view.x += step / S.view.z; applyView(); sendView(); break;
      case '+': case '=': zoomAt(innerWidth / 2, innerHeight / 2, 1.2); break;
      case '-': zoomAt(innerWidth / 2, innerHeight / 2, 1 / 1.2); break;
      case '0': if (e.ctrlKey || !e.metaKey) setZoom(1); break;
      case 'Escape': hideMenu(); select(null); break;
      case 'Delete': if (S.sel) removeItem(S.sel); break;
      default: return;
    }
    if (e.key.startsWith('Arrow') || e.key === '+' || e.key === '-') e.preventDefault();
  });

  function hintFade() { hint.classList.add('gone'); }

  /* ------------------------ menu de contexto -------------------------- */
  function showMenu(cx, cy, entries, worldPt, ctxItem) {
    S.menuWorld = worldPt;
    S.menuCtxItem = ctxItem || null;
    ctxMenu.innerHTML = '';
    for (const it of entries) {
      if (it.sep) {
        const d = document.createElement('div');
        d.className = 'sep';
        ctxMenu.appendChild(d);
        continue;
      }
      const el = document.createElement('div');
      el.className = 'mi' + (it.danger ? ' danger' : '');
      el.innerHTML = `<span class="gl">${it.g}</span><span>${it.label}</span>`;
      el.addEventListener('click', () => { hideMenu(); it.fn(); });
      ctxMenu.appendChild(el);
    }
    ctxMenu.hidden = false;
    const r = ctxMenu.getBoundingClientRect();
    ctxMenu.style.left = Math.min(cx, innerWidth - r.width - 8) + 'px';
    ctxMenu.style.top = Math.min(cy, innerHeight - r.height - 8) + 'px';
  }
  function hideMenu() { ctxMenu.hidden = true; }
  addEventListener('pointerdown', (e) => {
    if (!ctxMenu.hidden && !ctxMenu.contains(e.target)) hideMenu();
  }, true);

  addEventListener('contextmenu', (e) => {
    e.preventDefault();
    const itemEl = e.target.closest && e.target.closest('.item');
    const pt = screenToWorld(e.clientX, e.clientY);
    const entries = [];
    if (itemEl) {
      const it = S.items.find((i) => i.id === itemEl.dataset.id);
      if (it && it.type === 'note') {
        entries.push({ g: '🎨', label: 'Mudar cor da nota', fn: () => cycleColor(it) });
        entries.push({ g: '⧉', label: 'Duplicar', fn: () => duplicateItem(it) });
        entries.push({ sep: true });
        entries.push({ g: '🗑', label: 'Excluir', danger: true, fn: () => removeItem(it.id) });
      } else if (it) {
        entries.push({ g: '⧉', label: 'Duplicar', fn: () => duplicateItem(it) });
        entries.push({ sep: true });
        entries.push({ g: '🗑', label: 'Excluir', danger: true, fn: () => removeItem(it.id) });
      }
      showMenu(e.clientX, e.clientY, entries, pt, it);
      return;
    }
    entries.push({ g: '✎', label: 'Nova nota', fn: () => newNote(pt) });
    entries.push({ g: '🖼', label: 'Inserir imagem…', fn: () => pickFiles('image') });
    entries.push({ g: '🎬', label: 'Inserir vídeo…', fn: () => pickFiles('video') });
    entries.push({ sep: true });
    entries.push({ g: '⌖', label: 'Ir para a origem', fn: () => flyTo(0, 0) });
    entries.push({ g: '◱', label: 'Zoom 100%', fn: () => setZoom(1) });
    showMenu(e.clientX, e.clientY, entries, pt, null);
  });

  addEventListener('dblclick', (e) => {
    if (isCanvasBg(e.target)) newNote(screenToWorld(e.clientX, e.clientY));
  });

  /* anima a câmera até um ponto do mundo */
  function flyTo(wx, wy) {
    const z = S.view.z;
    const tx = wx - innerWidth / (2 * z), ty = wy - innerHeight / (2 * z);
    const sx = S.view.x, sy = S.view.y, t0 = performance.now();
    (function step(t) {
      const k = Math.min(1, (t - t0) / 450);
      const e = 1 - Math.pow(1 - k, 3);
      S.view.x = sx + (tx - sx) * e;
      S.view.y = sy + (ty - sy) * e;
      applyView();
      if (k < 1) requestAnimationFrame(step); else sendView();
    })(t0);
  }

  /* ------------------------------ itens ------------------------------- */
  function mediaUrl(path) {
    return S.iconBase + 'm?path=' + encodeURIComponent(path);
  }

  function scheduleSave() {
    clearTimeout(scheduleSave._t);
    scheduleSave._t = setTimeout(save, 450);
  }
  function save() {
    Padrao.send({
      t: 'save', key: 'canvas',
      data: { view: S.view, items: S.items },
    });
  }
  addEventListener('pagehide', save);

  function baseItem(type, pt, extra) {
    const it = Object.assign({
      id: uid(), type,
      x: Math.round(pt.x), y: Math.round(pt.y),
      w: type === 'note' ? 250 : 340,
      h: type === 'note' ? 190 : 240,
    }, extra || {});
    S.items.push(it);
    mountItem(it);
    scheduleSave();
    return it;
  }

  function newNote(pt) {
    const it = baseItem('note', pt, { text: '', color: Math.floor(Math.random() * 6) });
    select(it.id);
    const body = itemsLayer.querySelector(`[data-id="${it.id}"] .body`);
    if (body) body.focus();
    return it;
  }

  function addMedia(path, pt) {
    const name = path.split('/').pop();
    const isVideo = /\.(webm|mp4|mkv|ogv|mov|m4v|avi)$/i.test(path);
    const it = baseItem(isVideo ? 'video' : 'image', pt, { src: path, name });
    if (!isVideo) {
      const im = new Image();
      im.onload = () => {
        if (!it._sized) {
          const scale = Math.min(1, 560 / im.naturalWidth);
          it.w = Math.max(160, Math.round(im.naturalWidth * scale));
          it.h = Math.max(120, Math.round(im.naturalHeight * scale));
          it._sized = true;
          const el = itemsLayer.querySelector(`[data-id="${it.id}"]`);
          if (el) { el.style.width = it.w + 'px'; el.style.height = it.h + 'px'; }
          scheduleSave();
        }
      };
      im.src = mediaUrl(path);
    }
    return it;
  }

  function duplicateItem(it) {
    const cp = JSON.parse(JSON.stringify(it));
    cp.id = uid();
    cp.x += 36; cp.y += 36;
    delete cp._sized;
    S.items.push(cp);
    mountItem(cp);
    select(cp.id);
    scheduleSave();
  }

  function cycleColor(it) {
    it.color = ((it.color || 0) + 1) % 6;
    const el = itemsLayer.querySelector(`[data-id="${it.id}"]`);
    if (el) el.dataset.color = it.color;
    scheduleSave();
  }

  function removeItem(id) {
    const idx = S.items.findIndex((i) => i.id === id);
    if (idx < 0) return;
    S.items.splice(idx, 1);
    const el = itemsLayer.querySelector(`[data-id="${id}"]`);
    if (el) el.remove();
    if (S.sel === id) select(null);
    scheduleSave();
  }

  function select(id) {
    if (S.sel) {
      const prev = itemsLayer.querySelector(`[data-id="${S.sel}"]`);
      if (prev) prev.classList.remove('selected');
    }
    S.sel = id;
    if (id) {
      const el = itemsLayer.querySelector(`[data-id="${id}"]`);
      if (el) {
        el.classList.add('selected');
        el.style.zIndex = ++S.zTop;
      }
    }
  }

  function mountItem(it) {
    const el = document.createElement('div');
    el.className = 'item ' + (it.type === 'note' ? 'note' : 'media ' + it.type);
    el.dataset.id = it.id;
    el.style.left = it.x + 'px';
    el.style.top = it.y + 'px';
    el.style.width = it.w + 'px';
    el.style.height = it.h + 'px';
    el.style.zIndex = ++S.zTop;
    if (it.type === 'note') el.dataset.color = it.color || 0;

    const isNote = it.type === 'note';
    const name = isNote ? 'nota' : (it.name || it.src.split('/').pop());
    const bar = document.createElement('div');
    bar.className = 'bar';
    bar.innerHTML =
      `<span class="name"></span>` +
      (isNote ? `<button class="col" title="cor">◐</button>` : '') +
      `<button class="del" title="excluir">✕</button>`;
    bar.querySelector('.name').textContent = name;
    el.appendChild(bar);

    const body = document.createElement('div');
    body.className = 'body';
    if (isNote) {
      body.contentEditable = 'true';
      body.spellcheck = false;
      body.textContent = it.text || '';
      body.addEventListener('input', () => { it.text = body.innerText; scheduleSave(); });
      body.addEventListener('pointerdown', (e) => e.stopPropagation());
      body.addEventListener('focus', () => select(it.id));
    } else {
      const src = mediaUrl(it.src);
      const node = document.createElement(it.type === 'video' ? 'video' : 'img');
      node.src = src;
      if (it.type === 'video') {
        node.controls = true;
        node.preload = 'metadata';
        node.addEventListener('pointerdown', (e) => e.stopPropagation());
      }
      body.appendChild(node);
      if (it.type === 'image') {
        node.addEventListener('load', () => {
          if (!it._sized && node.naturalWidth) {
            const scale = Math.min(1, 560 / node.naturalWidth);
            it.w = Math.max(160, Math.round(node.naturalWidth * scale));
            it.h = Math.max(120, Math.round(node.naturalHeight * scale));
            it._sized = true;
            el.style.width = it.w + 'px';
            el.style.height = it.h + 'px';
            scheduleSave();
          }
        });
      }
    }
    el.appendChild(body);

    const res = document.createElement('div');
    res.className = 'resizer';
    el.appendChild(res);

    /* seleção */
    el.addEventListener('pointerdown', () => select(it.id));

    /* arrastar pela tarja */
    bar.addEventListener('pointerdown', (e) => {
      if (e.target.closest('button') || e.button !== 0) return;
      e.preventDefault();
      e.stopPropagation();
      select(it.id);
      dragBy(e, (dx, dy) => {
        it.x = Math.round(it._x0 + dx / S.view.z);
        it.y = Math.round(it._y0 + dy / S.view.z);
        el.style.left = it.x + 'px';
        el.style.top = it.y + 'px';
      }, () => scheduleSave(), () => { it._x0 = it.x; it._y0 = it.y; });
    });

    /* redimensionar */
    res.addEventListener('pointerdown', (e) => {
      if (e.button !== 0) return;
      e.preventDefault();
      e.stopPropagation();
      select(it.id);
      const minW = isNote ? 150 : 120, minH = isNote ? 110 : 90;
      dragBy(e, (dx, dy) => {
        it.w = Math.max(minW, Math.round(it._w0 + dx / S.view.z));
        it.h = Math.max(minH, Math.round(it._h0 + dy / S.view.z));
        el.style.width = it.w + 'px';
        el.style.height = it.h + 'px';
      }, () => scheduleSave(), () => { it._w0 = it.w; it._h0 = it.h; });
    });

    bar.querySelector('.del').addEventListener('click', (e) => {
      e.stopPropagation();
      removeItem(it.id);
    });
    const colBtn = bar.querySelector('.col');
    if (colBtn) colBtn.addEventListener('click', (e) => {
      e.stopPropagation();
      cycleColor(it);
    });

    itemsLayer.appendChild(el);
  }

  /* genérico: arrasto de tela com callback em deltas de tela */
  function dragBy(startEv, onMove, onEnd, onBegin) {
    if (onBegin) onBegin();
    const sx = startEv.clientX, sy = startEv.clientY;
    function move(e) { onMove(e.clientX - sx, e.clientY - sy); }
    function up() {
      removeEventListener('pointermove', move);
      removeEventListener('pointerup', up);
      if (onEnd) onEnd();
    }
    addEventListener('pointermove', move);
    addEventListener('pointerup', up);
  }

  /* --------------------- seleção de arquivos -------------------------- */
  function pickFiles(kind) {
    S.pendingPick = S.menuWorld || screenToWorld(innerWidth / 2, innerHeight / 2);
    Padrao.send({ t: 'pick', kind });
  }

  /* --------------------------- janelas reais -------------------------- */
  function iconFor(app) {
    return S.iconBase + 'i/' + encodeURIComponent(app || 'app') + '?s=48';
  }

  function renderWin(w) {
    let el = winsLayer.querySelector(`[data-win="${w.id}"]`);
    let frame = winsLayer.querySelector(`[data-frame="${w.id}"]`);
    if (!el) {
      el = document.createElement('div');
      el.className = 'wtag';
      el.dataset.win = w.id;
      el.innerHTML =
        `<img class="ic" alt=""><span class="ttl"></span>` +
        `<button class="close" title="fechar">✕</button>`;
      winsLayer.appendChild(el);

      frame = document.createElement('div');
      frame.className = 'wframe';
      frame.dataset.frame = w.id;
      winsLayer.appendChild(frame);

      el.addEventListener('pointerdown', (e) => {
        if (e.target.classList.contains('close') || e.button !== 0) return;
        e.preventDefault();
        e.stopPropagation();
        const w0 = S.wins.get(w.id);
        if (!w0) return;
        const start = { x: w0.x, y: w0.y };
        dragBy(e, (dx, dy) => {
          const nw = {
            x: Math.round(start.x + dx / S.view.z),
            y: Math.round(start.y + dy / S.view.z),
          };
          w0.x = nw.x; w0.y = nw.y;
          updateWinGeom(w0);
          Padrao.send({ t: 'win_move', id: w.id, x: nw.x, y: nw.y });
        });
      });
      el.addEventListener('click', (e) => {
        if (e.target.classList.contains('close')) {
          Padrao.send({ t: 'win_close', id: w.id });
          return;
        }
        Padrao.send({ t: 'win_focus', id: w.id });
      });
    }
    el.querySelector('.ttl').textContent = w.title || w.app || 'janela';
    const ic = el.querySelector('.ic');
    ic.src = iconFor(w.app);
    ic.onerror = () => { ic.style.visibility = 'hidden'; };
    ic.onload = () => { ic.style.visibility = ''; };
    el.classList.toggle('focused', !!w.focused);
    frame.classList.toggle('focused', !!w.focused);
    updateWinGeom(w);
  }

  function updateWinGeom(w) {
    const el = winsLayer.querySelector(`[data-win="${w.id}"]`);
    const frame = winsLayer.querySelector(`[data-frame="${w.id}"]`);
    if (el) {
      el.style.left = (w.x + w.w / 2) + 'px';
      el.style.top = (w.y - 9) + 'px';
    }
    if (frame) {
      frame.style.left = (w.x - 3) + 'px';
      frame.style.top = (w.y - 3) + 'px';
      frame.style.width = (w.w + 6) + 'px';
      frame.style.height = (w.h + 6) + 'px';
    }
  }

  function removeWin(id) {
    const el = winsLayer.querySelector(`[data-win="${id}"]`);
    const frame = winsLayer.querySelector(`[data-frame="${id}"]`);
    if (el) el.remove();
    if (frame) frame.remove();
    S.wins.delete(id);
  }

  /* ----------------------------- bridge -------------------------------- */
  Padrao.on('init', (e) => {
    if (e.page !== 'canvas') return;
    S.iconBase = e.iconBase || '';
    if (e.out) { /* viewport inicial da superfície */ }
    if (e.saved && e.saved.canvas) {
      const c = e.saved.canvas;
      if (c.view) S.view = c.view;
      S.items = c.items || [];
    }
    applyView();
    itemsLayer.innerHTML = '';
    for (const it of S.items) mountItem(it);
    (e.wins || []).forEach((w) => { S.wins.set(w.id, w); renderWin(w); });
    if (!e.saved || !e.saved.canvas) sendView();
  });

  Padrao.on('view', (e) => {
    S.view = { x: e.x, y: e.y, z: e.z };
    applyView();
  });

  Padrao.on('win', (e) => {
    if (e.id === undefined) return;
    S.wins.set(e.id, e);
    renderWin(e);
  });
  Padrao.on('win_gone', (e) => removeWin(e.id));
  Padrao.on('win_focus', (e) => {
    S.wins.forEach((w) => { w.focused = w.id === e.id; });
    S.wins.forEach((w) => renderWin(w));
  });
  Padrao.on('output', () => { /* futuro: ajustar HUD */ });

  Padrao.on('picked', (e) => {
    const pt = S.pendingPick || screenToWorld(innerWidth / 2, innerHeight / 2);
    S.pendingPick = null;
    let n = 0;
    for (const p of (e.paths || [])) {
      const offset = { x: pt.x + n * 34, y: pt.y + n * 34 };
      addMedia(p, offset);
      n++;
    }
    if (n) toast(n === 1 ? 'mídia inserida no canvas' : n + ' mídias inseridas');
  });

  /* estado inicial */
  applyView();
})();
