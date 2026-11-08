//! KingSmith WalkingPad "WiLink" BLE protocol (service FE00).
//! See ../../PROTOCOL.md for the verified reference this implements.

use uuid::{uuid, Uuid};

pub const SERVICE: Uuid = uuid!("0000fe00-0000-1000-8000-00805f9b34fb");
pub const NOTIFY_CHAR: Uuid = uuid!("0000fe01-0000-1000-8000-00805f9b34fb");
pub const WRITE_CHAR: Uuid = uuid!("0000fe02-0000-1000-8000-00805f9b34fb");

pub const MIN_SPEED: u8 = 5; // 0.5 km/h, in tenths
pub const DEFAULT_MAX_SPEED: u8 = 60; // 6.0 km/h

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    Auto,
    Manual,
    Standby,
}

impl Mode {
    pub fn code(self) -> u8 {
        match self {
            Mode::Auto => 0,
            Mode::Manual => 1,
            Mode::Standby => 2,
        }
    }
    pub fn from_code(c: u8) -> Option<Mode> {
        match c {
            0 => Some(Mode::Auto),
            1 => Some(Mode::Manual),
            2 => Some(Mode::Standby),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Belt {
    Idle,
    Running,
    /// Start countdown; value is the number shown (3, 2, 1).
    Countdown(u8),
    Standby,
    Unknown(u8),
}

impl Belt {
    pub fn from_code(c: u8) -> Belt {
        match c {
            0 => Belt::Idle,
            1 => Belt::Running,
            5 => Belt::Standby,
            7..=9 => Belt::Countdown(c - 6),
            other => Belt::Unknown(other),
        }
    }
    pub fn is_moving_or_starting(self) -> bool {
        matches!(self, Belt::Running | Belt::Countdown(_))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Command {
    AskStatus,
    /// Target speed in tenths of km/h.
    SetSpeed(u8),
    SetMode(Mode),
    /// Starts an idle belt (with 3-2-1 countdown) or stops a running one.
    StartStopToggle,
    AskPrefs,
    AskLastRecord,
    /// Reply starts with an ASCII model code, e.g. "MA1". Kept for model detection.
    #[allow(dead_code)]
    AskDeviceInfo,
}

fn frame(body: &[u8]) -> Vec<u8> {
    let crc = body.iter().fold(0u8, |a, b| a.wrapping_add(*b));
    let mut v = Vec::with_capacity(body.len() + 3);
    v.push(0xF7);
    v.extend_from_slice(body);
    v.push(crc);
    v.push(0xFD);
    v
}

pub fn encode(cmd: Command) -> Vec<u8> {
    match cmd {
        Command::AskStatus => frame(&[0xA2, 0x00, 0x00]),
        Command::SetSpeed(t) => frame(&[0xA2, 0x01, t]),
        Command::SetMode(m) => frame(&[0xA2, 0x02, m.code()]),
        Command::StartStopToggle => frame(&[0xA2, 0x04, 0x01]),
        Command::AskPrefs => frame(&[0xA6, 0x00, 0x00, 0x00, 0x00, 0x00]),
        Command::AskLastRecord => frame(&[0xA7, 0xAA, 0xFF]),
        Command::AskDeviceInfo => frame(&[0xA5, 0x61, 0x01, 0x6A, 0x23, 0x1D, 0xBE]),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub struct Status {
    pub belt: Belt,
    pub speed: u8,
    pub mode: Option<Mode>,
    pub time_s: u32,
    /// Units of 10 m.
    pub dist_10m: u32,
    pub steps: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Prefs {
    pub max_speed: u8,
    pub start_speed: u8,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Record {
    pub time_s: u32,
    pub dist_10m: u32,
    pub steps: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Message {
    Status(Status),
    Prefs(Prefs),
    Record(Record),
    Other(Vec<u8>),
}

fn be24(b: &[u8]) -> u32 {
    (b[0] as u32) << 16 | (b[1] as u32) << 8 | b[2] as u32
}

/// Decode a notification. Returns None for frames with bad framing or checksum.
pub fn decode(d: &[u8]) -> Option<Message> {
    if d.len() < 4 || d[0] != 0xF8 || d[d.len() - 1] != 0xFD {
        return None;
    }
    let crc = d[1..d.len() - 2].iter().fold(0u8, |a, b| a.wrapping_add(*b));
    if crc != d[d.len() - 2] {
        return None;
    }
    Some(match d[1] {
        0xA2 if d.len() >= 16 => Message::Status(Status {
            belt: Belt::from_code(d[2]),
            speed: d[3],
            mode: Mode::from_code(d[4]),
            time_s: be24(&d[5..8]),
            dist_10m: be24(&d[8..11]),
            steps: be24(&d[11..14]),
        }),
        0xA6 if d.len() >= 12 => Message::Prefs(Prefs { max_speed: d[7], start_speed: d[8] }),
        0xA7 if d.len() >= 19 => Message::Record(Record {
            time_s: be24(&d[8..11]),
            dist_10m: be24(&d[11..14]),
            steps: be24(&d[14..17]),
        }),
        _ => Message::Other(d.to_vec()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(s: &str) -> Vec<u8> {
        s.split_whitespace().map(|h| u8::from_str_radix(h, 16).unwrap()).collect()
    }

    #[test]
    fn encodes_frames_seen_on_the_wire() {
        assert_eq!(encode(Command::AskStatus), hex("f7 a2 00 00 a2 fd"));
        assert_eq!(encode(Command::SetSpeed(10)), hex("f7 a2 01 0a ad fd"));
        assert_eq!(encode(Command::SetMode(Mode::Manual)), hex("f7 a2 02 01 a5 fd"));
        assert_eq!(encode(Command::SetMode(Mode::Standby)), hex("f7 a2 02 02 a6 fd"));
        assert_eq!(encode(Command::StartStopToggle), hex("f7 a2 04 01 a7 fd"));
        assert_eq!(encode(Command::AskPrefs), hex("f7 a6 00 00 00 00 00 a6 fd"));
        assert_eq!(encode(Command::AskLastRecord), hex("f7 a7 aa ff 50 fd"));
        assert_eq!(encode(Command::AskDeviceInfo), hex("f7 a5 61 01 6a 23 1d be 6f fd"));
    }

    #[test]
    fn decodes_running_status() {
        let m = decode(&hex("f8 a2 01 14 01 00 00 06 00 00 00 00 00 02 1e 00 00 00 de fd")).unwrap();
        assert_eq!(
            m,
            Message::Status(Status {
                belt: Belt::Running,
                speed: 20,
                mode: Some(Mode::Manual),
                time_s: 6,
                dist_10m: 0,
                steps: 2
            })
        );
    }

    #[test]
    fn decodes_countdown_and_standby() {
        let Some(Message::Status(s)) = decode(&hex("f8 a2 09 00 01 00 00 00 00 00 00 00 00 00 01 00 03 00 b0 fd")) else { panic!() };
        assert_eq!(s.belt, Belt::Countdown(3));
        let Some(Message::Status(s)) = decode(&hex("f8 a2 05 00 02 00 00 00 00 00 00 00 00 00 02 00 00 00 ab fd")) else { panic!() };
        assert_eq!(s.belt, Belt::Standby);
        assert_eq!(s.mode, Some(Mode::Standby));
    }

    #[test]
    fn decodes_prefs_and_record() {
        assert_eq!(
            decode(&hex("f8 a6 00 00 00 00 00 3c 14 00 02 17 00 00 00 00 00 00 0f fd")),
            Some(Message::Prefs(Prefs { max_speed: 60, start_speed: 20 }))
        );
        assert_eq!(
            decode(&hex("f8 a7 00 00 39 00 00 2f 00 00 18 00 00 00 00 00 0b 00 32 fd")),
            Some(Message::Record(Record { time_s: 24, dist_10m: 0, steps: 11 }))
        );
    }

    #[test]
    fn rejects_bad_checksum() {
        assert_eq!(decode(&hex("f8 a2 05 00 02 00 00 00 00 00 00 00 00 00 02 00 00 00 ac fd")), None);
    }
}
