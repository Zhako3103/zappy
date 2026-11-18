//! Orientation, wrap-around maths and the broadcast "sound direction".

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Dir {
    N,
    E,
    S,
    W,
}

impl Dir {
    pub fn right(self) -> Dir {
        match self {
            Dir::N => Dir::E,
            Dir::E => Dir::S,
            Dir::S => Dir::W,
            Dir::W => Dir::N,
        }
    }

    pub fn left(self) -> Dir {
        match self {
            Dir::N => Dir::W,
            Dir::W => Dir::S,
            Dir::S => Dir::E,
            Dir::E => Dir::N,
        }
    }

    /// Unit vector on the map. Y grows downwards, so north is (0, -1).
    pub fn vec(self) -> (i32, i32) {
        match self {
            Dir::N => (0, -1),
            Dir::E => (1, 0),
            Dir::S => (0, 1),
            Dir::W => (-1, 0),
        }
    }

    pub fn index(self) -> u8 {
        match self {
            Dir::N => 0,
            Dir::E => 1,
            Dir::S => 2,
            Dir::W => 3,
        }
    }

    pub fn from_index(i: usize) -> Dir {
        match i % 4 {
            0 => Dir::N,
            1 => Dir::E,
            2 => Dir::S,
            _ => Dir::W,
        }
    }

    /// Mathematical angle (degrees, counter-clockwise from east) of the facing.
    fn angle(self) -> f64 {
        match self {
            Dir::E => 0.0,
            Dir::N => 90.0,
            Dir::W => 180.0,
            Dir::S => 270.0,
        }
    }
}

pub fn wrap(v: i32, n: i32) -> i32 {
    v.rem_euclid(n)
}

/// Shortest signed distance from `from` to `to` on a ring of size `size`.
pub fn wrap_delta(from: i32, to: i32, size: i32) -> i32 {
    let mut d = (to - from).rem_euclid(size);
    if d > size / 2 {
        d -= size;
    }
    d
}

/// Number 0..=8 of the square around a player (1 = in front, then
/// counter-clockwise) that the vector (dx, dy) points to. `dy` is a map
/// vector (Y down). 0 means "same square".
pub fn direction_number(dx: i32, dy: i32, facing: Dir) -> u8 {
    if dx == 0 && dy == 0 {
        return 0;
    }
    let angle = ((-dy) as f64).atan2(dx as f64).to_degrees();
    let mut rel = (angle - facing.angle()) % 360.0;
    if rel < 0.0 {
        rel += 360.0;
    }
    let sector = (rel / 45.0).round() as i32 % 8;
    (sector + 1) as u8
}

/// Direction from which the receiver hears a broadcast sent from (sx, sy),
/// following the shortest path on the spherical (wrapping) map.
pub fn sound_direction(rx: i32, ry: i32, rdir: Dir, sx: i32, sy: i32, w: i32, h: i32) -> u8 {
    let dx = wrap_delta(rx, sx, w);
    let dy = wrap_delta(ry, sy, h);
    direction_number(dx, dy, rdir)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spec_example() {
        // Map 10x10. A = (5, 8) looks north, B = (2, 2) looks east.
        assert_eq!(sound_direction(2, 2, Dir::E, 5, 8, 10, 10), 2); // B hears via 2
        assert_eq!(sound_direction(5, 8, Dir::N, 2, 2, 10, 10), 4); // A sends via 4
    }

    #[test]
    fn same_square() {
        assert_eq!(sound_direction(3, 3, Dir::S, 3, 3, 10, 10), 0);
    }

    #[test]
    fn straight_lines() {
        assert_eq!(sound_direction(5, 5, Dir::N, 5, 2, 10, 10), 1); // ahead
        assert_eq!(sound_direction(5, 5, Dir::N, 2, 5, 10, 10), 3); // left
        assert_eq!(sound_direction(5, 5, Dir::N, 5, 8, 10, 10), 5); // behind
        assert_eq!(sound_direction(5, 5, Dir::N, 8, 5, 10, 10), 7); // right
    }

    #[test]
    fn wrapping_delta() {
        assert_eq!(wrap_delta(0, 9, 10), -1);
        assert_eq!(wrap_delta(9, 0, 10), 1);
        assert_eq!(wrap_delta(2, 8, 10), -4);
    }
}
