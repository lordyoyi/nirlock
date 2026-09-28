//! `parse_uvcm`, ported 1:1 from `phase0/v4l2cap.cpp` (DESIGN §2.4).
//!
//! Buffer layout: repeated `struct uvc_meta_buf { u64 ns; u16 sof; u8
//! length; u8 flags; u8 buf[length-2]; }`. `length`/`flags` are the first
//! two bytes of the UVC payload header; the standard header continues with
//! PTS (4 bytes, if flags bit 2) and SCR (6 bytes, if flags bit 3). With
//! format `UVCM` whatever follows is a list of `KSCAMERA_METADATA_ITEMHEADER
//! { u32 id; u32 size; }` records, size including the 8 header bytes. Id 6
//! = FrameIllumination: payload `{ u32 flags; u32 reserved }`, flags bit 0
//! = emitter ON.
//!
//! MS UVC 1.5 extensions s2.2.3: the metadata of a frame is the
//! CONCATENATION of the partial blobs carried by all payload headers of
//! that frame, so an item may straddle two blocks. The parser therefore
//! first concatenates the bytes after each standard header, then walks the
//! items over the whole blob. If the FID bit toggles inside one buffer
//! (`fid_mixed`), the blocks before the toggle belong to the previous frame
//! slot and are discarded from the blob (observed on the first buffer after
//! STREAMON). All-zero trailing bytes are accepted as padding; anything
//! else that does not parse sets `parse_error`.

/// UVC payload header flag bits (`<linux/usb/video.h>`).
pub const UVC_STREAM_FID: u8 = 1 << 0;
pub const UVC_STREAM_EOF: u8 = 1 << 1;
pub const UVC_STREAM_PTS: u8 = 1 << 2;
pub const UVC_STREAM_SCR: u8 = 1 << 3;
pub const UVC_STREAM_EOH: u8 = 1 << 7;

/// `KSCAMERA_METADATA_ITEMHEADER` id of FrameIllumination.
pub const MS_METADATA_ID_FRAME_ILLUMINATION: u32 = 6;

/// Result of parsing one UVCM metadata buffer (= one video frame).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MetaInfo {
    /// A metadata buffer with > 0 bytes was paired.
    pub present: bool,
    /// MetadataId 6 FrameIllumination flag bit 0.
    pub lit: Option<bool>,
    /// UVC payload header FID bit (last block).
    pub fid: Option<bool>,
    /// FID toggled inside the buffer (stale leading blocks dropped).
    pub fid_mixed: bool,
    /// `uvc_meta_buf` blocks in the buffer.
    pub blocks: u32,
    /// Microsoft metadata items seen.
    pub items: u32,
    pub bytes: u32,
    /// `V4L2_BUF_FLAG_ERROR` on the METADATA buffer (uvcvideo sets it on
    /// metadata overflow or copies it from the video buffer). The content
    /// is then not trusted: `lit` is left unknown.
    pub buf_error: bool,
    /// Malformed block, truncated item, or two FrameIllumination items
    /// that disagree (then `lit` is unknown). Never guessed around.
    pub parse_error: bool,
    /// The whole raw UVCM buffer as dequeued (`meta_raw` in `record`;
    /// fixtures for the parser). Empty when no buffer was paired.
    pub raw: Vec<u8>,
}

fn rd32(p: &[u8]) -> u32 {
    u32::from_le_bytes([p[0], p[1], p[2], p[3]])
}

/// Parses one UVCM buffer. `raw` is left empty; `drain_meta` fills it.
pub fn parse_uvcm(data: &[u8]) -> MetaInfo {
    let len = data.len();
    let mut m = MetaInfo {
        bytes: len as u32,
        ..MetaInfo::default()
    };
    if len == 0 {
        return m;
    }
    m.present = true;

    // Pass 1: walk the uvc_meta_buf blocks, remember the FID bit and append
    // whatever follows each standard UVC header to one blob.
    let mut blob: Vec<u8> = Vec::new();
    let mut j = 0usize;
    while j + 12 <= len {
        // Each block: ns(8) sof(2) then the UVC payload header, whose first
        // byte is its own length (covering the length and flags bytes).
        let hl = data[j + 10] as usize;
        let fl = data[j + 11];
        if hl < 2 || j + 10 + hl > len {
            m.parse_error = true;
            break;
        }
        let hdr = &data[j + 10..j + 10 + hl];
        let fid = fl & UVC_STREAM_FID != 0;
        if m.fid.is_some_and(|prev| prev != fid) {
            // FID toggled inside one buffer: the earlier blocks belong to the
            // PREVIOUS frame slot. Observed on this camera: the first buffer
            // after STREAMON starts with a header-only payload of the skipped
            // dark slot (FID=0) followed by the lit frame's payload (FID=1).
            // Only the blocks of the last FID run describe this frame.
            m.fid_mixed = true;
            blob.clear();
        }
        m.fid = Some(fid);
        m.blocks += 1;
        let k = 2
            + if fl & UVC_STREAM_PTS != 0 { 4 } else { 0 }
            + if fl & UVC_STREAM_SCR != 0 { 6 } else { 0 };
        if k > hl {
            m.parse_error = true;
        } else {
            blob.extend_from_slice(&hdr[k..]);
        }
        j += 10 + hl;
    }
    if !m.parse_error && j != len {
        m.parse_error = true; // trailing partial block
    }

    // Pass 2: KSCAMERA_METADATA_ITEMHEADER records over the concatenated blob.
    let mut k = 0usize;
    let mut conflict = false;
    while k + 8 <= blob.len() {
        let id = rd32(&blob[k..]);
        let size = rd32(&blob[k + 4..]) as usize;
        if size < 8 || k + size > blob.len() {
            break;
        }
        m.items += 1;
        if id == MS_METADATA_ID_FRAME_ILLUMINATION && size >= 12 {
            let lit = rd32(&blob[k + 8..]) & 1 != 0;
            if m.lit.is_some_and(|prev| prev != lit) {
                conflict = true;
            }
            m.lit = Some(lit);
        }
        k += size;
    }
    // Leftover bytes: all-zero = padding, anything else = truncated/garbled item.
    if blob[k..].iter().any(|&b| b != 0) {
        m.parse_error = true;
    }
    if conflict {
        m.parse_error = true;
        m.lit = None;
    }
    m
}

/// Test-vector builders shared with the pairing tests: the `block` /
/// `item6` helpers of fuprobe's `cmd_selftest`.
#[cfg(test)]
pub(crate) mod fixtures {
    use super::*;

    /// Appends one `uvc_meta_buf` block: 10 bytes of ns+sof, then a UVC
    /// payload header with `flags` (PTS/SCR fields filled with 0xAA) and
    /// `payload` after it.
    pub fn block(buf: &mut Vec<u8>, flags: u8, payload: &[u8]) {
        buf.extend_from_slice(&[0u8; 10]);
        let pts = flags & UVC_STREAM_PTS != 0;
        let scr = flags & UVC_STREAM_SCR != 0;
        let extra = if pts { 4 } else { 0 } + if scr { 6 } else { 0 };
        buf.push((2 + extra + payload.len()) as u8);
        buf.push(flags);
        buf.extend(std::iter::repeat_n(0xAAu8, extra));
        buf.extend_from_slice(payload);
    }

    /// A 16-byte FrameIllumination item with the given flag word.
    pub fn item6(flag: u32) -> Vec<u8> {
        let mut v = vec![6, 0, 0, 0, 16, 0, 0, 0];
        v.extend_from_slice(&flag.to_le_bytes());
        v.extend_from_slice(&[0, 0, 0, 0]);
        v
    }

    /// A complete single-block buffer as this camera sends it: EOH | SCR |
    /// PTS | FID(fid), 38 bytes, one item 6.
    pub fn frame(fid: bool, lit: bool) -> Vec<u8> {
        let mut b = Vec::new();
        block(
            &mut b,
            0x8C | if fid { UVC_STREAM_FID } else { 0 },
            &item6(u32::from(lit)),
        );
        b
    }
}

#[cfg(test)]
mod tests {
    use super::fixtures::*;
    use super::*;

    // The vectors of fuprobe's `cmd_selftest` (main.cpp), one test each.

    #[test]
    fn single_38_byte_block_lit() {
        let mut b = Vec::new();
        block(&mut b, 0x8D, &item6(1)); // EOH | SCR | PTS | FID
        let m = parse_uvcm(&b);
        assert_eq!(b.len(), 38);
        assert_eq!(m.lit, Some(true));
        assert_eq!((m.items, m.blocks), (1, 1));
        assert!(!m.parse_error);
        assert_eq!(m.fid, Some(true));
        assert!(m.present && !m.fid_mixed && !m.buf_error);
        assert_eq!(m.bytes, 38);
        assert!(m.raw.is_empty());
    }

    #[test]
    fn header_only_block_plus_item_block_dark() {
        let mut b = Vec::new();
        block(&mut b, 0x8C, &[]); // header-only payload, as seen on the first frame
        block(&mut b, 0x8C, &item6(0));
        let m = parse_uvcm(&b);
        assert_eq!(b.len(), 60);
        assert_eq!(m.lit, Some(false));
        assert_eq!((m.blocks, m.items), (2, 1));
        assert!(!m.parse_error && !m.fid_mixed);
    }

    #[test]
    fn item_straddling_two_blocks_is_reassembled() {
        let it = item6(1);
        let mut b = Vec::new();
        block(&mut b, 0x8D, &it[..10]);
        block(&mut b, 0x8D, &it[10..]);
        let m = parse_uvcm(&b);
        assert_eq!(m.lit, Some(true));
        assert_eq!(m.items, 1);
        assert!(!m.parse_error);
    }

    #[test]
    fn two_disagreeing_illumination_items_are_unknown_plus_parse_error() {
        let mut b = Vec::new();
        block(&mut b, 0x8D, &item6(1));
        block(&mut b, 0x8D, &item6(0));
        let m = parse_uvcm(&b);
        assert_eq!(m.lit, None);
        assert!(m.parse_error);
        assert_eq!(m.items, 2);
    }

    #[test]
    fn fid_toggle_inside_a_buffer_uses_only_the_last_run() {
        let mut b = Vec::new();
        block(&mut b, 0x8C, &item6(0)); // stale payload of the previous (dark) slot, FID=0
        block(&mut b, 0x8D, &item6(1)); // this frame, FID=1
        let m = parse_uvcm(&b);
        assert_eq!(m.lit, Some(true));
        assert!(m.fid_mixed);
        assert!(!m.parse_error);
        assert_eq!(m.fid, Some(true));
        assert_eq!(m.blocks, 2);
        assert_eq!(m.items, 1, "the stale item must not be counted");
    }

    #[test]
    fn truncated_item_is_unknown_plus_parse_error() {
        let it = item6(1);
        let mut b = Vec::new();
        block(&mut b, 0x8D, &it[..10]);
        let m = parse_uvcm(&b);
        assert_eq!(m.lit, None);
        assert!(m.parse_error);
    }

    #[test]
    fn standard_header_only_is_unknown_and_empty_is_not_present() {
        let mut b = Vec::new();
        block(&mut b, 0x0C, &[]);
        let m = parse_uvcm(&b);
        assert!(m.present && m.lit.is_none() && !m.parse_error && m.items == 0);
        let m = parse_uvcm(&[]);
        assert!(!m.present);
        assert_eq!(m, MetaInfo::default());
    }

    // Extra edge cases the C++ code handles implicitly.

    #[test]
    fn malformed_blocks_set_parse_error() {
        // Header length < 2.
        let mut b = vec![0u8; 10];
        b.extend_from_slice(&[1, 0x8C]);
        assert!(parse_uvcm(&b).parse_error);
        // Header length beyond the buffer.
        let mut b = vec![0u8; 10];
        b.extend_from_slice(&[40, 0x8C, 0, 0]);
        assert!(parse_uvcm(&b).parse_error);
        // Trailing partial block (fewer than 12 bytes left).
        let mut b = frame(true, true);
        b.extend_from_slice(&[0, 0, 0]);
        assert!(parse_uvcm(&b).parse_error);
        // PTS+SCR flagged but header too short to hold them.
        let mut b = vec![0u8; 10];
        b.extend_from_slice(&[4, 0x8D, 0xAA, 0xAA]);
        let m = parse_uvcm(&b);
        assert!(m.parse_error && m.lit.is_none());
        // All-zero padding after the item is fine; a non-zero byte is not.
        let mut b = Vec::new();
        let mut p = item6(1);
        p.extend_from_slice(&[0, 0, 0, 0]);
        block(&mut b, 0x8D, &p);
        assert!(!parse_uvcm(&b).parse_error);
        let mut b = Vec::new();
        let mut p = item6(1);
        p.extend_from_slice(&[0, 1, 0, 0]);
        block(&mut b, 0x8D, &p);
        assert!(parse_uvcm(&b).parse_error);
    }

    #[test]
    fn other_items_are_counted_but_do_not_label() {
        let mut p = vec![9, 0, 0, 0, 12, 0, 0, 0, 1, 2, 3, 4]; // some other id
        p.extend(item6(1));
        let mut b = Vec::new();
        block(&mut b, 0x8D, &p);
        let m = parse_uvcm(&b);
        assert_eq!(m.items, 2);
        assert_eq!(m.lit, Some(true));
        // An id-6 item shorter than 12 bytes carries no flag word.
        let mut b = Vec::new();
        block(&mut b, 0x8D, &[6, 0, 0, 0, 8, 0, 0, 0]);
        let m = parse_uvcm(&b);
        assert_eq!((m.items, m.lit), (1, None));
        assert!(!m.parse_error);
        // Item size 0 or beyond the blob stops the walk; leftover non-zero
        // bytes are then a parse error.
        let mut b = Vec::new();
        block(&mut b, 0x8D, &[6, 0, 0, 0, 0, 0, 0, 0]);
        assert!(parse_uvcm(&b).parse_error);
    }

    #[test]
    fn fixtures_match_the_camera_shape() {
        let f = frame(true, true);
        assert_eq!(f.len(), 38);
        let m = parse_uvcm(&f);
        assert_eq!((m.fid, m.lit), (Some(true), Some(true)));
        let m = parse_uvcm(&frame(false, false));
        assert_eq!((m.fid, m.lit), (Some(false), Some(false)));
    }

    /// Real buffers captured from the pinned camera (3277:0055, kernel
    /// 7.2.5) by `nirlockctl record --label m1-final` on 2026-09-23. They
    /// contain the UVC/UVCM headers only — no image data — so they can live
    /// in the repository. DESIGN §10 asks for exactly this: the synthetic
    /// vectors pin the shape, these pin the firmware's actual bytes.
    #[test]
    fn real_camera_buffers_parse_as_recorded() {
        fn hex(h: &str) -> Vec<u8> {
            (0..h.len() / 2)
                .map(|i| u8::from_str_radix(&h[2 * i..2 * i + 2], 16).unwrap())
                .collect()
        }

        // Sequence 1, the first buffer after STREAMON: two blocks, because
        // the header of a skipped frame slot precedes the lit payload. The
        // parser must drop the blocks before the FID change and still report
        // one item. This is the FID-mixed case of `v4l2cap.cpp` 197.
        let m = parse_uvcm(&hex(
            "761c6198624b000068000c8c00000000000000000000665c949e624b0000d0001c8dd67d18007c9d1800cf0006000000100000000100000000000000",
        ));
        assert_eq!(
            (m.blocks, m.items, m.bytes, m.fid_mixed),
            (2, 1, 60, true),
            "seq 1: two blocks, one item, FID change inside the buffer"
        );
        assert_eq!((m.lit, m.fid), (Some(true), Some(true)));
        assert!(!m.parse_error);

        // Sequence 2, the ambient half of the pair: one block, one item.
        let m = parse_uvcm(&hex(
            "3af0a1a2624b000014011c8c16c02700a0e42700120106000000100000000000000000000000",
        ));
        assert_eq!((m.blocks, m.items, m.bytes, m.fid_mixed), (1, 1, 38, false));
        assert_eq!((m.lit, m.fid), (Some(false), Some(false)));
        assert!(!m.parse_error);

        // Sequence 3, illuminated again: FID toggled with the label, which
        // is the relation the shadow cross-check (a) watches.
        let m = parse_uvcm(&hex(
            "8699afa6624b000058011c8d560237006b243700540106000000100000000100000000000000",
        ));
        assert_eq!((m.blocks, m.items, m.bytes, m.fid_mixed), (1, 1, 38, false));
        assert_eq!((m.lit, m.fid), (Some(true), Some(true)));
        assert!(!m.parse_error);
    }
}
