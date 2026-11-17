#!/usr/bin/env python3
"""Mantém o desktop Spatial Deck no topo e reativo.

O KWin, com janelas Xwayland, pode deixar de entregar cliques ao deck quando
outra janela (por exemplo o Konsole Wayland atrás dele) vira a janela ativa,
mesmo com o deck desenhado por cima. Este guardião reativa o deck sempre que
o ponteiro está sobre ele e ele não é a janela ativa.
"""
import signal
import subprocess
import sys
import time

from Xlib import display

TITLE = sys.argv[1] if len(sys.argv) > 1 else "Spatial Deck"
ACTIVATE_DEBOUNCE = 1.0  # segundos entre reativações


def find_window(root, needle):
    stack = [root]
    while stack:
        window = stack.pop()
        try:
            name = window.get_wm_name()
        except Exception:
            name = None
        if name and needle in str(name):
            return window
        try:
            stack.extend(window.query_tree().children)
        except Exception:
            pass
    return None


def absolute_position(window, root):
    x = y = 0
    current = window
    while current is not None and current.id != root.id:
        try:
            geometry = current.get_geometry()
        except Exception:
            return None
        x += geometry.x
        y += geometry.y
        try:
            current = current.query_tree().parent
        except Exception:
            return None
    return x, y


def active_window(dpy, root):
    try:
        prop = root.get_full_property(dpy.intern_atom("_NET_ACTIVE_WINDOW"), 0)
        if prop is not None and len(prop.value):
            return int(prop.value[0])
    except Exception:
        pass
    return 0


def main():
    running = True

    def stop(_signum, _frame):
        nonlocal running
        running = False

    signal.signal(signal.SIGTERM, stop)
    signal.signal(signal.SIGINT, stop)

    dpy = display.Display()
    root = dpy.screen().root
    deck = None
    last_activation = 0.0

    while running:
        if deck is None:
            deck = find_window(root, TITLE)
        if deck is not None:
            try:
                position = absolute_position(deck, root)
                geometry = deck.get_geometry()
                pointer = root.query_pointer()
                over = (
                    position is not None
                    and position[0] <= pointer.root_x < position[0] + geometry.width
                    and position[1] <= pointer.root_y < position[1] + geometry.height
                )
                active = active_window(dpy, root)
                now = time.time()
                if over and active != deck.id and now - last_activation >= ACTIVATE_DEBOUNCE:
                    ident = hex(deck.id)
                    subprocess.run(
                        ["wmctrl", "-i", "-a", ident],
                        check=False,
                        stdout=subprocess.DEVNULL,
                        stderr=subprocess.DEVNULL,
                    )
                    subprocess.run(
                        ["wmctrl", "-i", "-r", ident, "-b", "add,above"],
                        check=False,
                        stdout=subprocess.DEVNULL,
                        stderr=subprocess.DEVNULL,
                    )
                    last_activation = now
            except Exception:
                deck = None
        time.sleep(0.4)

    dpy.close()


if __name__ == "__main__":
    main()
