use crate::{fail, Result};
use serde::{Deserialize, Serialize};
use std::io::{Read, Write};
pub const MAX_PACKET: usize = 8 * 1024 * 1024;
pub const HELLO: u8 = 1;
pub const VIDEO: u8 = 2;
pub const INPUT: u8 = 3;
pub const CLIPBOARD: u8 = 4;
pub const FILE: u8 = 5;
pub const PING: u8 = 6;
pub const CLOSE: u8 = 7;
pub const CONFIG: u8 = 8;
pub const AUDIO: u8 = 9;
#[derive(Debug)]
pub struct Packet {
    pub kind: u8,
    pub payload: Vec<u8>,
}
impl Packet {
    pub fn json<T: Serialize>(kind: u8, value: &T) -> Result<Self> {
        let p = Self {
            kind,
            payload: serde_json::to_vec(value)?,
        };
        p.validate()?;
        Ok(p)
    }
    pub fn parse<'a, T: Deserialize<'a>>(&'a self) -> Result<T> {
        Ok(serde_json::from_slice(&self.payload)?)
    }
    fn validate(&self) -> Result<()> {
        if !(HELLO..=AUDIO).contains(&self.kind) || self.payload.len() > MAX_PACKET {
            return fail("invalid packet type/length");
        }
        if !matches!(self.kind, VIDEO | FILE | CLIPBOARD) && self.payload.len() > 65536 {
            return fail("control packet too large");
        }
        Ok(())
    }
    pub fn write(&self, writer: &mut impl Write) -> Result<()> {
        self.validate()?;
        writer.write_all(&[1, self.kind])?;
        writer.write_all(&(self.payload.len() as u32).to_be_bytes())?;
        writer.write_all(&self.payload)?;
        writer.flush()?;
        Ok(())
    }
    pub fn read(reader: &mut impl Read) -> Result<Self> {
        let mut head = [0; 6];
        reader.read_exact(&mut head)?;
        if head[0] != 1 || !(HELLO..=AUDIO).contains(&head[1]) {
            return fail("incompatible protocol");
        }
        let n = u32::from_be_bytes(head[2..6].try_into()?) as usize;
        if n > MAX_PACKET || (!matches!(head[1], VIDEO | FILE | CLIPBOARD) && n > 65536) {
            return fail("packet length limit");
        }
        let mut payload = vec![0; n];
        reader.read_exact(&mut payload)?;
        Ok(Self {
            kind: head[1],
            payload,
        })
    }
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Hello {
    pub major: u32,
    pub role: String,
    pub user_session: u32,
    pub nonce: u64,
    #[serde(default)]
    pub resume_token: Option<String>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResumeAck {
    pub resumed: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<HandshakeError>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HandshakeError {
    SessionBusy,
    ResumeExpired,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VideoConfig {
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    pub monitor: u32,
    pub input_epoch: u64,
    pub session_id: u64,
    pub resume_token: String,
    #[serde(default)]
    pub monitors: Vec<Monitor>,
}
impl VideoConfig {
    pub fn validate(&self) -> Result<()> {
        if self.width == 0
            || self.height == 0
            || self.width > 8192
            || self.height > 8192
            || self.fps == 0
            || self.fps > 120
            || self.input_epoch == 0
            || self.session_id == 0
            || self.monitors.len() > 16
            || self.monitors.iter().any(|m| {
                m.name.len() > 128
                    || m.modes.len() > 128
                    || m.width > 8192
                    || m.height > 8192
                    || m.modes.iter().any(|v| !v.valid())
            })
            || self.resume_token.len() != 64
            || !self.resume_token.bytes().all(|b| b.is_ascii_hexdigit())
        {
            return fail("invalid video configuration");
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DisplayMode {
    pub width: u32,
    pub height: u32,
    pub hz: u32,
}
impl DisplayMode {
    pub fn valid(&self) -> bool {
        (640..=8192).contains(&self.width)
            && (480..=8192).contains(&self.height)
            && self.width.is_multiple_of(2)
            && self.height.is_multiple_of(2)
            && (1..=360).contains(&self.hz)
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Monitor {
    pub id: u32,
    pub name: String,
    pub width: u32,
    pub height: u32,
    pub modes: Vec<DisplayMode>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "event", deny_unknown_fields)]
pub enum Input {
    Key { code: u16, down: bool },
    Text { text: String },
    Mouse { x: f64, y: f64 },
    Relative { dx: i32, dy: i32 },
    Button { button: u8, down: bool },
    Wheel { delta: i32 },
    Ime { enabled: bool },
    Release,
    KeepAlive,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InputPacket {
    pub epoch: u64,
    pub sequence: u64,
    pub input: Input,
}
impl InputPacket {
    pub fn validate(&self) -> Result<()> {
        if self.epoch == 0 || self.sequence == 0 {
            return fail("invalid input epoch/sequence");
        }
        match &self.input {
            Input::Key { code, .. } if *code > 0xff => fail("invalid virtual key"),
            Input::Text { text } if text.len() > 4096 || text.contains('\0') => {
                fail("text size limit")
            }
            Input::Mouse { x, y }
                if !x.is_finite()
                    || !y.is_finite()
                    || !(0.0..=1.0).contains(x)
                    || !(0.0..=1.0).contains(y) =>
            {
                fail("invalid mouse position")
            }
            Input::Button { button, .. } if *button > 4 => fail("invalid mouse button"),
            Input::Relative { dx, dy }
                if dx.unsigned_abs() > 32767 || dy.unsigned_abs() > 32767 =>
            {
                fail("relative input limit")
            }
            Input::Wheel { delta } if delta.unsigned_abs() > 12000 => fail("wheel input limit"),
            _ => Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn successful_resume_ack_stays_compatible_and_errors_are_typed() {
        let ack = ResumeAck {
            resumed: true,
            error: None,
        };
        assert_eq!(serde_json::to_string(&ack).unwrap(), r#"{"resumed":true}"#);
        assert!(
            serde_json::from_str::<ResumeAck>(r#"{"resumed":true}"#)
                .unwrap()
                .resumed
        );
        let rejection = Packet::json(
            HELLO,
            &ResumeAck {
                resumed: false,
                error: Some(HandshakeError::SessionBusy),
            },
        )
        .unwrap();
        assert_eq!(
            rejection.parse::<ResumeAck>().unwrap().error,
            Some(HandshakeError::SessionBusy)
        );
        assert!(serde_json::from_str::<ResumeAck>(
            r#"{"resumed":false,"error":"untrusted free text"}"#
        )
        .is_err());
    }
    #[test]
    fn rejects_huge_prefix_before_allocation() {
        let mut bytes = &[1, VIDEO, 255, 255, 255, 255][..];
        assert!(Packet::read(&mut bytes).is_err());
    }
    #[test]
    fn boundary_and_two_packets() {
        let mut b = Vec::new();
        for _ in 0..2 {
            Packet::json(PING, &42).unwrap().write(&mut b).unwrap();
        }
        let mut b = &b[..];
        for _ in 0..2 {
            assert_eq!(Packet::read(&mut b).unwrap().parse::<u64>().unwrap(), 42);
        }
        assert!(b.is_empty());
    }
    #[test]
    fn malformed_input_is_rejected() {
        assert!(InputPacket {
            epoch: 1,
            sequence: 1,
            input: Input::Mouse {
                x: f64::NAN,
                y: 0.0
            }
        }
        .validate()
        .is_err());
        assert!(InputPacket {
            epoch: 0,
            sequence: 1,
            input: Input::Release
        }
        .validate()
        .is_err());
    }
}
