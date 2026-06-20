use anyhow::{Context, Result};
use rosc::{encoder, OscMessage, OscPacket, OscType};
use std::net::UdpSocket;

/// Sends text to the VRChat chatbox over OSC.
pub struct Vrc {
    socket: UdpSocket,
    target: String,
}

impl Vrc {
    pub fn new(target: &str) -> Result<Vrc> {
        // Bind to an ephemeral local port; we only send.
        let socket = UdpSocket::bind("0.0.0.0:0").context("binding OSC udp socket")?;
        Ok(Vrc {
            socket,
            target: target.to_string(),
        })
    }

    fn send(&self, msg: OscMessage) -> Result<()> {
        let buf = encoder::encode(&OscPacket::Message(msg)).context("encoding OSC packet")?;
        self.socket
            .send_to(&buf, &self.target)
            .with_context(|| format!("sending OSC to {}", self.target))?;
        Ok(())
    }

    /// Put `text` in the chatbox immediately (no keyboard, no notification sound).
    /// VRChat hard-limits the chatbox to 144 characters.
    pub fn chatbox(&self, text: &str) -> Result<()> {
        let text = truncate_chars(text, 144);
        self.send(OscMessage {
            addr: "/chatbox/input".to_string(),
            args: vec![
                OscType::String(text),
                OscType::Bool(true),  // send immediately
                OscType::Bool(false), // no notification SFX
            ],
        })
    }

    /// Show/hide the "typing…" bubble while an utterance is being processed.
    pub fn typing(&self, on: bool) -> Result<()> {
        self.send(OscMessage {
            addr: "/chatbox/typing".to_string(),
            args: vec![OscType::Bool(on)],
        })
    }
}

/// Truncate to `max` characters (not bytes) so multibyte JA/Thai text stays valid.
fn truncate_chars(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        s.chars().take(max).collect()
    }
}
