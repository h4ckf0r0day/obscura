#[cfg(any(feature = "headed", test))]
use std::io::{BufRead, Read};

#[cfg(any(feature = "headed", test))]
use anyhow::{ensure, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[cfg(any(feature = "headed", test))]
pub(super) const MAX_HEADER: usize = 16 * 1024;
#[cfg(any(feature = "headed", test))]
pub(super) const MAX_PIXELS: usize = 16 * 1024 * 1024;

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum Packet {
    Ready,
    Frame { width: u32, height: u32 },
    Input { method: String, params: Value },
}

#[cfg(any(feature = "headed", test))]
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct FrameHeader {
    pub width: u32,
    pub height: u32,
    pub title: String,
    pub error: Option<String>,
}

#[cfg(any(feature = "headed", test))]
impl FrameHeader {
    pub fn byte_len(&self) -> Result<usize> {
        let pixels = (self.width as usize).checked_mul(self.height as usize);
        ensure!(self.width <= 8192 && self.height <= 8192, "native frame dimensions exceed the limit");
        ensure!(pixels.is_some_and(|pixels| pixels <= MAX_PIXELS), "native frame exceeds the pixel limit");
        ensure!((self.width == 0) == (self.height == 0), "incomplete native frame size");
        Ok(pixels.unwrap() * 4)
    }
}

#[cfg(any(feature = "headed", test))]
pub(super) struct Frame {
    pub header: FrameHeader,
    pub pixels: Vec<u8>,
}

#[cfg(any(feature = "headed", test))]
impl Frame {
    pub fn read(reader: &mut impl BufRead) -> Result<Option<Self>> {
        let mut header = String::new();
        if Read::by_ref(reader).take((MAX_HEADER + 1) as u64).read_line(&mut header)? == 0 { return Ok(None); }
        ensure!(header.len() <= MAX_HEADER && header.ends_with('\n'), "invalid native frame header");
        let header: FrameHeader = serde_json::from_str(&header)?;
        let mut pixels = vec![0; header.byte_len()?];
        reader.read_exact(&mut pixels)?;
        Ok(Some(Self { header, pixels }))
    }
}

#[cfg(any(feature = "headed", test))]
pub(super) fn opaque_pixel(pixel: &[u8]) -> u32 {
    let alpha = u32::from(pixel[3]);
    let channel = |value: u8| (u32::from(value) * alpha + 255 * (255 - alpha) + 127) / 255;
    channel(pixel[0]) << 16 | channel(pixel[1]) << 8 | channel(pixel[2])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_frames_round_trip_binary_newlines_and_following_headers() {
        let mut header = FrameHeader { width: 1, height: 1, title: "fixture".into(), error: None };
        let mut bytes = format!("{}\n", serde_json::to_string(&header).unwrap()).into_bytes();
        bytes.extend_from_slice(&[10, 0, 255, 255]);
        header.width = 0;
        header.height = 0;
        bytes.extend_from_slice(format!("{}\n", serde_json::to_string(&header).unwrap()).as_bytes());
        let mut reader = std::io::Cursor::new(bytes);
        let frame = Frame::read(&mut reader).unwrap().unwrap();
        assert_eq!(frame.header.title, "fixture");
        assert_eq!(frame.pixels, [10, 0, 255, 255]);
        assert!(Frame::read(&mut reader).unwrap().unwrap().pixels.is_empty());
        assert!(Frame::read(&mut reader).unwrap().is_none());
    }

    #[test]
    fn native_protocol_bounds_frames_and_rejects_partial_payloads() {
        let mut header = FrameHeader { width: 8192, height: 8192, title: String::new(), error: None };
        assert!(header.byte_len().is_err());
        header.width = 1;
        header.height = 0;
        assert!(header.byte_len().is_err());
        header.height = 1;
        let encoded = format!("{}\n", serde_json::to_string(&header).unwrap());
        assert!(Frame::read(&mut std::io::Cursor::new(encoded)).is_err());
        assert!(Frame::read(&mut std::io::Cursor::new(vec![b'x'; MAX_HEADER + 1])).is_err());
    }

    #[test]
    fn native_pixels_composite_transparency_over_the_window_background() {
        assert_eq!(opaque_pixel(&[255, 0, 0, 255]), 0xff0000);
        assert_eq!(opaque_pixel(&[0, 0, 0, 0]), 0xffffff);
        assert_eq!(opaque_pixel(&[0, 0, 0, 128]), 0x7f7f7f);
    }
}
