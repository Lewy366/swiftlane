//! Copying a photo's EXIF block (camera, date, location...) into a new file, the way the Python
//! version did with Pillow: the pixels are already turned upright, so the orientation becomes
//! "normal", and the little preview picture some cameras hide in EXIF is left out (it would still
//! show the uncropped, unedited photo). Everything is rebuilt from scratch, so nothing that was
//! left out survives as stray bytes. "Remove location & camera info" never gets here: then no
//! EXIF is written at all.

const ORIENTATION: u16 = 0x0112;
const EXIF_IFD: u16 = 0x8769;
const GPS_IFD: u16 = 0x8825;
const INTEROP_IFD: u16 = 0xA005;
const MAX_ENTRIES: usize = 1000;

#[derive(Clone, Copy)]
struct Order {
    little: bool,
}

impl Order {
    fn u16(self, b: &[u8], at: usize) -> Option<u16> {
        let s: [u8; 2] = b.get(at..at + 2)?.try_into().ok()?;
        Some(if self.little { u16::from_le_bytes(s) } else { u16::from_be_bytes(s) })
    }
    fn u32(self, b: &[u8], at: usize) -> Option<u32> {
        let s: [u8; 4] = b.get(at..at + 4)?.try_into().ok()?;
        Some(if self.little { u32::from_le_bytes(s) } else { u32::from_be_bytes(s) })
    }
    fn put16(self, v: u16) -> [u8; 2] {
        if self.little { v.to_le_bytes() } else { v.to_be_bytes() }
    }
    fn put32(self, v: u32) -> [u8; 4] {
        if self.little { v.to_le_bytes() } else { v.to_be_bytes() }
    }
}

struct Entry {
    tag: u16,
    typ: u16,
    count: u32,
    data: Vec<u8>,          // the value bytes, in the file's byte order
    sub: Option<Vec<Entry>>, // a sub-directory (Exif, GPS, Interop)
}

fn type_size(typ: u16) -> Option<usize> {
    Some(match typ {
        1 | 2 | 6 | 7 => 1,
        3 | 8 => 2,
        4 | 9 | 11 | 13 => 4,
        5 | 10 | 12 => 8,
        _ => return None,
    })
}

/// The raw TIFF-style block, without the "Exif\0\0" marker JPEG files put in front.
pub fn raw(block: &[u8]) -> &[u8] {
    block.strip_prefix(b"Exif\0\0").unwrap_or(block)
}

fn read_ifd(b: &[u8], o: Order, at: usize, depth: u8) -> Option<Vec<Entry>> {
    let n = o.u16(b, at)? as usize;
    if n > MAX_ENTRIES {
        return None;
    }
    let mut out = Vec::with_capacity(n);
    for k in 0..n {
        let e = at + 2 + 12 * k;
        let (tag, typ, count) = (o.u16(b, e)?, o.u16(b, e + 2)?, o.u32(b, e + 4)?);
        let subdir = matches!(tag, EXIF_IFD | GPS_IFD | INTEROP_IFD) && matches!(typ, 4 | 13) && count == 1;
        if subdir {
            if depth < 2 {
                if let Some(sub) = read_ifd(b, o, o.u32(b, e + 8)? as usize, depth + 1) {
                    out.push(Entry { tag, typ: 4, count: 1, data: Vec::new(), sub: Some(sub) });
                }
            }
            continue;
        }
        let Some(size) = type_size(typ).and_then(|s| s.checked_mul(count as usize)) else { continue };
        let data = if size <= 4 {
            b.get(e + 8..e + 8 + size)?.to_vec()
        } else {
            let off = o.u32(b, e + 8)? as usize;
            match b.get(off..off.checked_add(size)?) {
                Some(d) => d.to_vec(),
                None => continue, // broken value: leave it out
            }
        };
        out.push(Entry { tag, typ, count, data, sub: None });
    }
    Some(out)
}

/// Writes one directory at the end of `out` (and its values and sub-directories after it);
/// returns where it starts.
fn write_ifd(out: &mut Vec<u8>, o: Order, entries: &mut [Entry]) -> u32 {
    entries.sort_by_key(|e| e.tag);
    if out.len() % 2 == 1 {
        out.push(0);
    }
    let start = out.len();
    out.extend_from_slice(&o.put16(entries.len() as u16));
    out.resize(start + 2 + 12 * entries.len() + 4, 0); // the table and a zero "next directory" link
    for (k, e) in entries.iter_mut().enumerate() {
        let slot = start + 2 + 12 * k;
        let value: [u8; 4] = if let Some(sub) = e.sub.as_mut() {
            o.put32(write_ifd(out, o, sub))
        } else if e.data.len() <= 4 {
            let mut v = [0u8; 4];
            v[..e.data.len()].copy_from_slice(&e.data);
            v
        } else {
            if out.len() % 2 == 1 {
                out.push(0);
            }
            let at = out.len() as u32;
            out.extend_from_slice(&e.data);
            o.put32(at)
        };
        out[slot..slot + 2].copy_from_slice(&o.put16(e.tag));
        out[slot + 2..slot + 4].copy_from_slice(&o.put16(e.typ));
        out[slot + 4..slot + 8].copy_from_slice(&o.put32(e.count));
        out[slot + 8..slot + 12].copy_from_slice(&value);
    }
    start as u32
}

/// The EXIF block to put in a new file made from this photo, or None if it can't be read.
pub fn for_copy(block: &[u8]) -> Option<Vec<u8>> {
    let b = raw(block);
    let o = match b.get(0..4)? {
        [b'I', b'I', 42, 0] => Order { little: true },
        [b'M', b'M', 0, 42] => Order { little: false },
        _ => return None,
    };
    let mut ifd0 = read_ifd(b, o, o.u32(b, 4)? as usize, 0)?;
    for e in ifd0.iter_mut().filter(|e| e.tag == ORIENTATION) {
        // the pixels are upright now
        e.typ = 3;
        e.count = 1;
        e.data = o.put16(1).to_vec();
    }
    let mut out = if o.little { b"II\x2a\0".to_vec() } else { b"MM\0\x2a".to_vec() };
    out.extend_from_slice(&o.put32(8));
    write_ifd(&mut out, o, &mut ifd0);
    Some(out)
}

#[cfg(test)]
pub mod tests {
    use super::*;

    /// A small EXIF block like a phone's: camera make, orientation, GPS position, and a hidden
    /// thumbnail (IFD1) whose bytes are "THUMBNAIL-SECRET".
    pub fn sample(orientation: u16) -> Vec<u8> {
        let mut b = b"II\x2a\0\x08\0\0\0".to_vec();
        // IFD0 at 8: 3 entries
        let ifd0 = 8;
        let make_at = ifd0 + 2 + 3 * 12 + 4; // 50
        let gps_at = make_at + 12; // 62
        let ifd1_at = gps_at + 2 + 2 * 12 + 4 + 24; // after GPS dir + its 3 rationals
        b.extend_from_slice(&3u16.to_le_bytes());
        b.extend_from_slice(&[0x0f, 0x01, 2, 0]); // Make, ASCII
        b.extend_from_slice(&12u32.to_le_bytes());
        b.extend_from_slice(&(make_at as u32).to_le_bytes());
        b.extend_from_slice(&[0x12, 0x01, 3, 0]); // Orientation, SHORT
        b.extend_from_slice(&1u32.to_le_bytes());
        b.extend_from_slice(&orientation.to_le_bytes());
        b.extend_from_slice(&[0, 0]);
        b.extend_from_slice(&[0x25, 0x88, 4, 0]); // GPS IFD pointer
        b.extend_from_slice(&1u32.to_le_bytes());
        b.extend_from_slice(&(gps_at as u32).to_le_bytes());
        b.extend_from_slice(&(ifd1_at as u32).to_le_bytes()); // next: IFD1 (thumbnail)
        b.extend_from_slice(b"SecretPhone\0");
        assert_eq!(b.len(), gps_at);
        // GPS IFD: LatitudeRef "N", Latitude 3 rationals
        b.extend_from_slice(&2u16.to_le_bytes());
        b.extend_from_slice(&[0x01, 0x00, 2, 0]);
        b.extend_from_slice(&2u32.to_le_bytes());
        b.extend_from_slice(b"N\0\0\0");
        b.extend_from_slice(&[0x02, 0x00, 5, 0]);
        b.extend_from_slice(&3u32.to_le_bytes());
        b.extend_from_slice(&((gps_at + 2 + 24 + 4) as u32).to_le_bytes());
        b.extend_from_slice(&0u32.to_le_bytes());
        for v in [51u32, 1, 30, 1, 1234, 100] {
            b.extend_from_slice(&v.to_le_bytes());
        }
        assert_eq!(b.len(), ifd1_at);
        // IFD1: one entry pointing at the "thumbnail"
        b.extend_from_slice(&1u16.to_le_bytes());
        b.extend_from_slice(&[0x01, 0x02, 4, 0]);
        b.extend_from_slice(&1u32.to_le_bytes());
        b.extend_from_slice(&((ifd1_at + 18) as u32).to_le_bytes());
        b.extend_from_slice(&0u32.to_le_bytes());
        b.extend_from_slice(b"THUMBNAIL-SECRET");
        b
    }

    fn contains(hay: &[u8], needle: &[u8]) -> bool {
        hay.windows(needle.len()).any(|w| w == needle)
    }

    #[test]
    fn copy_keeps_camera_and_gps_but_not_thumbnail_and_resets_orientation() {
        let src = sample(6);
        let mut with_marker = b"Exif\0\0".to_vec();
        with_marker.extend_from_slice(&src);
        let out = for_copy(&with_marker).unwrap();
        assert!(contains(&out, b"SecretPhone"));
        assert!(!contains(&out, b"THUMBNAIL-SECRET"));
        assert_eq!(image::metadata::Orientation::from_exif_chunk(&out), Some(image::metadata::Orientation::NoTransforms));
        assert_eq!(image::metadata::Orientation::from_exif_chunk(&src), Some(image::metadata::Orientation::Rotate90));
        // the GPS rationals came along intact
        let mut lat = Vec::new();
        for v in [51u32, 1, 30, 1, 1234, 100] {
            lat.extend_from_slice(&v.to_le_bytes());
        }
        assert!(contains(&out, &lat));
        // and a second pass gives the same thing
        assert_eq!(for_copy(&out).unwrap(), out);
    }

    #[test]
    fn garbage_is_refused() {
        assert!(for_copy(b"nonsense").is_none());
        assert!(for_copy(b"II\x2a\0\xff\xff\xff\x7f").is_none());
        let mut loopy = b"MM\0\x2a\0\0\0\x08".to_vec();
        loopy.extend_from_slice(&[0, 1, 0x87, 0x69, 0, 4, 0, 0, 0, 1, 0, 0, 0, 8, 0, 0, 0, 0]); // Exif IFD points at itself
        assert!(for_copy(&loopy).is_some());
    }
}
