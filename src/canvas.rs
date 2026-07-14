//! Minimal decoder for the `CANVAZ` extended-metadata payload (Spotify
//! Canvas — the short looping video/GIF shown behind now-playing).
//!
//! librespot-protocol ships `canvaz.proto` but its generated
//! `EntityCanvazResponse` is **empty** — the proto only declares the
//! nested `Canvaz` message, not the `repeated Canvaz` field the wire
//! actually carries. Rather than fork the proto, we hand-decode the two
//! fields we need off the wire (same approach as `extracted_color.rs`).
//!
//! Wire schema (stable since Spotify ~1.2.x):
//! ```proto
//! message EntityCanvazResponse { repeated Canvaz canvas = 1; }
//! message Canvaz {
//!   string id = 1; string url = 2; string file_id = 3;
//!   Type type = 4;  // 0 IMAGE, 1 VIDEO, 2 VIDEO_LOOPING,
//!                   // 3 VIDEO_LOOPING_RANDOM, 4 GIF
//!   string entity_uri = 5; ...
//! }
//! ```

/// Canvas media kind (mirrors Spotify's `Type` enum). We only branch on
/// "is this a video we can decode" vs not.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CanvasType {
    Image,
    Video,
    VideoLooping,
    VideoLoopingRandom,
    Gif,
    Unknown(u64),
}

impl CanvasType {
    fn from_raw(v: u64) -> Self {
        match v {
            0 => CanvasType::Image,
            1 => CanvasType::Video,
            2 => CanvasType::VideoLooping,
            3 => CanvasType::VideoLoopingRandom,
            4 => CanvasType::Gif,
            other => CanvasType::Unknown(other),
        }
    }
    /// True for the H.264/MP4 video kinds we can decode + loop.
    pub fn is_video(self) -> bool {
        matches!(
            self,
            CanvasType::Video | CanvasType::VideoLooping | CanvasType::VideoLoopingRandom
        )
    }
}

/// One decoded Canvas entry: its media URL + kind.
#[derive(Debug, Clone)]
pub struct CanvasEntry {
    pub url: String,
    pub kind: CanvasType,
}

/// Decode a `Canvaz` entry from the extended-metadata payload. `None` if
/// there's no canvas for the track or the bytes don't parse — the caller
/// falls back to the album art.
///
/// The extended-metadata `CANVAZ` extension returns the **inner `Canvaz`
/// message directly** (its `type_url` ends in `…EntityCanvazResponse.Canvaz`),
/// so we decode `Canvaz` fields off the top level. For robustness we also
/// accept a full `EntityCanvazResponse` (`repeated Canvaz canvas = 1`) and
/// unwrap its first entry — covers the dedicated canvaz-cache endpoint.
pub fn parse_canvas(bytes: &[u8]) -> Option<CanvasEntry> {
    parse_canvaz(bytes).or_else(|| {
        // Fallback: bytes are an EntityCanvazResponse — unwrap canvas = 1.
        let inner = field_submessage(bytes, 1)?;
        parse_canvaz(&inner)
    })
}

/// Decode a single `Canvaz` message: `url = field 2` (string), `type =
/// field 4` (enum/varint). `None` if there's no non-empty url.
fn parse_canvaz(canvaz: &[u8]) -> Option<CanvasEntry> {
    let url_bytes = field_submessage(canvaz, 2)?;
    let url = String::from_utf8(url_bytes).ok()?;
    if url.is_empty() || !url.starts_with("http") {
        return None;
    }
    let kind = field_varint(canvaz, 4)
        .map(CanvasType::from_raw)
        .unwrap_or(CanvasType::Unknown(0));
    Some(CanvasEntry { url, kind })
}

/// Read a base-128 varint at `*pos`, advancing it.
fn read_varint(buf: &[u8], pos: &mut usize) -> Option<u64> {
    let mut result: u64 = 0;
    let mut shift = 0u32;
    while *pos < buf.len() {
        let byte = buf[*pos];
        *pos += 1;
        result |= ((byte & 0x7F) as u64) << shift;
        if byte & 0x80 == 0 {
            return Some(result);
        }
        shift += 7;
        if shift >= 64 {
            return None;
        }
    }
    None
}

/// Skip a field's value, advancing `pos`. `None` past-buffer or on groups.
fn skip_value(buf: &[u8], pos: &mut usize, wire: u64) -> Option<()> {
    match wire {
        0 => {
            read_varint(buf, pos)?;
        }
        1 => *pos = pos.checked_add(8)?,
        5 => *pos = pos.checked_add(4)?,
        2 => {
            let len = read_varint(buf, pos)? as usize;
            *pos = pos.checked_add(len)?;
        }
        _ => return None,
    }
    if *pos > buf.len() { None } else { Some(()) }
}

/// Bytes of the first length-delimited (wire type 2) field matching `field`.
fn field_submessage(buf: &[u8], field: u64) -> Option<Vec<u8>> {
    let mut pos = 0;
    while pos < buf.len() {
        let tag = read_varint(buf, &mut pos)?;
        let (fnum, wire) = (tag >> 3, tag & 0x7);
        if fnum == field && wire == 2 {
            let len = read_varint(buf, &mut pos)? as usize;
            let end = pos.checked_add(len)?;
            return buf.get(pos..end).map(|s| s.to_vec());
        }
        skip_value(buf, &mut pos, wire)?;
    }
    None
}

/// First varint (wire type 0) field matching `field`.
fn field_varint(buf: &[u8], field: u64) -> Option<u64> {
    let mut pos = 0;
    while pos < buf.len() {
        let tag = read_varint(buf, &mut pos)?;
        let (fnum, wire) = (tag >> 3, tag & 0x7);
        if fnum == field && wire == 0 {
            return read_varint(buf, &mut pos);
        }
        skip_value(buf, &mut pos, wire)?;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn len_delim(field: u8, payload: &[u8]) -> Vec<u8> {
        let mut out = vec![(field << 3) | 2, payload.len() as u8];
        out.extend_from_slice(payload);
        out
    }

    /// Encode `EntityCanvazResponse { canvas: Canvaz { url, type } }`.
    fn encode(url: &str, ty: u8) -> Vec<u8> {
        let mut canvaz = len_delim(2, url.as_bytes()); // url = field 2
        canvaz.push(4 << 3); // type = field 4, wire type 0 (varint)
        canvaz.push(ty);
        len_delim(1, &canvaz) // canvas = field 1
    }

    /// Encode a bare `Canvaz { url = 2, type = 4 }` — the shape the
    /// extended-metadata CANVAZ extension actually returns (no wrapper).
    fn encode_canvaz(url: &str, ty: u8) -> Vec<u8> {
        let mut canvaz = len_delim(2, url.as_bytes());
        canvaz.push(4 << 3); // field 4, wire type 0 (varint)
        canvaz.push(ty);
        canvaz
    }

    #[test]
    fn decodes_video_looping() {
        let e = parse_canvas(&encode("https://canvaz.scdn.co/x.mp4", 2)).unwrap();
        assert_eq!(e.url, "https://canvaz.scdn.co/x.mp4");
        assert_eq!(e.kind, CanvasType::VideoLooping);
        assert!(e.kind.is_video());
    }

    #[test]
    fn decodes_bare_canvaz_message() {
        // The real extended-metadata payload: inner Canvaz, no wrapper.
        let e = parse_canvas(&encode_canvaz("https://canvaz.scdn.co/y.mp4", 1)).unwrap();
        assert_eq!(e.url, "https://canvaz.scdn.co/y.mp4");
        assert_eq!(e.kind, CanvasType::Video);
        assert!(e.kind.is_video());
    }

    #[test]
    fn image_kind_not_video() {
        let e = parse_canvas(&encode("https://i.scdn.co/x.jpg", 0)).unwrap();
        assert_eq!(e.kind, CanvasType::Image);
        assert!(!e.kind.is_video());
    }

    #[test]
    fn empty_response_is_none() {
        assert!(parse_canvas(&[]).is_none());
    }

    #[test]
    fn empty_url_is_none() {
        assert!(parse_canvas(&encode("", 2)).is_none());
    }

    #[test]
    fn garbage_is_none_not_panic() {
        assert!(parse_canvas(&[0xFF, 0xFF, 0xFF]).is_none());
    }
}
