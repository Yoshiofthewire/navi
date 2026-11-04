//! PNG and JPEG decoding of untrusted bytes. The format comes from the magic bytes, and the size is
//! capped from the header before any pixel buffer is allocated. Shared by navi and navi-bar.

use std::io::Cursor;

pub const MAX_FILE: u64 = 64 << 20;
/// The largest cap a caller may ask for, and the wallpaper's.
pub const MAX_SIDE: u32 = 8192;
/// 8K: 7680 × 4320.
pub const MAX_AREA: u64 = 33_177_600;

pub struct Image {
    pub w: u32,
    pub h: u32,
    /// RGBA8, top row first.
    pub rgba: Vec<u8>,
}

/// Decodes an image of at most `max_side` px a side (and `MAX_SIDE`, and 8K's area).
pub fn decode(bytes: &[u8], max_side: u32) -> Result<Image, String> {
    if bytes.len() as u64 > MAX_FILE {
        return Err(format!("over {MAX_FILE} bytes"));
    }
    let max_side = max_side.min(MAX_SIDE);
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        png(bytes, max_side)
    } else if bytes.starts_with(&[0xff, 0xd8, 0xff]) {
        jpeg(bytes, max_side)
    } else {
        Err("unsupported format".into())
    }
}

fn max_area(side: u32) -> u64 {
    MAX_AREA.min(u64::from(side) * u64::from(side))
}

fn check(w: u32, h: u32, side: u32) -> Result<(), String> {
    let area = u64::from(w) * u64::from(h);
    let max = max_area(side);
    match (1..=side).contains(&w) && (1..=side).contains(&h) && area <= max {
        true => Ok(()),
        false => Err(format!(
            "{w}x{h} is outside the size cap ({side} px a side, {max} px)"
        )),
    }
}

fn png(bytes: &[u8], side: u32) -> Result<Image, String> {
    let err = |e: png::DecodingError| format!("png: {e}");
    let limits = png::Limits {
        bytes: (max_area(side) * 4) as usize,
    };
    let mut dec = png::Decoder::new_with_limits(Cursor::new(bytes), limits);
    dec.set_transformations(png::Transformations::normalize_to_color8());
    let (w, h) = dec.read_header_info().map_err(err)?.size();
    check(w, h, side)?;
    let mut reader = dec.read_info().map_err(err)?;
    let mut buf = vec![0; reader.output_buffer_size().ok_or("png: too large")?];
    let info = reader.next_frame(&mut buf).map_err(err)?;
    buf.truncate(info.buffer_size());
    use png::ColorType::*;
    let rgba = match info.color_type {
        Rgba => buf,
        Rgb => rgba(&buf, 3, |p| [p[0], p[1], p[2], 255]),
        GrayscaleAlpha => rgba(&buf, 2, |p| [p[0], p[0], p[0], p[1]]),
        Grayscale => rgba(&buf, 1, |p| [p[0], p[0], p[0], 255]),
        Indexed => return Err("png: palette not expanded".into()),
    };
    Ok(Image { w, h, rgba })
}

fn rgba(px: &[u8], n: usize, f: impl Fn(&[u8]) -> [u8; 4]) -> Vec<u8> {
    px.chunks_exact(n).flat_map(f).collect()
}

fn jpeg(bytes: &[u8], side: u32) -> Result<Image, String> {
    use zune_jpeg::zune_core::{bytestream::ZCursor, colorspace::ColorSpace, options};
    let opts = options::DecoderOptions::default()
        .set_max_width(side as usize)
        .set_max_height(side as usize)
        .jpeg_set_out_colorspace(ColorSpace::RGBA)
        // A truncated file is an error, not a half-grey image.
        .set_strict_mode(true);
    let mut dec = zune_jpeg::JpegDecoder::new_with_options(ZCursor::new(bytes), opts);
    let err = |e| format!("jpeg: {e:?}");
    dec.decode_headers().map_err(err)?;
    let info = dec.info().ok_or("jpeg: no size")?;
    let (w, h) = (u32::from(info.width), u32::from(info.height));
    check(w, h, side)?;
    let rgba = dec.decode().map_err(err)?;
    if rgba.len() as u64 != u64::from(w) * u64::from(h) * 4 {
        return Err("jpeg: short output".into());
    }
    Ok(Image { w, h, rgba })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn decode_max(bytes: &[u8]) -> Result<Image, String> {
        decode(bytes, MAX_SIDE)
    }

    fn encode(w: u32, h: u32, color: png::ColorType, depth: png::BitDepth, px: &[u8]) -> Vec<u8> {
        encode_with(w, h, color, depth, |_| {}, px)
    }

    fn encode_with(
        w: u32,
        h: u32,
        color: png::ColorType,
        depth: png::BitDepth,
        setup: impl FnOnce(&mut png::Encoder<&mut Vec<u8>>),
        px: &[u8],
    ) -> Vec<u8> {
        let mut out = Vec::new();
        let mut enc = png::Encoder::new(&mut out, w, h);
        enc.set_color(color);
        enc.set_depth(depth);
        setup(&mut enc);
        let mut writer = enc.write_header().unwrap();
        if !px.is_empty() {
            writer.write_image_data(px).unwrap();
        }
        drop(writer);
        out
    }

    /// A PNG with only the header chunk, claiming `w`×`h`.
    fn png_header(w: u32, h: u32) -> Vec<u8> {
        encode(w, h, png::ColorType::Rgb, png::BitDepth::Eight, &[])
    }

    const JPEG: &[u8] = include_bytes!("../testdata/red-64x48.jpg");

    /// The test JPEG with its frame header claiming `w`×`h`.
    fn jpeg_claiming(w: u16, h: u16) -> Vec<u8> {
        let mut j = JPEG.to_vec();
        let sof = j.windows(2).position(|m| m == [0xff, 0xc0]).unwrap();
        j[sof + 5..sof + 7].copy_from_slice(&h.to_be_bytes());
        j[sof + 7..sof + 9].copy_from_slice(&w.to_be_bytes());
        j
    }

    #[test]
    fn rgb_palette_and_16_bit_png_decode_to_rgba() {
        use png::{BitDepth::*, ColorType::*};
        let rgb = encode(2, 1, Rgb, Eight, &[1, 2, 3, 4, 5, 6]);
        let img = decode_max(&rgb).unwrap();
        assert_eq!((img.w, img.h), (2, 1));
        assert_eq!(img.rgba, [1, 2, 3, 255, 4, 5, 6, 255]);

        let palette = encode_with(
            2,
            1,
            Indexed,
            Eight,
            |e| {
                e.set_palette(vec![10, 20, 30, 40, 50, 60]);
                e.set_trns(vec![255, 128]);
            },
            &[1, 0],
        );
        assert_eq!(
            decode_max(&palette).unwrap().rgba,
            [40, 50, 60, 128, 10, 20, 30, 255]
        );

        // 16-bit samples keep their high byte.
        let deep = encode(1, 1, Rgba, Sixteen, &[0xab, 1, 0xcd, 2, 0xef, 3, 0x80, 4]);
        assert_eq!(decode_max(&deep).unwrap().rgba, [0xab, 0xcd, 0xef, 0x80]);

        let gray = encode(1, 1, Grayscale, Eight, &[77]);
        assert_eq!(decode_max(&gray).unwrap().rgba, [77, 77, 77, 255]);
    }

    #[test]
    fn jpeg_decodes_to_its_size_and_colour() {
        assert!(JPEG.len() < 4096);
        let img = decode_max(JPEG).unwrap();
        assert_eq!((img.w, img.h), (64, 48));
        assert_eq!(img.rgba.len(), 64 * 48 * 4);
        // Made with `magick -size 64x48 xc:'rgb(200,50,30)' -quality 90`.
        for px in img.rgba.as_chunks::<4>().0 {
            let near = |a: u8, b: u8| a.abs_diff(b) <= 8;
            assert!(
                near(px[0], 200) && near(px[1], 50) && near(px[2], 30),
                "{px:?}"
            );
            assert_eq!(px[3], 255);
        }
    }

    #[test]
    fn rejects_without_panic() {
        let err = |b: &[u8]| decode_max(b).err().expect("rejected");
        assert_eq!(err(b""), "unsupported format");
        assert_eq!(err(b"GIF89a......"), "unsupported format");
        assert_eq!(err(b"\x89PNG\r\n\x1a"), "unsupported format");

        let png = encode(4, 4, png::ColorType::Rgb, png::BitDepth::Eight, &[9; 48]);
        let e = err(&png[..png.len() / 2]);
        assert!(e.starts_with("png:"), "{e}");
        // Cut inside the headers, and two bytes into the entropy-coded data.
        let sos = JPEG.windows(2).position(|m| m == [0xff, 0xda]).unwrap();
        let data = sos + 2 + usize::from(u16::from_be_bytes([JPEG[sos + 2], JPEG[sos + 3]]));
        for cut in [0xa0, data + 2] {
            let e = err(&JPEG[..cut]);
            assert!(e.starts_with("jpeg:"), "{cut}: {e}");
        }

        let big = vec![0; MAX_FILE as usize + 1];
        assert_eq!(err(&big), format!("over {MAX_FILE} bytes"));
    }

    #[test]
    fn size_is_capped_from_the_header() {
        let cap = |e: String| assert!(e.contains("outside the size cap"), "{e}");
        for (w, h) in [(100_000, 100_000), (8193, 1), (1, 8193), (8192, 8192)] {
            cap(decode_max(&png_header(w, h)).err().unwrap());
        }
        // A JPEG header can't claim more than 65535 a side. Over 8192, zune-jpeg's own limit,
        // set from MAX_SIDE, refuses it while parsing the header.
        for (w, h) in [(65535, 65535), (8193, 1), (1, 8193)] {
            let e = decode_max(&jpeg_claiming(w, h)).err().unwrap();
            assert!(e.contains("limit 8192"), "{e}");
        }
        cap(decode_max(&jpeg_claiming(8192, 8192)).err().unwrap());
        cap(decode_max(&jpeg_claiming(7681, 4320)).err().unwrap());
        assert_eq!(check(7680, 4320, MAX_SIDE), Ok(()));
        assert_eq!(check(8192, 4050, MAX_SIDE), Ok(()));
        // A smaller cap, as icons use; never above MAX_SIDE.
        let e = decode(&png_header(513, 1), 512).err().unwrap();
        assert_eq!(
            e,
            "513x1 is outside the size cap (512 px a side, 262144 px)"
        );
        cap(decode(&png_header(8193, 1), 99_999).err().unwrap());
        let one = encode(1, 1, png::ColorType::Rgb, png::BitDepth::Eight, &[1, 2, 3]);
        assert!(decode(&one, 512).is_ok());
    }
}
