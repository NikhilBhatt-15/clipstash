//! Clipboard images: DIB <-> RGBA conversion, PNG encode/decode, thumbnails
//! and a content hash for de-duplication. Pure Rust, no Win32 calls.

/// 8-bit RGBA pixels, top row first.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Rgba {
    pub w: u32,
    pub h: u32,
    pub px: Vec<u8>,
}

/// Images larger than this (pixels) are not recorded.
pub const MAX_PIXELS: u64 = 64 * 1024 * 1024;

fn u16_at(b: &[u8], o: usize) -> Option<u16> {
    Some(u16::from_le_bytes(b.get(o..o + 2)?.try_into().ok()?))
}
fn u32_at(b: &[u8], o: usize) -> Option<u32> {
    Some(u32::from_le_bytes(b.get(o..o + 4)?.try_into().ok()?))
}
fn i32_at(b: &[u8], o: usize) -> Option<i32> {
    u32_at(b, o).map(|v| v as i32)
}

const BI_RGB: u32 = 0;
const BI_BITFIELDS: u32 = 3;
const BI_ALPHABITFIELDS: u32 = 6;

/// Extracts the 8-bit channel selected by `mask` (0 = channel absent).
fn channel(v: u32, mask: u32) -> Option<u8> {
    if mask == 0 {
        return None;
    }
    let shift = mask.trailing_zeros();
    let bits = (mask >> shift).count_ones();
    let raw = (v & mask) >> shift;
    Some(if bits >= 8 { (raw >> (bits - 8)) as u8 } else { (raw * 255 / ((1 << bits) - 1)) as u8 })
}

/// Parses a packed DIB (CF_DIB / CF_DIBV5 contents): 24- and 32-bit,
/// uncompressed or with bit-field masks, bottom-up or top-down.
pub fn dib_to_rgba(dib: &[u8]) -> Option<Rgba> {
    let hdr = u32_at(dib, 0)? as usize;
    if hdr < 40 {
        return None;
    }
    let w = i32_at(dib, 4)?;
    let h = i32_at(dib, 8)?;
    let bpp = u16_at(dib, 14)?;
    let comp = u32_at(dib, 16)?;
    let clr_used = u32_at(dib, 32)? as usize;
    if w <= 0 || h == 0 || (w as u64) * (h.unsigned_abs() as u64) > MAX_PIXELS {
        return None;
    }
    let (w, top_down, h) = (w as u32, h < 0, h.unsigned_abs());

    let (mut rm, mut gm, mut bm, mut am) = (0x00ff_0000, 0x0000_ff00, 0x0000_00ff, 0);
    let mut masks_after_header = 0;
    match comp {
        BI_RGB => {}
        BI_BITFIELDS | BI_ALPHABITFIELDS if bpp == 32 => {
            if hdr >= 56 {
                // V4/V5 header: masks live inside it.
                (rm, gm, bm, am) = (u32_at(dib, 40)?, u32_at(dib, 44)?, u32_at(dib, 48)?, u32_at(dib, 52)?);
            } else {
                (rm, gm, bm) = (u32_at(dib, hdr)?, u32_at(dib, hdr + 4)?, u32_at(dib, hdr + 8)?);
                masks_after_header = 12;
                if comp == BI_ALPHABITFIELDS {
                    am = u32_at(dib, hdr + 12)?;
                    masks_after_header = 16;
                }
            }
        }
        _ => return None,
    }
    if bpp != 24 && bpp != 32 {
        return None;
    }
    // 24/32-bit DIBs may still carry an (unused) colour table.
    let offset = hdr + masks_after_header + clr_used * 4;
    let stride = (w as usize * bpp as usize).div_ceil(32) * 4;
    let data = dib.get(offset..offset + stride * h as usize)?;

    let mut px = Vec::with_capacity(w as usize * h as usize * 4);
    for y in 0..h as usize {
        let src_row = if top_down { y } else { h as usize - 1 - y };
        let row = &data[src_row * stride..src_row * stride + stride];
        for x in 0..w as usize {
            if bpp == 24 {
                let p = &row[x * 3..x * 3 + 3];
                px.extend_from_slice(&[p[2], p[1], p[0], 255]);
            } else {
                let v = u32::from_le_bytes(row[x * 4..x * 4 + 4].try_into().unwrap());
                let a = channel(v, am).unwrap_or(255);
                px.extend_from_slice(&[channel(v, rm)?, channel(v, gm)?, channel(v, bm)?, a]);
            }
        }
    }
    // An alpha mask that is zero everywhere means "no alpha" in practice.
    if am != 0 && px.as_chunks::<4>().0.iter().all(|p| p[3] == 0) {
        px.as_chunks_mut::<4>().0.iter_mut().for_each(|p| p[3] = 255);
    }
    Some(Rgba { w, h, px })
}

/// A packed 32-bit bottom-up BI_RGB DIB, the most widely understood form.
pub fn rgba_to_dib(img: &Rgba) -> Vec<u8> {
    let mut out = Vec::with_capacity(40 + img.px.len());
    out.extend_from_slice(&40u32.to_le_bytes());
    out.extend_from_slice(&(img.w as i32).to_le_bytes());
    out.extend_from_slice(&(img.h as i32).to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes()); // planes
    out.extend_from_slice(&32u16.to_le_bytes()); // bpp
    out.extend_from_slice(&BI_RGB.to_le_bytes());
    out.extend_from_slice(&(img.px.len() as u32).to_le_bytes());
    out.extend_from_slice(&[0; 16]); // resolution, colours used/important
    let row = img.w as usize * 4;
    for y in (0..img.h as usize).rev() {
        for p in img.px[y * row..y * row + row].as_chunks::<4>().0.iter() {
            out.extend_from_slice(&[p[2], p[1], p[0], p[3]]);
        }
    }
    out
}

pub fn encode_png(img: &Rgba) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    {
        let mut enc = png::Encoder::new(&mut out, img.w, img.h);
        enc.set_color(png::ColorType::Rgba);
        enc.set_depth(png::BitDepth::Eight);
        enc.set_compression(png::Compression::Fast);
        let mut writer = enc.write_header().ok()?;
        writer.write_image_data(&img.px).ok()?;
    }
    Some(out)
}

pub fn decode_png(bytes: &[u8]) -> Option<Rgba> {
    let mut dec = png::Decoder::new(bytes);
    dec.set_transformations(png::Transformations::normalize_to_color8());
    let mut reader = dec.read_info().ok()?;
    let (w, h) = (reader.info().width, reader.info().height);
    if w == 0 || h == 0 || w as u64 * h as u64 > MAX_PIXELS {
        return None;
    }
    let mut buf = vec![0; reader.output_buffer_size()];
    let frame = reader.next_frame(&mut buf).ok()?;
    let buf = &buf[..frame.buffer_size()];
    let px: Vec<u8> = match frame.color_type {
        png::ColorType::Rgba => buf.to_vec(),
        png::ColorType::Rgb => buf.as_chunks::<3>().0.iter().flat_map(|p| [p[0], p[1], p[2], 255]).collect(),
        png::ColorType::GrayscaleAlpha => buf.as_chunks::<2>().0.iter().flat_map(|p| [p[0], p[0], p[0], p[1]]).collect(),
        png::ColorType::Grayscale => buf.iter().flat_map(|&g| [g, g, g, 255]).collect(),
        png::ColorType::Indexed => return None, // expanded by normalize_to_color8
    };
    (px.len() == w as usize * h as usize * 4).then_some(Rgba { w, h, px })
}

/// Box-filtered downscale so the longer side is at most `max` (never upscales).
pub fn thumbnail(img: &Rgba, max: u32) -> Rgba {
    let scale = (max as f32 / img.w.max(img.h) as f32).min(1.0);
    let (tw, th) = (((img.w as f32 * scale).round() as u32).max(1), ((img.h as f32 * scale).round() as u32).max(1));
    let mut px = Vec::with_capacity((tw * th * 4) as usize);
    for ty in 0..th {
        let (y0, y1) = (ty * img.h / th, ((ty + 1) * img.h / th).max(ty * img.h / th + 1));
        for tx in 0..tw {
            let (x0, x1) = (tx * img.w / tw, ((tx + 1) * img.w / tw).max(tx * img.w / tw + 1));
            let mut sum = [0u64; 4];
            for y in y0..y1 {
                let row = (y * img.w) as usize * 4;
                for x in x0..x1 {
                    let p = &img.px[row + x as usize * 4..row + x as usize * 4 + 4];
                    for c in 0..4 {
                        sum[c] += p[c] as u64;
                    }
                }
            }
            let n = ((y1 - y0) * (x1 - x0)) as u64;
            px.extend(sum.iter().map(|s| (s / n) as u8));
        }
    }
    Rgba { w: tw, h: th, px }
}

/// FNV-1a over dimensions and pixels: identical screenshots dedupe even when
/// they arrive through different clipboard formats.
pub fn content_hash(img: &Rgba) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    let mut feed = |v: u64| {
        h ^= v;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    };
    feed(img.w as u64);
    feed(img.h as u64);
    let (chunks, rest) = img.px.as_chunks::<8>();
    for c in chunks {
        feed(u64::from_le_bytes(*c));
    }
    for &b in rest {
        feed(b as u64);
    }
    h
}

/// RGBA -> BGRA (GDI+ PixelFormat32bppARGB byte order).
pub fn to_bgra(img: &Rgba) -> Vec<u8> {
    img.px.as_chunks::<4>().0.iter().flat_map(|p| [p[2], p[1], p[0], p[3]]).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(w: u32, h: u32) -> Rgba {
        let px = (0..w * h).flat_map(|i| [(i * 7) as u8, (i * 13) as u8, (i * 29) as u8, 255]).collect();
        Rgba { w, h, px }
    }

    #[test]
    fn dib_round_trip() {
        let img = sample(5, 3); // odd width exercises row padding logic
        assert_eq!(dib_to_rgba(&rgba_to_dib(&img)).unwrap(), img);
    }

    #[test]
    fn parses_24bit_bottom_up_with_padding() {
        // 2x2: bottom row stored first; each 6-byte row padded to 8.
        let mut dib = Vec::new();
        for v in [40u32, 2, 2] {
            dib.extend_from_slice(&v.to_le_bytes());
        }
        dib.extend_from_slice(&1u16.to_le_bytes());
        dib.extend_from_slice(&24u16.to_le_bytes());
        dib.extend_from_slice(&[0; 24]);
        dib.extend_from_slice(&[0, 0, 255, 0, 255, 0, 0, 0]); // bottom: red, green (BGR)
        dib.extend_from_slice(&[255, 0, 0, 255, 255, 255, 0, 0]); // top: blue, white
        let img = dib_to_rgba(&dib).unwrap();
        assert_eq!(img.px, [0, 0, 255, 255, 255, 255, 255, 255, 255, 0, 0, 255, 0, 255, 0, 255]);
    }

    #[test]
    fn parses_top_down_and_v5_alpha_masks() {
        let mut dib = vec![0u8; 124];
        dib[0..4].copy_from_slice(&124u32.to_le_bytes());
        dib[4..8].copy_from_slice(&1i32.to_le_bytes());
        dib[8..12].copy_from_slice(&(-2i32).to_le_bytes()); // top-down
        dib[12..14].copy_from_slice(&1u16.to_le_bytes());
        dib[14..16].copy_from_slice(&32u16.to_le_bytes());
        dib[16..20].copy_from_slice(&BI_BITFIELDS.to_le_bytes());
        for (i, m) in [0x00ff_0000u32, 0x0000_ff00, 0x0000_00ff, 0xff00_0000].iter().enumerate() {
            dib[40 + i * 4..44 + i * 4].copy_from_slice(&m.to_le_bytes());
        }
        dib.extend_from_slice(&[10, 20, 30, 128]); // first row: B G R A
        dib.extend_from_slice(&[1, 2, 3, 255]);
        let img = dib_to_rgba(&dib).unwrap();
        assert_eq!(img.px, [30, 20, 10, 128, 3, 2, 1, 255]);
    }

    #[test]
    fn rejects_unsupported_dibs() {
        assert!(dib_to_rgba(&[]).is_none());
        let mut dib = rgba_to_dib(&sample(2, 2));
        dib[14..16].copy_from_slice(&8u16.to_le_bytes()); // paletted
        assert!(dib_to_rgba(&dib).is_none());
        let short = &rgba_to_dib(&sample(4, 4))[..60];
        assert!(dib_to_rgba(short).is_none());
    }

    #[test]
    fn png_round_trip() {
        let img = sample(17, 9);
        assert_eq!(decode_png(&encode_png(&img).unwrap()).unwrap(), img);
        assert!(decode_png(b"not a png").is_none());
    }

    #[test]
    fn thumbnail_keeps_aspect_and_never_upscales() {
        let t = thumbnail(&sample(400, 100), 64);
        assert_eq!((t.w, t.h), (64, 16));
        let small = sample(10, 20);
        assert_eq!(thumbnail(&small, 64), small);
    }

    #[test]
    fn hash_distinguishes_content_and_shape() {
        let a = sample(4, 4);
        let mut b = a.clone();
        b.px[5] ^= 1;
        let reshaped = Rgba { w: 8, h: 2, px: a.px.clone() };
        assert_eq!(content_hash(&a), content_hash(&a.clone()));
        assert_ne!(content_hash(&a), content_hash(&b));
        assert_ne!(content_hash(&a), content_hash(&reshaped));
    }
}
