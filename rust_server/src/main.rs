//! Zappy server: non-blocking TCP server multiplexed with poll(2).

mod game;
mod geometry;
mod rng;

use std::collections::BTreeMap;
use std::io::{ErrorKind, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::os::raw::{c_int, c_short, c_ulong};
use std::os::unix::io::AsRawFd;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use game::{Config, Game};

const USAGE: &str = "Usage: ./server -p <port> -x <width> -y <height> -n <team> [<team>] ... -c <nb> [-t <t>] [-s <seed>] [-d <density>] [-l]
    -p  port number
    -x  width of the world
    -y  height of the world
    -n  names of the teams
    -c  number of authorized clients per team
    -t  time unit divider (the greater t is, the faster the game goes, default 100)
    -s  seed of the random generator (reproducible worlds)
    -d  resource density, 0.1 .. 3.0 (default 1.0)
    -l  log game events on stdout";

// ----------------------------------------------------------------------
// poll(2) binding (std links libc already, no extra crate needed)
// ----------------------------------------------------------------------

#[repr(C)]
struct PollFd {
    fd: c_int,
    events: c_short,
    revents: c_short,
}

extern "C" {
    fn poll(fds: *mut PollFd, nfds: c_ulong, timeout: c_int) -> c_int;
}

const POLLIN: c_short = 0x001;
const POLLOUT: c_short = 0x004;
const POLLERR: c_short = 0x008;
const POLLHUP: c_short = 0x010;

// ----------------------------------------------------------------------
// Connections
// ----------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq)]
enum Kind {
    Login,
    Player,
    Gui,
}

struct Conn {
    stream: TcpStream,
    inbuf: Vec<u8>,
    outbuf: Vec<u8>,
    kind: Kind,
    dead: bool,
    /// Set when the connection must be closed once its output is flushed.
    closing: Option<Instant>,
}

impl Conn {
    fn new(stream: TcpStream) -> Conn {
        Conn {
            stream,
            inbuf: Vec::new(),
            outbuf: Vec::new(),
            kind: Kind::Login,
            dead: false,
            closing: None,
        }
    }

    /// Queues one line. A client that never reads is dropped, not waited for.
    fn send(&mut self, line: &str) {
        if self.outbuf.len() > 1_000_000 {
            self.dead = true;
            return;
        }
        self.outbuf.extend_from_slice(line.as_bytes());
        self.outbuf.push(b'\n');
    }

    fn close_soon(&mut self) {
        self.closing = Some(Instant::now() + Duration::from_secs(1));
    }

    fn flush(&mut self) {
        while !self.outbuf.is_empty() {
            match self.stream.write(&self.outbuf) {
                Ok(0) => {
                    self.dead = true;
                    break;
                }
                Ok(n) => {
                    self.outbuf.drain(..n);
                }
                Err(e) if e.kind() == ErrorKind::WouldBlock => break,
                Err(e) if e.kind() == ErrorKind::Interrupted => continue,
                Err(_) => {
                    self.dead = true;
                    break;
                }
            }
        }
    }

    /// Reads everything available and returns the complete lines.
    fn read_lines(&mut self) -> Vec<String> {
        let mut buf = [0u8; 4096];
        loop {
            match self.stream.read(&mut buf) {
                Ok(0) => {
                    self.dead = true;
                    break;
                }
                Ok(n) => {
                    self.inbuf.extend_from_slice(&buf[..n]);
                    if self.inbuf.len() > 65_536 && !self.inbuf.contains(&b'\n') {
                        self.dead = true;
                        break;
                    }
                    if n < buf.len() {
                        break;
                    }
                }
                Err(e) if e.kind() == ErrorKind::WouldBlock => break,
                Err(e) if e.kind() == ErrorKind::Interrupted => continue,
                Err(_) => {
                    self.dead = true;
                    break;
                }
            }
        }
        let mut lines = Vec::new();
        while let Some(pos) = self.inbuf.iter().position(|&b| b == b'\n') {
            let raw: Vec<u8> = self.inbuf.drain(..=pos).collect();
            let text = String::from_utf8_lossy(&raw[..raw.len() - 1]).to_string();
            lines.push(text.trim_end_matches('\r').to_string());
        }
        lines
    }
}

fn handle_line(
    id: u32,
    line: String,
    conns: &mut BTreeMap<u32, Conn>,
    game: &mut Game,
    now: u64,
) {
    let kind = match conns.get(&id) {
        Some(c) => c.kind,
        None => return,
    };
    match kind {
        Kind::Player => game.enqueue(id, line),
        Kind::Gui => {}
        Kind::Login => {
            let name = line.trim().to_string();
            let c = match conns.get_mut(&id) {
                Some(c) => c,
                None => return,
            };
            if name == "GRAPHIC" {
                c.kind = Kind::Gui;
                return;
            }
            match game.team_index(&name) {
                None => {
                    let msg = format!("Error: the team {} doesn't exist", name);
                    eprintln!("{}", msg);
                    c.send(&msg);
                    c.close_soon();
                }
                Some(t) if game.team_free(t) == 0 => {
                    c.send("ko");
                    c.close_soon();
                }
                Some(t) => {
                    let free = game.add_player(id, t, now);
                    let (w, h) = game.size();
                    c.send(&free.to_string());
                    c.send(&format!("{} {}", w, h));
                    c.kind = Kind::Player;
                }
            }
        }
    }
}

// ----------------------------------------------------------------------
// Arguments
// ----------------------------------------------------------------------

fn value<'a>(args: &'a [String], i: usize, flag: &str) -> Result<&'a str, String> {
    args.get(i)
        .map(|s| s.as_str())
        .ok_or_else(|| format!("missing value for {}", flag))
}

fn parse_args(args: &[String]) -> Result<Config, String> {
    let mut port: Option<u16> = None;
    let mut width: Option<u32> = None;
    let mut height: Option<u32> = None;
    let mut teams: Vec<String> = Vec::new();
    let mut clients: Option<u32> = None;
    let mut time: u32 = 100;
    let mut seed: Option<u64> = None;
    let mut density: f64 = 1.0;
    let mut log = false;

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "-p" => {
                i += 1;
                port = Some(
                    value(args, i, "-p")?
                        .parse::<u16>()
                        .map_err(|_| "invalid port".to_string())?,
                );
            }
            "-x" => {
                i += 1;
                width = Some(
                    value(args, i, "-x")?
                        .parse::<u32>()
                        .map_err(|_| "invalid width".to_string())?,
                );
            }
            "-y" => {
                i += 1;
                height = Some(
                    value(args, i, "-y")?
                        .parse::<u32>()
                        .map_err(|_| "invalid height".to_string())?,
                );
            }
            "-n" => {
                i += 1;
                while i < args.len() && !args[i].starts_with('-') {
                    teams.push(args[i].clone());
                    i += 1;
                }
                i -= 1; // the common `i += 1` below moves to the next flag
            }
            "-c" => {
                i += 1;
                clients = Some(
                    value(args, i, "-c")?
                        .parse::<u32>()
                        .map_err(|_| "invalid client count".to_string())?,
                );
            }
            "-t" => {
                i += 1;
                time = value(args, i, "-t")?
                    .parse::<u32>()
                    .map_err(|_| "invalid time unit".to_string())?;
            }
            "-s" => {
                i += 1;
                seed = Some(
                    value(args, i, "-s")?
                        .parse::<u64>()
                        .map_err(|_| "invalid seed".to_string())?,
                );
            }
            "-d" => {
                i += 1;
                density = value(args, i, "-d")?
                    .parse::<f64>()
                    .map_err(|_| "invalid density".to_string())?;
            }
            "-l" => log = true,
            other => return Err(format!("unknown argument '{}'", other)),
        }
        i += 1;
    }

    let port = port.ok_or("missing -p")?;
    let width = width.ok_or("missing -x")?;
    let height = height.ok_or("missing -y")?;
    let clients = clients.ok_or("missing -c")?;
    if width == 0 || height == 0 || width > 1000 || height > 1000 {
        return Err("width and height must be between 1 and 1000".to_string());
    }
    if teams.is_empty() {
        return Err("at least one team is required (-n)".to_string());
    }
    for (a, name) in teams.iter().enumerate() {
        if name == "GRAPHIC" {
            return Err("the team name GRAPHIC is reserved".to_string());
        }
        if teams[..a].contains(name) {
            return Err(format!("duplicate team name '{}'", name));
        }
    }
    if clients == 0 {
        return Err("-c must be at least 1".to_string());
    }
    if time == 0 {
        return Err("-t must be at least 1".to_string());
    }
    if !(0.1..=3.0).contains(&density) {
        return Err("-d must be between 0.1 and 3.0".to_string());
    }
    let seed = seed.unwrap_or_else(|| {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(42)
    });
    Ok(Config {
        width,
        height,
        teams,
        clients,
        time,
        seed,
        density,
        log,
    })
}

// ----------------------------------------------------------------------
// Main loop
// ----------------------------------------------------------------------

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let cfg = match parse_args(&args) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("Error: {}", e);
            eprintln!("{}", USAGE);
            std::process::exit(1);
        }
    };
    let port = args
        .iter()
        .position(|a| a == "-p")
        .and_then(|i| args.get(i + 1))
        .and_then(|p| p.parse::<u16>().ok())
        .unwrap_or(0);

    let listener = match TcpListener::bind(("0.0.0.0", port)) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("Error: cannot listen on port {}: {}", port, e);
            std::process::exit(1);
        }
    };
    listener.set_nonblocking(true).expect("set_nonblocking");

    println!(
        "Zappy server on port {} | world {}x{} | teams {:?} | {} slots/team | t={} | seed={}",
        port, cfg.width, cfg.height, cfg.teams, cfg.clients, cfg.time, cfg.seed
    );

    let start = Instant::now();
    let mut game = Game::new(&cfg);
    let mut conns: BTreeMap<u32, Conn> = BTreeMap::new();
    let mut next_id: u32 = 1;
    let mut last_frame = Instant::now();

    loop {
        // ---- wait for socket events (or at most 1 ms) ----
        let mut fds = vec![PollFd {
            fd: listener.as_raw_fd(),
            events: POLLIN,
            revents: 0,
        }];
        let mut ids: Vec<u32> = Vec::new();
        for (id, c) in conns.iter() {
            let mut ev = POLLIN;
            if !c.outbuf.is_empty() {
                ev |= POLLOUT;
            }
            fds.push(PollFd {
                fd: c.stream.as_raw_fd(),
                events: ev,
                revents: 0,
            });
            ids.push(*id);
        }
        unsafe {
            poll(fds.as_mut_ptr(), fds.len() as c_ulong, 1);
        }
        let accept_ready = fds[0].revents & POLLIN != 0;
        let readable: Vec<u32> = ids
            .iter()
            .zip(fds[1..].iter())
            .filter(|(_, f)| f.revents & (POLLIN | POLLHUP | POLLERR) != 0)
            .map(|(id, _)| *id)
            .collect();

        let now = (start.elapsed().as_secs_f64() * cfg.time as f64) as u64;

        // ---- new connections ----
        if accept_ready {
            loop {
                match listener.accept() {
                    Ok((stream, _)) => {
                        if stream.set_nonblocking(true).is_err() {
                            continue;
                        }
                        let _ = stream.set_nodelay(true);
                        let mut c = Conn::new(stream);
                        c.send("WELCOME");
                        conns.insert(next_id, c);
                        next_id += 1;
                    }
                    Err(e) if e.kind() == ErrorKind::WouldBlock => break,
                    Err(e) if e.kind() == ErrorKind::Interrupted => continue,
                    Err(e) => {
                        eprintln!("accept error: {}", e);
                        break;
                    }
                }
            }
        }

        // ---- incoming data ----
        let mut pending: Vec<(u32, String)> = Vec::new();
        for id in readable {
            if let Some(c) = conns.get_mut(&id) {
                for line in c.read_lines() {
                    pending.push((id, line));
                }
            }
        }
        for (id, line) in pending {
            handle_line(id, line, &mut conns, &mut game, now);
        }

        // ---- game logic ----
        game.update(now);
        for (id, lines) in game.take_outputs() {
            if let Some(c) = conns.get_mut(&id) {
                for l in lines {
                    c.send(&l);
                }
            }
        }
        for id in game.reap_dead() {
            if let Some(c) = conns.get_mut(&id) {
                c.close_soon();
            }
        }
        for l in game.take_log() {
            println!("[{:>7}] {}", now, l);
        }

        // ---- frames for the graphical client (10 per second) ----
        if last_frame.elapsed() >= Duration::from_millis(100) {
            last_frame = Instant::now();
            if conns.values().any(|c| c.kind == Kind::Gui) {
                let snap = game.snapshot();
                let snap = snap.trim_end();
                for c in conns.values_mut() {
                    if c.kind == Kind::Gui && c.outbuf.len() < 500_000 {
                        c.send(snap);
                    }
                }
            } else {
                game.clear_sounds();
            }
        }

        // ---- game over ----
        if let Some(team) = game.winner() {
            println!("Game over: team {} wins!", team);
            for c in conns.values_mut() {
                if c.kind != Kind::Login {
                    c.send(&format!("win {}", team));
                }
            }
            for _ in 0..20 {
                for c in conns.values_mut() {
                    c.flush();
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            return;
        }

        // ---- output & cleanup ----
        for c in conns.values_mut() {
            c.flush();
        }
        let gone: Vec<u32> = conns
            .iter()
            .filter(|(_, c)| {
                c.dead
                    || match c.closing {
                        Some(deadline) => c.outbuf.is_empty() || Instant::now() >= deadline,
                        None => false,
                    }
            })
            .map(|(id, _)| *id)
            .collect();
        for id in gone {
            if let Some(c) = conns.remove(&id) {
                if c.kind == Kind::Player {
                    game.disconnect(id);
                }
            }
        }
    }
}
