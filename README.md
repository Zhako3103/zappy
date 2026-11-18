# zappy

A multiplayer network simulation game made of three independent programs: a **zero-dependency non-blocking server in Rust** (talking to `poll(2)` via FFI), an **autonomous AI agent in Python**, and a **2D graphical client**.

## Components

| Component | Language | Role |
| --- | --- | --- |
| `server` | Rust | Authoritative game world, TCP protocol, tick scheduler |
| `ai` | Python | Autonomous player that decides what to do from `look` / `inventory` replies |
| `gui` | 2D GUI | Read-only observer that renders the world |

## Server architecture

- A single thread multiplexes the listening socket and all clients with `poll(2)`, called through hand-written FFI declarations. No external crates are used.
- All sockets are non-blocking; each client owns an input buffer (commands are split on newlines) and an output buffer.
- **Tick-based time.** The world advances in ticks derived from the configured frequency `f`. Every command has a fixed duration expressed in ticks, and queued commands are executed in order when their deadline is reached.
- Each player has a bounded command queue, a food clock and an inventory; the map is a grid of tiles containing resources.

## Command protocol

Text-based, one command per line:

| Command | Effect |
| --- | --- |
| `Forward` | Move one tile ahead |
| `Right` / `Left` | Turn 90 degrees |
| `Look` | Describe visible tiles |
| `Inventory` | Report carried resources |
| `Take <object>` / `Set <object>` | Pick up or drop a resource |
| `Broadcast <text>` | Send a message heard by every player |
| `Incantation` | Start an elevation ritual |
| `Fork` | Lay an egg for the team |
| `Connect_nbr` | Number of free team slots |

### Toroidal sound direction

The map wraps around on both axes. For `Broadcast`, the server computes the **shortest path on the torus** between the emitter and each receiver and converts it to one of 8 directions relative to the receiver's orientation (`0` means the receiver is on the same tile).

## Running

Server:

```bash
cd server
cargo build --release
./target/release/server -p 4242 -x 20 -y 20 -n team1 team2 -c 5 -f 100
```

AI client:

```bash
cd ai
python3 client.py -n team1 -p 4242 -h 127.0.0.1
```

GUI:

```bash
cd gui
./run_gui.sh 127.0.0.1 4242
```

## What this project demonstrates

Low-level network programming, a custom tick scheduler, protocol design, FFI in Rust, and an autonomous agent communicating over a plain TCP text protocol.
