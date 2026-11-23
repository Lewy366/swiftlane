//! Lossy WebP pictures (the image crate only writes lossless WebP, which is far too big for
//! photos). The encoding itself is Google's libwebp, built from source by libwebp-sys with the
//! normal C compiler; only our own pixels ever go into it, files are always read by the image
//! crate (pure Rust). Metadata (EXIF, colour profile) is added here in plain Rust.

use libwebp_sys as w;

/// WebP's own size limit.
pub const MAX_SIDE: u32 = 16383;

fn check_size(width: u32, height: u32) -> Result<(), String> {
    if width == 0 || height == 0 || width > MAX_SIDE || height > MAX_SIDE {
        return Err(format!("WebP pictures can be at most {MAX_SIDE} pixels wide and tall. Make it smaller or pick another format."));
    }
    Ok(())
}

/// A still picture. `pixels`: RGB (3 bytes each) or RGBA (4 bytes each), rows top to bottom.
pub fn encode(pixels: &[u8], width: u32, height: u32, alpha: bool, quality: f32) -> Result<Vec<u8>, String> {
    check_size(width, height)?;
    let bpp = if alpha { 4 } else { 3 };
    let stride = width as usize * bpp;
    if pixels.len() < stride * height as usize {
        return Err("The picture couldn't be saved as WebP.".into());
    }
    let mut out: *mut u8 = std::ptr::null_mut();
    // SAFETY: `pixels` holds at least stride * height bytes (checked above); libwebp only reads them
    // and hands back a buffer it allocated, which we copy and free with WebPFree.
    let n = unsafe {
        if alpha {
            w::WebPEncodeRGBA(pixels.as_ptr(), width as i32, height as i32, stride as i32, quality, &mut out)
        } else {
            w::WebPEncodeRGB(pixels.as_ptr(), width as i32, height as i32, stride as i32, quality, &mut out)
        }
    };
    if out.is_null() {
        return Err("The picture couldn't be saved as WebP.".into());
    }
    // SAFETY: libwebp returned `n` valid bytes at `out`.
    let bytes = unsafe { std::slice::from_raw_parts(out, n) }.to_vec();
    unsafe { w::WebPFree(out.cast()) };
    if n == 0 {
        return Err("The picture couldn't be saved as WebP.".into());
    }
    Ok(bytes)
}

/// Adds an EXIF block and/or colour profile to a still WebP made by `encode` (the extended
/// "VP8X" layout the WebP spec describes). Anything else in the file is left as it is.
pub fn add_metadata(webp: &[u8], width: u32, height: u32, exif: Option<&[u8]>, icc: Option<&[u8]>) -> Vec<u8> {
    let exif = exif.filter(|e| !e.is_empty());
    let icc = icc.filter(|i| !i.is_empty());
    if (exif.is_none() && icc.is_none()) || webp.len() < 12 || &webp[0..4] != b"RIFF" || &webp[8..12] != b"WEBP" {
        return webp.to_vec();
    }
    // the chunks after the header: (fourcc, payload)
    let mut chunks: Vec<([u8; 4], &[u8])> = Vec::new();
    let mut i = 12;
    while i + 8 <= webp.len() {
        let id: [u8; 4] = webp[i..i + 4].try_into().unwrap();
        let len = u32::from_le_bytes(webp[i + 4..i + 8].try_into().unwrap()) as usize;
        let Some(body) = webp.get(i + 8..i + 8 + len) else { return webp.to_vec() };
        chunks.push((id, body));
        i += 8 + len + (len & 1);
    }
    let mut flags = 0u8;
    let mut images: Vec<([u8; 4], &[u8])> = Vec::new();
    for (id, body) in &chunks {
        match id {
            b"VP8X" => flags = body.first().copied().unwrap_or(0) & 0x10, // keep the alpha flag
            b"ICCP" | b"EXIF" | b"XMP " => {}
            _ => {
                if id == b"ALPH" {
                    flags |= 0x10;
                }
                images.push((*id, body));
            }
        }
    }
    if images.iter().any(|(id, body)| id == b"VP8L" && body.len() > 4 && body[4] & 0x10 != 0) {
        flags |= 0x10; // lossless picture that uses alpha
    }
    if icc.is_some() {
        flags |= 0x20;
    }
    if exif.is_some() {
        flags |= 0x08;
    }
    let mut vp8x = vec![flags, 0, 0, 0];
    vp8x.extend_from_slice(&(width - 1).to_le_bytes()[..3]);
    vp8x.extend_from_slice(&(height - 1).to_le_bytes()[..3]);

    let mut out = b"RIFF\0\0\0\0WEBP".to_vec();
    let mut put = |id: &[u8], body: &[u8]| {
        out.extend_from_slice(id);
        out.extend_from_slice(&(body.len() as u32).to_le_bytes());
        out.extend_from_slice(body);
        if body.len() % 2 == 1 {
            out.push(0);
        }
    };
    put(b"VP8X", &vp8x);
    if let Some(icc) = icc {
        put(b"ICCP", icc);
    }
    for (id, body) in images {
        put(&id, body);
    }
    if let Some(exif) = exif {
        put(b"EXIF", exif);
    }
    let size = (out.len() - 8) as u32;
    out[4..8].copy_from_slice(&size.to_le_bytes());
    out
}
