//! Game state and rules. Knows nothing about sockets: every message for a
//! player is pushed into its `outbox`, and `main.rs` ships it over TCP.

use std::collections::{BTreeMap, VecDeque};

use crate::geometry::{direction_number, sound_direction, wrap, Dir};
use crate::rng::Rng;

pub const STONES: [&str; 6] = ["jade", "peridot", "amber", "amethyst", "garnet", "ammolite"];
/// One unit of food keeps a player alive for 126 time units.
pub const FOOD_LIFE: u64 = 126;
pub const MAX_LEVEL: usize = 8;
/// A client may have at most 10 pending requests.
pub const QUEUE_LIMIT: usize = 10;
pub const WIN_COUNT: usize = 6;
const RESPAWN_EVERY: u64 = 20;
const FORK_CALL_TIME: u64 = 48;
const EGG_ARRIVAL_TIME: u64 = 600;
const INCANT_TIME: u64 = 300;

/// (players needed, stones needed [jade, peridot, amber, amethyst, garnet, ammolite])
/// index = current level - 1
const RITUALS: [(usize, [u32; 6]); 7] = [
    (1, [1, 0, 0, 0, 0, 0]),
    (2, [1, 1, 1, 0, 0, 0]),
    (2, [2, 0, 1, 0, 2, 0]),
    (4, [1, 1, 2, 0, 1, 0]),
    (4, [1, 2, 1, 3, 0, 0]),
    (6, [1, 2, 3, 0, 1, 0]),
    (6, [2, 2, 2, 2, 2, 1]),
];

fn ritual(level: usize) -> Option<(usize, [u32; 6])> {
    if (1..=7).contains(&level) {
        Some(RITUALS[level - 1])
    } else {
        None
    }
}

pub struct Config {
    pub width: u32,
    pub height: u32,
    pub teams: Vec<String>,
    pub clients: u32,
    pub time: u32,
    pub seed: u64,
    pub density: f64,
    pub log: bool,
}

#[derive(Clone, Default)]
struct Tile {
    food: u32,
    stones: [u32; 6],
}

struct Player {
    team: usize,
    x: i32,
    y: i32,
    dir: Dir,
    level: usize,
    stones: [u32; 6],
    /// Tick at which the player starves. Food is stored as remaining life.
    life_end: u64,
    queue: VecDeque<String>,
    /// Command currently being executed: (finish tick, command line).
    busy: Option<(u64, String)>,
    /// True while taking part in an enchantment.
    locked: bool,
    dead: bool,
    outbox: Vec<String>,
}

impl Player {
    fn food_units(&self, now: u64) -> u64 {
        if self.life_end <= now {
            0
        } else {
            (self.life_end - now + FOOD_LIFE - 1) / FOOD_LIFE
        }
    }
}

struct Team {
    name: String,
    free: u32,
    /// Positions of eggs that already hatched (spawn points for new clients).
    hatched: Vec<(i32, i32)>,
}

struct Egg {
    at: u64,
    team: usize,
    x: i32,
    y: i32,
}

struct Incant {
    members: Vec<u32>,
    x: i32,
    y: i32,
    level: usize,
    end: u64,
}

enum Step {
    Run,
    Start,
    Wait,
}

pub struct Game {
    w: i32,
    h: i32,
    tiles: Vec<Tile>,
    teams: Vec<Team>,
    players: BTreeMap<u32, Player>,
    eggs: Vec<Egg>,
    incants: Vec<Incant>,
    rng: Rng,
    tick: u64,
    next_spawn: u64,
    density: f64,
    winner: Option<String>,
    sounds: Vec<(i32, i32, String)>,
    log: Vec<String>,
    verbose: bool,
}

impl Game {
    pub fn new(cfg: &Config) -> Game {
        let w = cfg.width as i32;
        let h = cfg.height as i32;
        let teams = cfg
            .teams
            .iter()
            .map(|n| Team {
                name: n.clone(),
                free: cfg.clients,
                hatched: Vec::new(),
            })
            .collect();
        let mut g = Game {
            w,
            h,
            tiles: vec![Tile::default(); (w * h) as usize],
            teams,
            players: BTreeMap::new(),
            eggs: Vec::new(),
            incants: Vec::new(),
            rng: Rng::new(cfg.seed),
            tick: 0,
            next_spawn: RESPAWN_EVERY,
            density: cfg.density,
            winner: None,
            sounds: Vec::new(),
            log: Vec::new(),
            verbose: cfg.log,
        };
        g.top_up();
        g
    }

    // ------------------------------------------------------------------
    // Public API used by the network layer
    // ------------------------------------------------------------------

    pub fn size(&self) -> (i32, i32) {
        (self.w, self.h)
    }

    pub fn team_index(&self, name: &str) -> Option<usize> {
        self.teams.iter().position(|t| t.name == name)
    }

    pub fn team_free(&self, team: usize) -> u32 {
        self.teams[team].free
    }

    pub fn winner(&self) -> Option<String> {
        self.winner.clone()
    }

    pub fn take_log(&mut self) -> Vec<String> {
        std::mem::take(&mut self.log)
    }

    pub fn clear_sounds(&mut self) {
        self.sounds.clear();
    }

    /// Creates a player for connection `id`. Returns the remaining free slots.
    pub fn add_player(&mut self, id: u32, team: usize, now: u64) -> u32 {
        let hatched = self.teams[team].hatched.pop();
        let (x, y) = match hatched {
            Some(pos) => pos,
            None => {
                let x = self.rng.below(self.w as usize) as i32;
                let y = self.rng.below(self.h as usize) as i32;
                (x, y)
            }
        };
        let dir = Dir::from_index(self.rng.below(4));
        let t = &mut self.teams[team];
        t.free = t.free.saturating_sub(1);
        let free = t.free;
        let name = t.name.clone();
        self.players.insert(
            id,
            Player {
                team,
                x,
                y,
                dir,
                level: 1,
                stones: [0; 6],
                life_end: now + 10 * FOOD_LIFE,
                queue: VecDeque::new(),
                busy: None,
                locked: false,
                dead: false,
                outbox: Vec::new(),
            },
        );
        self.note(format!("player {id} joined team {name} at ({x},{y})"));
        free
    }

    /// Connection closed by the client.
    pub fn disconnect(&mut self, id: u32) {
        if let Some(p) = self.players.remove(&id) {
            if !p.dead {
                self.teams[p.team].free += 1;
            }
            self.note(format!("player {id} disconnected"));
        }
    }

    pub fn enqueue(&mut self, id: u32, line: String) {
        if let Some(p) = self.players.get_mut(&id) {
            if p.dead {
                return;
            }
            let pending = p.queue.len() + if p.busy.is_some() { 1 } else { 0 };
            if pending >= QUEUE_LIMIT {
                return;
            }
            p.queue.push_back(line);
        }
    }

    /// Messages waiting to be sent: (player id, lines).
    pub fn take_outputs(&mut self) -> Vec<(u32, Vec<String>)> {
        let mut out = Vec::new();
        for (id, p) in self.players.iter_mut() {
            if !p.outbox.is_empty() {
                out.push((*id, std::mem::take(&mut p.outbox)));
            }
        }
        out
    }

    /// Removes dead players (call after `take_outputs`) and returns their ids.
    pub fn reap_dead(&mut self) -> Vec<u32> {
        let ids: Vec<u32> = self
            .players
            .iter()
            .filter(|(_, p)| p.dead)
            .map(|(id, _)| *id)
            .collect();
        for id in &ids {
            self.players.remove(id);
        }
        ids
    }

    /// Advances the game up to tick `now`.
    pub fn update(&mut self, now: u64) {
        self.tick = now;

        if now >= self.next_spawn {
            self.top_up();
            self.next_spawn = now + RESPAWN_EVERY;
        }

        let mut i = 0;
        while i < self.eggs.len() {
            if self.eggs[i].at <= now {
                let egg = self.eggs.remove(i);
                let t = &mut self.teams[egg.team];
                t.free += 1;
                t.hatched.push((egg.x, egg.y));
                let name = t.name.clone();
                self.note(format!("egg of team {name} hatched at ({},{})", egg.x, egg.y));
            } else {
                i += 1;
            }
        }

        let ids: Vec<u32> = self.players.keys().copied().collect();
        for id in ids {
            self.step_player(id, now);
        }

        let mut i = 0;
        while i < self.incants.len() {
            if self.incants[i].end <= now {
                let inc = self.incants.remove(i);
                self.finish_incant(inc);
            } else {
                i += 1;
            }
        }

        if self.winner.is_none() {
            let mut found: Option<String> = None;
            for (ti, t) in self.teams.iter().enumerate() {
                let n = self
                    .players
                    .values()
                    .filter(|p| p.team == ti && !p.dead && p.level >= MAX_LEVEL)
                    .count();
                if n >= WIN_COUNT {
                    found = Some(t.name.clone());
                    break;
                }
            }
            if let Some(name) = found {
                self.note(format!("team {name} wins"));
                self.winner = Some(name);
            }
        }
    }

    /// Text frame for the graphical client.
    pub fn snapshot(&mut self) -> String {
        let mut s = String::new();
        s.push_str(&format!("size {} {}\n", self.w, self.h));
        for (i, t) in self.teams.iter().enumerate() {
            s.push_str(&format!("team {} {}\n", i, t.name));
        }
        for y in 0..self.h {
            for x in 0..self.w {
                let t = &self.tiles[(y * self.w + x) as usize];
                if t.food > 0 || t.stones.iter().any(|&n| n > 0) {
                    s.push_str(&format!(
                        "tile {} {} {} {} {} {} {} {} {}\n",
                        x, y, t.food, t.stones[0], t.stones[1], t.stones[2], t.stones[3],
                        t.stones[4], t.stones[5]
                    ));
                }
            }
        }
        for (id, p) in self.players.iter() {
            if p.dead {
                continue;
            }
            s.push_str(&format!(
                "player {} {} {} {} {} {} {} {} {} {} {} {} {} {}\n",
                id,
                p.team,
                p.x,
                p.y,
                p.dir.index(),
                p.level,
                p.food_units(self.tick),
                p.stones[0],
                p.stones[1],
                p.stones[2],
                p.stones[3],
                p.stones[4],
                p.stones[5],
                if p.locked { 1 } else { 0 }
            ));
        }
        for (x, y, text) in self.sounds.drain(..) {
            s.push_str(&format!("sound {} {} {}\n", x, y, text.replace('\n', " ")));
        }
        s.push_str("end\n");
        s
    }

    // ------------------------------------------------------------------
    // Resources
    // ------------------------------------------------------------------

    /// Keeps the amount of resources on the map close to the target.
    /// Rules: max 1 food per square, max 1 stone of each type per square,
    /// max 3 different stones per square.
    fn top_up(&mut self) {
        let cells = (self.w * self.h) as usize;
        let food_target = (((cells as f64) * 0.5 * self.density).round() as usize).clamp(1, cells);
        let stone_target = (((cells as f64) * 0.15 * self.density).round() as usize)
            .clamp(1, (cells / 2).max(1));

        let food_now: usize = self.tiles.iter().map(|t| t.food as usize).sum();
        for _ in food_now..food_target {
            self.spawn_food();
        }
        for k in 0..6 {
            let now_k = self.tiles.iter().filter(|t| t.stones[k] > 0).count();
            for _ in now_k..stone_target {
                self.spawn_stone(k);
            }
        }
    }

    fn spawn_food(&mut self) {
        for _ in 0..100 {
            let i = self.rng.below(self.tiles.len());
            if self.tiles[i].food == 0 {
                self.tiles[i].food = 1;
                return;
            }
        }
    }

    fn spawn_stone(&mut self, k: usize) {
        for _ in 0..100 {
            let i = self.rng.below(self.tiles.len());
            let kinds = self.tiles[i].stones.iter().filter(|&&n| n > 0).count();
            if self.tiles[i].stones[k] == 0 && kinds < 3 {
                self.tiles[i].stones[k] = 1;
                return;
            }
        }
    }

    // ------------------------------------------------------------------
    // Player scheduling
    // ------------------------------------------------------------------

    fn note(&mut self, msg: String) {
        if self.verbose {
            self.log.push(msg);
        }
    }

    fn reply(&mut self, id: u32, msg: &str) {
        if let Some(p) = self.players.get_mut(&id) {
            p.outbox.push(msg.to_string());
        }
    }

    fn kill(&mut self, id: u32) {
        let team = match self.players.get_mut(&id) {
            Some(p) if !p.dead => {
                p.dead = true;
                p.outbox.push("death".to_string());
                p.team
            }
            _ => return,
        };
        self.teams[team].free += 1;
        self.note(format!("player {id} died"));
    }

    fn step_player(&mut self, id: u32, now: u64) {
        let starved = match self.players.get(&id) {
            Some(p) if !p.dead => now >= p.life_end,
            _ => return,
        };
        if starved {
            self.kill(id);
            return;
        }

        loop {
            let step = match self.players.get(&id) {
                Some(p) if !p.dead && !p.locked => match &p.busy {
                    Some((end, _)) => {
                        if *end <= now {
                            Step::Run
                        } else {
                            Step::Wait
                        }
                    }
                    None => {
                        if p.queue.is_empty() {
                            Step::Wait
                        } else {
                            Step::Start
                        }
                    }
                },
                _ => return,
            };
            match step {
                Step::Run => {
                    let cmd = self
                        .players
                        .get_mut(&id)
                        .and_then(|p| p.busy.take())
                        .map(|(_, c)| c)
                        .unwrap_or_default();
                    self.exec(id, &cmd, now);
                }
                Step::Start => {
                    let cmd = self
                        .players
                        .get_mut(&id)
                        .and_then(|p| p.queue.pop_front())
                        .unwrap_or_default();
                    self.begin(id, cmd, now);
                }
                Step::Wait => return,
            }
        }
    }

    /// Called when a command leaves the queue: sets its duration.
    fn begin(&mut self, id: u32, cmd: String, now: u64) {
        let name = cmd.split_whitespace().next().unwrap_or("").to_string();
        if name == "enchantment" {
            if cmd.trim() != "enchantment" {
                self.reply(id, "ko");
            } else {
                self.start_incant(id, now);
            }
            return;
        }
        let delay = match name.as_str() {
            "advance" | "right" | "left" | "see" | "pick" | "drop" | "kick" | "broadcast" => Some(7),
            "inventory" => Some(1),
            "fork" => Some(FORK_CALL_TIME),
            "connect_nbr" => Some(0),
            _ => None,
        };
        match delay {
            Some(d) => {
                if let Some(p) = self.players.get_mut(&id) {
                    p.busy = Some((now + d, cmd));
                }
            }
            None => self.reply(id, "ko"),
        }
    }

    /// Called when the duration of a command has elapsed: applies its effect.
    fn exec(&mut self, id: u32, cmd: &str, now: u64) {
        let (name, arg) = match cmd.split_once(' ') {
            Some((n, a)) => (n, a.trim()),
            None => (cmd.trim(), ""),
        };
        let takes_no_arg = matches!(
            name,
            "advance" | "right" | "left" | "see" | "inventory" | "kick" | "fork" | "connect_nbr"
        );
        if takes_no_arg && !arg.is_empty() {
            self.reply(id, "ko");
            return;
        }
        match name {
            "advance" => self.do_advance(id),
            "right" => self.do_turn(id, true),
            "left" => self.do_turn(id, false),
            "see" => {
                let s = self.see(id);
                self.reply(id, &s);
            }
            "inventory" => {
                let s = self.inventory(id, now);
                self.reply(id, &s);
            }
            "pick" => self.do_pick(id, arg),
            "drop" => self.do_drop(id, arg, now),
            "kick" => self.do_kick(id),
            "broadcast" => self.do_broadcast(id, arg),
            "fork" => self.do_fork(id, now),
            "connect_nbr" => {
                let free = match self.players.get(&id) {
                    Some(p) => self.teams[p.team].free,
                    None => 0,
                };
                self.reply(id, &free.to_string());
            }
            _ => self.reply(id, "ko"),
        }
    }

    // ------------------------------------------------------------------
    // Commands
    // ------------------------------------------------------------------

    fn pos(&self, id: u32) -> Option<(i32, i32)> {
        self.players.get(&id).map(|p| (p.x, p.y))
    }

    fn do_advance(&mut self, id: u32) {
        let (w, h) = (self.w, self.h);
        if let Some(p) = self.players.get_mut(&id) {
            let (dx, dy) = p.dir.vec();
            p.x = wrap(p.x + dx, w);
            p.y = wrap(p.y + dy, h);
            p.outbox.push("ok".to_string());
        }
    }

    fn do_turn(&mut self, id: u32, right: bool) {
        if let Some(p) = self.players.get_mut(&id) {
            p.dir = if right { p.dir.right() } else { p.dir.left() };
            p.outbox.push("ok".to_string());
        }
    }

    fn see(&self, id: u32) -> String {
        let p = match self.players.get(&id) {
            Some(p) => p,
            None => return "{}".to_string(),
        };
        let (fx, fy) = p.dir.vec();
        let (rx, ry) = (-fy, fx); // right-hand vector
        let mut cells: Vec<String> = Vec::new();
        for row in 0..=(p.level as i32) {
            for col in -row..=row {
                let x = wrap(p.x + fx * row + rx * col, self.w);
                let y = wrap(p.y + fy * row + ry * col, self.h);
                let mut items: Vec<String> = Vec::new();
                for (oid, o) in self.players.iter() {
                    if *oid != id && !o.dead && o.x == x && o.y == y {
                        items.push("player".to_string());
                    }
                }
                let t = &self.tiles[(y * self.w + x) as usize];
                for _ in 0..t.food {
                    items.push("food".to_string());
                }
                for k in 0..6 {
                    for _ in 0..t.stones[k] {
                        items.push(STONES[k].to_string());
                    }
                }
                cells.push(items.join(" "));
            }
        }
        format!("{{{}}}", cells.join(", "))
    }

    fn inventory(&self, id: u32, now: u64) -> String {
        let p = match self.players.get(&id) {
            Some(p) => p,
            None => return "{}".to_string(),
        };
        let mut parts = vec![format!("food {}", p.food_units(now))];
        for k in 0..6 {
            parts.push(format!("{} {}", STONES[k], p.stones[k]));
        }
        format!("{{{}}}", parts.join(", "))
    }

    fn do_pick(&mut self, id: u32, obj: &str) {
        let (x, y) = match self.pos(id) {
            Some(p) => p,
            None => return,
        };
        let idx = (y * self.w + x) as usize;
        let mut ok = false;
        if obj == "food" {
            if self.tiles[idx].food > 0 {
                self.tiles[idx].food -= 1;
                if let Some(p) = self.players.get_mut(&id) {
                    p.life_end += FOOD_LIFE;
                }
                ok = true;
            }
        } else if let Some(k) = STONES.iter().position(|s| *s == obj) {
            if self.tiles[idx].stones[k] > 0 {
                self.tiles[idx].stones[k] -= 1;
                if let Some(p) = self.players.get_mut(&id) {
                    p.stones[k] += 1;
                }
                ok = true;
            }
        }
        self.reply(id, if ok { "ok" } else { "ko" });
    }

    fn do_drop(&mut self, id: u32, obj: &str, now: u64) {
        let (x, y) = match self.pos(id) {
            Some(p) => p,
            None => return,
        };
        let idx = (y * self.w + x) as usize;
        let mut ok = false;
        if obj == "food" {
            // A player can only give away food it can afford to lose.
            if let Some(p) = self.players.get_mut(&id) {
                if p.life_end > now + FOOD_LIFE {
                    p.life_end -= FOOD_LIFE;
                    self.tiles[idx].food += 1;
                    ok = true;
                }
            }
        } else if let Some(k) = STONES.iter().position(|s| *s == obj) {
            if let Some(p) = self.players.get_mut(&id) {
                if p.stones[k] > 0 {
                    p.stones[k] -= 1;
                    self.tiles[idx].stones[k] += 1;
                    ok = true;
                }
            }
        }
        self.reply(id, if ok { "ok" } else { "ko" });
    }

    fn do_kick(&mut self, id: u32) {
        let (x, y, dir) = match self.players.get(&id) {
            Some(p) => (p.x, p.y, p.dir),
            None => return,
        };
        let (dx, dy) = dir.vec();
        let victims: Vec<u32> = self
            .players
            .iter()
            .filter(|(oid, o)| **oid != id && !o.dead && !o.locked && o.x == x && o.y == y)
            .map(|(oid, _)| *oid)
            .collect();
        if victims.is_empty() {
            self.reply(id, "ko");
            return;
        }
        let (w, h) = (self.w, self.h);
        for v in victims {
            if let Some(o) = self.players.get_mut(&v) {
                o.x = wrap(o.x + dx, w);
                o.y = wrap(o.y + dy, h);
                // Direction the push comes from, relative to the victim.
                let k = direction_number(-dx, -dy, o.dir);
                o.outbox.push(format!("moving {}", k));
            }
        }
        self.reply(id, "ok");
    }

    fn do_broadcast(&mut self, id: u32, text: &str) {
        if text.is_empty() {
            self.reply(id, "ko");
            return;
        }
        let (sx, sy) = match self.pos(id) {
            Some(p) => p,
            None => return,
        };
        let (w, h) = (self.w, self.h);
        for (oid, o) in self.players.iter_mut() {
            if *oid == id || o.dead {
                continue;
            }
            let k = sound_direction(o.x, o.y, o.dir, sx, sy, w, h);
            o.outbox.push(format!("message {},{}", k, text));
        }
        if self.sounds.len() < 500 {
            self.sounds.push((sx, sy, text.to_string()));
        }
        self.reply(id, "ok");
    }

    fn do_fork(&mut self, id: u32, now: u64) {
        let (x, y, team) = match self.players.get(&id) {
            Some(p) => (p.x, p.y, p.team),
            None => return,
        };
        self.eggs.push(Egg {
            at: now + EGG_ARRIVAL_TIME,
            team,
            x,
            y,
        });
        self.note(format!("player {id} forked (ship arrives in {EGG_ARRIVAL_TIME} units)"));
        self.reply(id, "ok");
    }

    // ------------------------------------------------------------------
    // Enchantment ritual
    // ------------------------------------------------------------------

    fn start_incant(&mut self, id: u32, now: u64) {
        let (x, y, level) = match self.players.get(&id) {
            Some(p) => (p.x, p.y, p.level),
            None => return,
        };
        let (need, stones) = match ritual(level) {
            Some(r) => r,
            None => {
                self.reply(id, "ko");
                return;
            }
        };
        let idx = (y * self.w + x) as usize;
        if (0..6).any(|k| self.tiles[idx].stones[k] < stones[k]) {
            self.reply(id, "ko");
            return;
        }
        let mut members = vec![id];
        for (oid, o) in self.players.iter() {
            if members.len() >= need {
                break;
            }
            if *oid != id && !o.dead && !o.locked && o.x == x && o.y == y && o.level == level {
                members.push(*oid);
            }
        }
        if members.len() < need {
            self.reply(id, "ko");
            return;
        }
        for m in &members {
            if let Some(p) = self.players.get_mut(m) {
                p.locked = true;
                p.outbox.push("evolution in progress".to_string());
            }
        }
        self.note(format!(
            "enchantment level {level}->{} started at ({x},{y}) by player {id} with {} players",
            level + 1,
            members.len()
        ));
        self.incants.push(Incant {
            members,
            x,
            y,
            level,
            end: now + INCANT_TIME,
        });
    }

    fn finish_incant(&mut self, inc: Incant) {
        let alive: Vec<u32> = inc
            .members
            .iter()
            .copied()
            .filter(|m| self.players.get(m).map_or(false, |p| !p.dead))
            .collect();
        let (need, stones) = match ritual(inc.level) {
            Some(r) => r,
            None => return,
        };
        let idx = (inc.y * self.w + inc.x) as usize;
        // If a member died the others continue, unless they are left alone.
        let ok = alive.len() >= need.min(2)
            && (0..6).all(|k| self.tiles[idx].stones[k] >= stones[k]);
        if ok {
            for k in 0..6 {
                self.tiles[idx].stones[k] -= stones[k];
            }
        }
        for m in &alive {
            if let Some(p) = self.players.get_mut(m) {
                p.locked = false;
                if ok {
                    p.level += 1;
                    let msg = format!("current level : {}", p.level);
                    p.outbox.push(msg);
                } else {
                    p.outbox.push("ko".to_string());
                }
            }
        }
        self.note(format!(
            "enchantment at ({},{}) level {} {}",
            inc.x,
            inc.y,
            inc.level,
            if ok { "succeeded" } else { "failed" }
        ));
    }
}
