#!/usr/bin/env python3
"""Zappy graphical client (2D, tkinter, no game engine).

Usage: ./gui [-p <port>] [-h <hostname>]

Shows the world in real time: food, the six stones, players (triangles,
coloured by team, the number is the level, a yellow outline means the player
is in an enchantment) and broadcasts (expanding rings).
Click a square to see its details in a floating box.
"""
import argparse
import socket
import threading
import time
import tkinter as tk

STONES = ["jade", "peridot", "amber", "amethyst", "garnet", "ammolite"]
STONE_COLORS = ["#2ecc71", "#a3d900", "#e6a117", "#9b59b6", "#c0392b", "#16a2d1"]
TEAM_COLORS = ["#e74c3c", "#3498db", "#f1c40f", "#1abc9c", "#e67e22", "#ecf0f1",
               "#fd79a8", "#a29bfe"]
DIR_NAMES = ["north", "east", "south", "west"]
SOUND_LIFETIME = 1.8


class Net(threading.Thread):
    """Reads frames from the server in the background."""

    def __init__(self, host, port):
        super().__init__(daemon=True)
        self.lock = threading.Lock()
        self.frame = None
        self.sounds = []  # (x, y, text, start time)
        self.status = "connecting..."
        self.host = host
        self.port = port

    def run(self):
        try:
            sock = socket.create_connection((self.host, self.port))
        except OSError as err:
            self.status = "cannot connect: %s" % err
            return
        reader = sock.makefile("r", encoding="utf-8", newline="\n")
        sock.sendall(b"GRAPHIC\n")
        if reader.readline().strip() != "WELCOME":
            self.status = "unexpected server greeting"
            return
        self.status = "connected"
        cur = self._empty()
        for line in reader:
            parts = line.rstrip("\n").split(" ")
            kind = parts[0]
            try:
                if kind == "size":
                    cur["w"], cur["h"] = int(parts[1]), int(parts[2])
                elif kind == "team":
                    cur["teams"][int(parts[1])] = " ".join(parts[2:])
                elif kind == "tile":
                    v = [int(p) for p in parts[1:]]
                    cur["tiles"][(v[0], v[1])] = v[2:]
                elif kind == "player":
                    v = [int(p) for p in parts[1:]]
                    cur["players"].append(v)
                elif kind == "sound":
                    with self.lock:
                        self.sounds.append((int(parts[1]), int(parts[2]),
                                            " ".join(parts[3:]), time.time()))
                elif kind == "win":
                    self.status = "GAME OVER - team %s wins" % " ".join(parts[1:])
                elif kind == "end":
                    with self.lock:
                        self.frame = cur
                    cur = self._empty()
            except (ValueError, IndexError):
                continue
        self.status = "disconnected"

    @staticmethod
    def _empty():
        return {"w": 0, "h": 0, "teams": {}, "tiles": {}, "players": []}


class App:
    def __init__(self, host, port):
        self.net = Net(host, port)
        self.net.start()
        self.root = tk.Tk()
        self.root.title("Zappy")
        self.status = tk.Label(self.root, anchor="w", font=("monospace", 10))
        self.status.pack(fill="x")
        self.canvas = tk.Canvas(self.root, width=900, height=720, bg="#1e272e",
                                highlightthickness=0)
        self.canvas.pack(fill="both", expand=True)
        self.canvas.bind("<Button-1>", self.on_click)
        self.selected = None
        self.cell = 40
        self.ox = self.oy = 0
        self.redraw()

    # ------------------------------------------------------------------
    def on_click(self, event):
        frame = self.net.frame
        if not frame or not frame["w"]:
            return
        x = int((event.x - self.ox) // self.cell)
        y = int((event.y - self.oy) // self.cell)
        if 0 <= x < frame["w"] and 0 <= y < frame["h"]:
            self.selected = None if self.selected == (x, y) else (x, y)
        else:
            self.selected = None

    def center(self, x, y):
        return (self.ox + (x + 0.5) * self.cell, self.oy + (y + 0.5) * self.cell)

    # ------------------------------------------------------------------
    def redraw(self):
        c = self.canvas
        c.delete("all")
        with self.net.lock:
            frame = self.net.frame
            now = time.time()
            self.net.sounds = [s for s in self.net.sounds if now - s[3] < SOUND_LIFETIME]
            sounds = list(self.net.sounds)
        if not frame or not frame["w"]:
            self.status.config(text=self.net.status)
            self.root.after(100, self.redraw)
            return

        w, h = frame["w"], frame["h"]
        cw = max(c.winfo_width(), 200)
        ch = max(c.winfo_height(), 200)
        self.cell = max(8, min(cw // w, ch // h))
        self.ox = (cw - self.cell * w) // 2
        self.oy = (ch - self.cell * h) // 2
        cell = self.cell

        # grid
        for gx in range(w + 1):
            c.create_line(self.ox + gx * cell, self.oy, self.ox + gx * cell,
                          self.oy + h * cell, fill="#485460")
        for gy in range(h + 1):
            c.create_line(self.ox, self.oy + gy * cell, self.ox + w * cell,
                          self.oy + gy * cell, fill="#485460")

        # resources
        for (x, y), v in frame["tiles"].items():
            left = self.ox + x * cell
            top = self.oy + y * cell
            if v[0] > 0:
                r = cell * 0.12
                c.create_oval(left + 2, top + 2, left + 2 + 2 * r, top + 2 + 2 * r,
                              fill="#ff9f43", outline="")
            for k in range(6):
                if v[1 + k] > 0:
                    sx = left + cell * (0.12 + 0.28 * (k % 3))
                    sy = top + cell * (0.62 + 0.18 * (k // 3))
                    s = cell * 0.15
                    c.create_rectangle(sx, sy, sx + s, sy + s,
                                       fill=STONE_COLORS[k], outline="")

        # players
        by_tile = {}
        for p in frame["players"]:
            by_tile.setdefault((p[2], p[3]), []).append(p)
        for (x, y), group in by_tile.items():
            for n, p in enumerate(group):
                pid, team, _, _, d, level = p[0], p[1], p[2], p[3], p[4], p[5]
                locked = p[13]
                cx, cy = self.center(x, y)
                cx += (n % 2) * cell * 0.18 - cell * 0.09 * (1 if len(group) > 1 else 0)
                cy += (n // 2) * cell * 0.18
                r = cell * 0.28
                pts = {
                    0: [cx, cy - r, cx - r, cy + r, cx + r, cy + r],
                    1: [cx + r, cy, cx - r, cy - r, cx - r, cy + r],
                    2: [cx, cy + r, cx - r, cy - r, cx + r, cy - r],
                    3: [cx - r, cy, cx + r, cy - r, cx + r, cy + r],
                }[d % 4]
                color = TEAM_COLORS[team % len(TEAM_COLORS)]
                c.create_polygon(pts, fill=color, outline="#f9ca24" if locked else "black",
                                 width=3 if locked else 1)
                c.create_text(cx, cy, text=str(level), fill="black",
                              font=("monospace", max(7, int(cell * 0.22)), "bold"))

        # sounds (expanding rings)
        for sx, sy, text, t0 in sounds:
            age = (now - t0) / SOUND_LIFETIME
            cx, cy = self.center(sx, sy)
            for ring in range(3):
                rr = cell * (0.4 + 2.5 * age + 0.5 * ring)
                shade = int(255 * (1 - age))
                col = "#%02x%02x%02x" % (shade, shade, 255)
                c.create_oval(cx - rr, cy - rr, cx + rr, cy + rr, outline=col, width=2)
            c.create_text(cx, cy - cell * 0.6 - 10 * age * cell / 10, text=text[:24],
                          fill="#ffffff", font=("monospace", 9))

        # details of the selected square
        if self.selected:
            self.draw_details(frame, self.selected)

        # status line
        teams = frame["teams"]
        summary = []
        for ti, name in sorted(teams.items()):
            members = [p for p in frame["players"] if p[1] == ti]
            top = max([p[5] for p in members], default=0)
            summary.append("%s: %d players, best level %d" % (name, len(members), top))
        self.status.config(text="%s | %dx%d | %s" % (
            self.net.status, w, h, " | ".join(summary)))
        self.root.after(100, self.redraw)

    def draw_details(self, frame, sel):
        x, y = sel
        c = self.canvas
        v = frame["tiles"].get((x, y), [0] * 7)
        lines = ["Square (%d, %d)" % (x, y), "food: %d" % v[0]]
        for k in range(6):
            lines.append("%s: %d" % (STONES[k], v[1 + k]))
        players = [p for p in frame["players"] if p[2] == x and p[3] == y]
        lines.append("players: %d" % len(players))
        for p in players:
            team = frame["teams"].get(p[1], "?")
            inv = ",".join(str(n) for n in p[7:13])
            lines.append("  #%d %s L%d food %d facing %s%s" % (
                p[0], team, p[5], p[6], DIR_NAMES[p[4] % 4],
                " (enchanting)" if p[13] else ""))
            lines.append("     stones [%s]" % inv)
        width = 270
        height = 16 * len(lines) + 12
        cx, cy = self.center(x, y)
        bx = min(cx + self.cell / 2 + 6, self.canvas.winfo_width() - width - 4)
        by = min(max(cy - height / 2, 4), self.canvas.winfo_height() - height - 4)
        c.create_rectangle(self.ox + x * self.cell, self.oy + y * self.cell,
                           self.ox + (x + 1) * self.cell, self.oy + (y + 1) * self.cell,
                           outline="#f9ca24", width=3)
        c.create_rectangle(bx, by, bx + width, by + height, fill="#2f3640",
                           outline="#f5f6fa")
        for i, text in enumerate(lines):
            c.create_text(bx + 8, by + 8 + i * 16, text=text, anchor="nw",
                          fill="#f5f6fa", font=("monospace", 9))

    def run(self):
        self.root.mainloop()


def main():
    parser = argparse.ArgumentParser(add_help=False)
    parser.add_argument("-p", dest="port", type=int, required=True)
    parser.add_argument("-h", dest="host", default="localhost")
    args = parser.parse_args()
    App(args.host, args.port).run()


if __name__ == "__main__":
    main()
