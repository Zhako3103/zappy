#!/usr/bin/env python3
"""Autonomous Zappy player.

Usage: ./client -n <team> -p <port> [-h <hostname>]

Strategy (no communication outside the game, only `broadcast`):
  * the first bot of a team ("founder") starts helper bots on the free slots
    and calls `fork` when the team needs more members;
  * every bot keeps its food high, looks for the stones needed for the next
    level and walks to them;
  * a bot that owns all stones for its level drops them and calls teammates of
    the same level with `broadcast`; they walk towards the sound until they
    stand on the same square, then the caller starts the enchantment.
"""
import argparse
import random
import socket
import subprocess
import sys

STONES = ["jade", "peridot", "amber", "amethyst", "garnet", "ammolite"]
# index = current level - 1: (players needed, stones needed)
RITUALS = [
    (1, [1, 0, 0, 0, 0, 0]),
    (2, [1, 1, 1, 0, 0, 0]),
    (2, [2, 0, 1, 0, 2, 0]),
    (4, [1, 1, 2, 0, 1, 0]),
    (4, [1, 2, 1, 3, 0, 0]),
    (6, [1, 2, 3, 0, 1, 0]),
    (6, [2, 2, 2, 2, 2, 1]),
]
TEAM_TARGET = 8  # members the founder tries to gather (6 needed to win)
FOOD_SAFE = 8


class Dead(Exception):
    pass


class Bot:
    def __init__(self, team, host, port, founder):
        self.team = team
        self.host = host
        self.port = port
        self.founder = founder
        self.level = 1
        self.call = None  # (direction, level, loop number) of the last call heard
        self.loops = 0
        self.forks_done = 0
        self.sock = socket.create_connection((host, port))
        self.rf = self.sock.makefile("r", encoding="utf-8", newline="\n")
        self.wf = self.sock.makefile("w", encoding="utf-8", newline="\n")
        if self._readline() != "WELCOME":
            raise SystemExit("bad greeting from server")
        self._write(team)
        first = self._readline()
        if not first.isdigit():
            raise SystemExit(first)  # unknown team / team full
        self.free_slots = int(first)
        self.width, self.height = (int(v) for v in self._readline().split())

    # ------------------------------------------------------------------
    # protocol helpers
    # ------------------------------------------------------------------
    def _write(self, text):
        self.wf.write(text + "\n")
        self.wf.flush()

    def _readline(self):
        line = self.rf.readline()
        if line == "":
            raise Dead()
        return line.rstrip("\n")

    def _next(self):
        """Next line that is not an asynchronous event."""
        while True:
            line = self._readline()
            if line.startswith("message "):
                self._on_message(line)
            elif line.startswith("moving "):
                continue
            elif line == "death":
                raise Dead()
            elif line.startswith("win "):
                raise SystemExit(0)
            else:
                return line

    def send(self, cmd):
        self._write(cmd)
        while True:
            line = self._next()
            if line == "evolution in progress":
                continue
            if line.startswith("current level"):
                self.level = int(line.split(":")[1])
                continue
            return line

    def _on_message(self, line):
        try:
            head, text = line[len("message "):].split(",", 1)
            direction = int(head)
            tag, team, level = text.rsplit(":", 2)
            if tag == "zappy" and team == self.team:
                self.call = (direction, int(level), self.loops)
        except ValueError:
            pass

    # ------------------------------------------------------------------
    # commands
    # ------------------------------------------------------------------
    def inventory(self):
        raw = self.send("inventory").strip("{}")
        inv = {}
        for part in raw.split(","):
            fields = part.split()
            if len(fields) == 2 and fields[1].isdigit():
                inv[fields[0]] = int(fields[1])
        inv.setdefault("food", 0)
        return inv

    def see(self):
        raw = self.send("see").strip("{}")
        return [cell.strip() for cell in raw.split(",")]

    def incant(self):
        self._write("enchantment")
        line = self._next()
        if line != "evolution in progress":
            return False
        while True:
            line = self._next()
            if line.startswith("current level"):
                self.level = int(line.split(":")[1])
                return True
            if line == "ko":
                return False

    def advance(self, n=1):
        for _ in range(n):
            self.send("advance")

    # ------------------------------------------------------------------
    # team management (founder only)
    # ------------------------------------------------------------------
    def manage_team(self):
        free = int(self.send("connect_nbr"))
        for _ in range(free):
            subprocess.Popen(
                [sys.executable, __file__, "-n", self.team, "-p", str(self.port),
                 "-h", self.host, "--helper"],
                stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
            )
        known = 1 + self.free_slots
        wanted_forks = max(0, TEAM_TARGET - known)
        if free == 0 and self.forks_done < wanted_forks:
            if self.send("fork") == "ok":
                self.forks_done += 1

    # ------------------------------------------------------------------
    # decision making
    # ------------------------------------------------------------------
    def missing(self, inv):
        if self.level >= 8:
            return {}
        _, req = RITUALS[self.level - 1]
        return {STONES[k]: req[k] - inv.get(STONES[k], 0)
                for k in range(6) if req[k] > inv.get(STONES[k], 0)}

    @staticmethod
    def coords(index):
        """Square number in `see` output -> (row ahead, column; negative = left)."""
        row = int(index ** 0.5)
        return row, index - row * row - row

    def goto(self, row, col):
        self.advance(row)
        if col < 0:
            self.send("left")
        elif col > 0:
            self.send("right")
        self.advance(abs(col))

    def wander(self):
        if random.random() < 0.4:
            self.send(random.choice(["left", "right"]))
        self.advance(random.randint(1, 3))

    def gather(self, inv, miss):
        food = inv["food"]
        wanted = []
        if food < 12 or not miss:
            wanted.append("food")
        wanted += list(miss.keys())
        if "food" not in wanted:
            wanted.append("food")
        cells = self.see()
        target = None
        for item in wanted:
            best = None
            for i, cell in enumerate(cells):
                if item in cell.split():
                    row, col = self.coords(i)
                    dist = row + abs(col)
                    if best is None or dist < best[0]:
                        best = (dist, i)
            if best is not None:
                target = best[1]
                break
        if target is None:
            self.wander()
            return
        row, col = self.coords(target)
        self.goto(row, col)
        useful = set(["food"] + list(miss.keys()))
        for obj in cells[target].split():
            if obj in useful:
                self.send("pick " + obj)

    def step_toward(self, k):
        if k in (1, 2, 8):
            pass
        elif k in (3, 4):
            self.send("left")
        elif k in (6, 7):
            self.send("right")
        elif k == 5:
            self.send("right")
            self.send("right")
        self.advance()

    def level_up(self):
        need, req = RITUALS[self.level - 1]
        level_before = self.level
        for k, count in enumerate(req):
            for _ in range(count):
                self.send("drop " + STONES[k])
        ok = False
        if need == 1:
            ok = self.incant()
        else:
            for _ in range(25):
                self.send("broadcast zappy:%s:%d" % (self.team, self.level))
                here = self.see()[0].split()
                if here.count("player") >= need - 1:
                    ok = self.incant()
                    break
        if not ok and self.level == level_before:
            # give up: take the stones back and try again later
            for k, count in enumerate(req):
                for _ in range(count):
                    self.send("pick " + STONES[k])

    def run(self):
        while True:
            self.loops += 1
            if self.founder and self.loops % 6 == 1:
                self.manage_team()
            inv = self.inventory()
            miss = self.missing(inv)
            if self.level < 8 and not miss and inv["food"] >= FOOD_SAFE:
                self.level_up()
                continue
            call = self.call
            if (call and call[1] == self.level and miss
                    and self.loops - call[2] <= 2 and inv["food"] >= 5):
                self.call = None
                if call[0] == 0:
                    self.see()  # wait on the square for the caller
                else:
                    self.step_toward(call[0])
                continue
            self.gather(inv, miss)


def main():
    parser = argparse.ArgumentParser(add_help=False)
    parser.add_argument("-n", dest="team", required=True)
    parser.add_argument("-p", dest="port", type=int, required=True)
    parser.add_argument("-h", dest="host", default="localhost")
    parser.add_argument("--helper", action="store_true")
    args = parser.parse_args()
    try:
        bot = Bot(args.team, args.host, args.port, founder=not args.helper)
        bot.run()
    except (Dead, ConnectionError, BrokenPipeError):
        pass
    except KeyboardInterrupt:
        pass


if __name__ == "__main__":
    main()
