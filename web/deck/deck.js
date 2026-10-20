/*
 * deck.js — deck trapezoidal do Padrão.
 *
 * Três estados:
 *   rest  — repouso: faixa fina em trapézio no rodapé central;
 *   open  — cresce quando o mouse entra (ícones maiores, legendas);
 *   grid  — grade de todos os aplicativos com busca.
 *
 * O tamanho da layer surface é animado em passos (rAF) via mensagens
 * deck_size para o shell-host, que redimensiona a janela GTK.
 */
(function () {
  'use strict';

  const $ = (s) => document.querySelector(s);
  const stage = $('#stage');
  const winsEl = $('#wins');
  const favsEl = $('#favs');
  const overlay = $('#overlay');
  const search = $('#search');
  const results = $('#results');
  const zoomChip = $('#zoom-chip');
  const clockEl = $('#clock');
  const tip = $('#tip');

  const S = {
    state: 'rest',
    prevState: 'rest',
    out: { w: 1280, h: 720 },
    size: { w: 0, h: 0 },
    wins: new Map(),
    apps: [],
    iconBase: '',
    raf: 0,
    leaveT: 0,
    tipT: 0,
  };

  /* --------------------------- geometria ------------------------------ */
  function targetSize(st) {
    const out = S.out;
    switch (st) {
      case 'open':
        return { w: Math.min(1240, out.w - 72), h: Math.min(250, Math.round(out.h * 0.30)) };
      case 'grid':
        return { w: Math.min(1360, out.w - 56), h: Math.min(640, out.h - 120) };
      default:
        return { w: Math.min(820, out.w - 200), h: 66 };
    }
  }

  function sendSize() {
    Padrao.send({ t: 'deck_size', w: S.size.w, h: S.size.h });
  }

  /* anima o tamanho da superfície (o compositor acompanha em ~1 quadro) */
  function animateSize(to, dur) {
    cancelAnimationFrame(S.raf);
    const from = { w: S.size.w || to.w, h: S.size.h || to.h };
    if (!S.size.w) { S.size = { ...to }; sendSize(); }
    const t0 = performance.now();
    dur = dur || 300;
    (function step(t) {
      const k = Math.min(1, (t - t0) / dur);
      const e = 1 - Math.pow(1 - k, 3);
      const w = Math.round(from.w + (to.w - from.w) * e);
      const h = Math.round(from.h + (to.h - from.h) * e);
      if (w !== S.size.w || h !== S.size.h) {
        S.size = { w, h };
        sendSize();
      }
      if (k < 1) S.raf = requestAnimationFrame(step);
    })(t0);
  }

  /* --------------------------- estados -------------------------------- */
  function go(st) {
    if (st === S.state) return;
    if (st !== 'grid') S.prevState = st;
    else if (S.state !== 'grid') S.prevState = S.state;
    S.state = st;
    stage.dataset.state = st;
    animateSize(targetSize(st), st === 'rest' ? 260 : 320);

    if (st === 'grid') {
      overlay.hidden = false;
      requestAnimationFrame(() => overlay.classList.add('show'));
      setTimeout(() => { try { search.focus(); } catch (e) {} }, 30);
    } else {
      overlay.classList.remove('show');
      setTimeout(() => { if (S.state !== 'grid') overlay.hidden = true; }, 230);
      try { search.blur(); } catch (e) {}
    }
  }

  /* hover: cresce ao entrar, volta ao repouso ao sair (com folga) */
  stage.addEventListener('mouseenter', () => {
    clearTimeout(S.leaveT);
    if (S.state === 'rest') go('open');
  });
  stage.addEventListener('mouseleave', () => {
    clearTimeout(S.leaveT);
    S.leaveT = setTimeout(() => {
      if (S.state !== 'rest') go('rest');
    }, 320);
  });

  /* --------------------------- blocos --------------------------------- */
  function iconUrl(name) {
    return S.iconBase + 'i/' + encodeURIComponent(name || 'app') + '?s=64';
  }

  function iconNode(iconName, label) {
    const img = document.createElement('img');
    img.className = 'ic';
    img.alt = '';
    if (!iconName) throw 0;
    img.src = iconUrl(iconName);
    img.onerror = () => {
      const s = document.createElement('span');
      s.className = 'ah';
      s.textContent = (label || '?').trim().charAt(0).toUpperCase() || '?';
      img.replaceWith(s);
    };
    return img;
  }

  function tileIcon(app) {
    try { return iconNode(app.icon, app.name); }
    catch (e) {
      const s = document.createElement('span');
      s.className = 'ah';
      s.textContent = (app.name || '?').charAt(0).toUpperCase();
      return s;
    }
  }

  function showTip(text) {
    clearTimeout(S.tipT);
    tip.textContent = text;
    tip.hidden = false;
  }
  function hideTip() { clearTimeout(S.tipT); S.tipT = setTimeout(() => { tip.hidden = true; }, 60); }

  function renderWins() {
    winsEl.innerHTML = '';
    for (const w of S.wins.values()) {
      const t = document.createElement('div');
      t.className = 'tile win' + (w.focused ? ' focused' : '');
      t.dataset.id = w.id;
      try { t.appendChild(iconNode(w.app, w.title)); }
      catch (e) {
        const s = document.createElement('span');
        s.className = 'ah';
        s.textContent = (w.title || '?').charAt(0).toUpperCase();
        t.appendChild(s);
      }
      const dot = document.createElement('span');
      dot.className = 'dot';
      t.appendChild(dot);
      const cap = document.createElement('span');
      cap.className = 'cap';
      cap.textContent = w.title || w.app || 'janela';
      t.appendChild(cap);
      const x = document.createElement('button');
      x.className = 'x';
      x.textContent = '✕';
      x.title = 'Fechar janela';
      x.addEventListener('click', (e) => {
        e.stopPropagation();
        Padrao.send({ t: 'win_close', id: w.id });
      });
      t.appendChild(x);
      t.addEventListener('click', () => {
        Padrao.send({ t: 'win_focus', id: w.id });
        go('rest');
      });
      t.addEventListener('mouseenter', () => showTip(w.title || w.app || ''));
      t.addEventListener('mouseleave', hideTip);
      winsEl.appendChild(t);
    }
    if (!S.wins.size) {
      const t = document.createElement('div');
      t.className = 'chip';
      t.style.pointerEvents = 'none';
      t.textContent = 'nenhuma janela aberta';
      winsEl.appendChild(t);
    }
  }

  function renderFavs() {
    favsEl.innerHTML = '';
    const favs = S.apps.filter((a) => a.fav).slice(0, 9);
    for (const app of favs) {
      const t = document.createElement('div');
      t.className = 'tile fav';
      t.appendChild(tileIcon(app));
      const cap = document.createElement('span');
      cap.className = 'cap';
      cap.textContent = app.name;
      t.appendChild(cap);
      t.addEventListener('click', () => { Padrao.send({ t: 'launch', app: app.id }); go('rest'); });
      t.addEventListener('mouseenter', () => showTip(app.name));
      t.addEventListener('mouseleave', hideTip);
      favsEl.appendChild(t);
    }
  }

  /* ------------------------ grade de apps ----------------------------- */
  const norm = (s) => (s || '').toLowerCase()
    .normalize('NFD').replace(/[\u0300-\u036f]/g, '');

  function renderResults() {
    const q = norm(search.value.trim());
    const list = [];
    for (const a of S.apps) {
      if (!q || norm(a.name).includes(q) || norm(a.id).includes(q) ||
          norm(a.keywords).includes(q)) list.push(a);
      if (list.length >= 72) break;
    }
    results.innerHTML = '';
    for (const app of list) {
      const r = document.createElement('div');
      r.className = 'res';
      r.appendChild(tileIcon(app));
      const nm = document.createElement('span');
      nm.className = 'nm';
      nm.textContent = app.name;
      r.appendChild(nm);
      r.addEventListener('click', () => {
        Padrao.send({ t: 'launch', app: app.id });
        go('rest');
      });
      results.appendChild(r);
    }
    if (!list.length) {
      const e = document.createElement('div');
      e.className = 'nm';
      e.style.gridColumn = '1 / -1';
      e.style.padding = '18px';
      e.style.color = 'var(--muted)';
      e.textContent = 'nenhum aplicativo encontrado';
      results.appendChild(e);
    }
  }

  search.addEventListener('input', renderResults);
  search.addEventListener('keydown', (e) => {
    if (e.key === 'Enter') {
      const first = results.querySelector('.res');
      if (first) first.click();
    } else if (e.key === 'Escape') {
      e.stopPropagation();
      go(S.prevState === 'grid' ? 'rest' : S.prevState);
    }
  });
  $('#close-grid').addEventListener('click', () => go(S.prevState === 'grid' ? 'rest' : S.prevState));
  $('#grid-btn').addEventListener('click', () => go(S.state === 'grid' ? 'open' : 'grid'));

  /* escrever já no estado aberto abre a busca (type-to-search) */
  document.addEventListener('keydown', (e) => {
    if (S.state === 'grid') {
      if (e.key === 'Escape') go(S.prevState === 'grid' ? 'rest' : S.prevState);
      return;
    }
    if (S.state !== 'rest' && e.key.length === 1 && !e.ctrlKey && !e.metaKey && !e.altKey) {
      if (document.activeElement === search) return;
      go('grid');
      search.value = e.key;
      renderResults();
      e.preventDefault();
    }
  });

  /* ----------------------------- relógio ------------------------------ */
  function tick() {
    const d = new Date();
    clockEl.textContent =
      String(d.getHours()).padStart(2, '0') + ':' +
      String(d.getMinutes()).padStart(2, '0');
  }
  tick();
  setInterval(tick, 15000);

  /* ----------------------------- bridge -------------------------------- */
  Padrao.on('init', (e) => {
    if (e.page !== 'deck') return;
    S.iconBase = e.iconBase || '';
    if (e.out) S.out = e.out;
    S.apps = e.apps || [];
    S.wins.clear();
    (e.wins || []).forEach((w) => S.wins.set(w.id, w));
    if (e.view) zoomChip.textContent = Math.round(e.view.z * 100) + '%';
    renderWins();
    renderFavs();
    renderResults();
    S.size = { w: 0, h: 0 };
    go('rest');
    animateSize(targetSize('rest'), 1);
  });

  Padrao.on('win', (e) => {
    if (e.id === undefined) return;
    S.wins.set(e.id, e);
    renderWins();
  });
  Padrao.on('win_gone', (e) => { S.wins.delete(e.id); renderWins(); });
  Padrao.on('win_focus', (e) => {
    S.wins.forEach((w) => { w.focused = w.id === e.id; });
    renderWins();
  });
  Padrao.on('apps', (e) => { S.apps = e.apps || []; renderFavs(); renderResults(); });
  Padrao.on('view', (e) => { zoomChip.textContent = Math.round(e.z * 100) + '%'; });
  Padrao.on('output', (e) => {
    S.out = { w: e.w, h: e.h };
    animateSize(targetSize(S.state), 200);
  });

  /* depuração: injeta estados via /debug/emit do host (somente loopback) */
  Padrao.on('dbg', (e) => { if (e && e.state) go(e.state); });
})();
