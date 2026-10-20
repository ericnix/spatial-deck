/*
 * bridge.js — ponte entre as páginas do shell (canvas/deck) e o shell-host C.
 *
 * Caminhos de comunicação:
 *   JS -> host : window.webkit.messageHandlers.p.postMessage(json)
 *   host -> JS : Padrao.emit(json)  (avaliado via webkit_web_view_evaluate_javascript)
 *
 * Quando a página roda fora do WebKit (modo demo, p/ visualização em
 * navegador comum), o bridge simula um host: persiste em localStorage e
 * emite eventos falsos, permitindo desenvolver a UI sem o compositor.
 */
(function () {
  'use strict';

  const handlers = Object.create(null);
  const readyWaiters = [];
  let booted = false;

  const hasNative =
    typeof window.webkit !== 'undefined' &&
    window.webkit.messageHandlers &&
    window.webkit.messageHandlers.p;

  const Padrao = {
    page: (document.body && document.body.dataset.page) || 'shell',
    native: !!hasNative,

    /** Envia uma mensagem JSON para o shell-host. */
    send(obj) {
      const s = JSON.stringify(obj);
      if (hasNative) {
        try {
          window.webkit.messageHandlers.p.postMessage(s);
          return;
        } catch (e) {
          console.error('[padrao] postMessage falhou', e);
        }
      }
      if (Padrao.demo) Demo.outbox(obj);
    },

    /** Registra um handler para eventos vindos do host. */
    on(type, fn) {
      (handlers[type] || (handlers[type] = [])).push(fn);
    },

    /** Chamado pelo host (evaluate_javascript) para entregar um evento. */
    emit(evt) {
      const list = handlers[evt && evt.t];
      if (list) for (const fn of list) fn(evt);
      const all = handlers['*'];
      if (all) for (const fn of all) fn(evt);
    },

    /** Executa fn depois do handshake com o host. */
    whenReady(fn) {
      if (booted) fn();
      else readyWaiters.push(fn);
    },
  };

  /* ------------------------------------------------------------------ */
  /* Modo demo: sem WebKit, simula o host para visualização em navegador */
  /* ------------------------------------------------------------------ */
  const Demo = {
    state: null,
    load() {
      try {
        Demo.state = JSON.parse(localStorage.getItem('padrao.demo') || 'null');
      } catch (e) { Demo.state = null; }
      if (!Demo.state) {
        Demo.state = {
          view: { x: -260, y: -160, z: 1 },
          items: [
            { id: 'n1', type: 'note', x: 60, y: 40, w: 260, h: 200, color: 0,
              text: 'Bem-vindo ao Padrão ✦\n\nIsto é uma nota. Arraste pela tarja, redimensione pelo canto.' },
            { id: 'n2', type: 'note', x: 380, y: 120, w: 240, h: 170, color: 3,
              text: 'Canvas infinito:\n• arraste o fundo para navegar\n• Ctrl+rolar = zoom\n• botão direito = menu' },
          ],
        };
      }
      return Demo.state;
    },
    outbox(obj) {
      switch (obj.t) {
        case 'ready': {
          const st = Demo.load();
          const wins = [
            { id: 1, app: 'firefox', title: 'Padrão — documentação', x: -40, y: -60, w: 760, h: 520, focused: true },
            { id: 2, app: 'konsole', title: 'terminal — make run', x: 700, y: 260, w: 620, h: 380, focused: false },
            { id: 3, app: 'org.gnome.TextEditor', title: 'notas.md', x: 320, y: -320, w: 560, h: 400, focused: false },
          ];
          const apps = [
            { id: 'firefox', name: 'Firefox', icon: 'firefox', fav: true },
            { id: 'org.gnome.Nautilus', name: 'Arquivos', icon: 'system-file-manager', fav: true },
            { id: 'konsole', name: 'Konsole', icon: 'utilities-terminal', fav: true },
            { id: 'org.gnome.TextEditor', name: 'Editor de Texto', icon: 'accessories-text-editor', fav: true },
            { id: 'org.gnome.Calculator', name: 'Calculadora', icon: 'accessories-calculator', fav: false },
            { id: 'vlc', name: 'VLC', icon: 'vlc', fav: false },
            { id: 'gimp', name: 'GIMP', icon: 'gimp', fav: false },
            { id: 'code', name: 'Code', icon: 'code', fav: false },
          ];
          queueMicrotask(() => Padrao.emit({
            t: 'init', page: Padrao.page, view: st.view,
            out: { w: innerWidth, h: innerHeight },
            wins, apps, saved: { canvas: st },
            iconBase: '',
          }));
          break;
        }
        case 'view':
          Demo.state.view = { x: obj.x, y: obj.y, z: obj.z };
          Demo.save();
          break;
        case 'save':
          Demo.state = obj.data;
          Demo.save();
          break;
        case 'win_move':
        case 'win_focus': {
          const w = Demo.wins && Demo.wins.find((w) => w.id === obj.id);
          if (w) {
            if (obj.x !== undefined) { w.x = obj.x; w.y = obj.y; }
            Demo.wins.forEach((w) => (w.focused = w.id === obj.id));
            queueMicrotask(() => {
              Padrao.emit({ t: 'win_focus', id: obj.id });
              Padrao.emit(Object.assign({ t: 'win' }, w));
            });
          }
          break;
        }
        case 'launch':
          toastDemo('abriria: ' + obj.app);
          break;
        case 'pick':
          toastDemo('escolha de arquivos requer o Padrão nativo');
          break;
        case 'deck_state': /* nada a animar no demo */ break;
        default: break;
      }
    },
    save() {
      try { localStorage.setItem('padrao.demo', JSON.stringify(Demo.state)); } catch (e) {}
    },
  };

  function toastDemo(msg) {
    const el = document.createElement('div');
    el.className = 'toast';
    el.textContent = 'demo: ' + msg;
    document.body.appendChild(el);
    setTimeout(() => el.remove(), 2200);
  }

  /* ------------------------------------------------------------------ */
  function boot() {
    if (booted) return;
    booted = true;
    if (!hasNative) {
      Padrao.demo = true;
      console.info('[padrao] WebKit ausente — modo demo ativo');
    }
    Padrao.send({ t: 'ready', page: Padrao.page });
    const fns = readyWaiters.splice(0, readyWaiters.length);
    for (const fn of fns) fn();
  }

  window.Padrao = Padrao;
  if (document.readyState === 'loading') {
    document.addEventListener('DOMContentLoaded', boot);
  } else {
    boot();
  }
})();
