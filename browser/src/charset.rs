//! Text encodings: bytes from the network to a Rust string, the way a browser
//! picks the encoding (BOM, then the Content-Type charset, then for HTML a
//! `<meta charset>` in the first bytes, else UTF-8).

/// An encoding this browser decodes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Encoding {
    Utf8,
    Utf16Le,
    Utf16Be,
    /// windows-1251 (Cyrillic)
    Windows1251,
    /// KOI8-R (Cyrillic)
    Koi8R,
    /// KOI8-U (Ukrainian; KOI8-R with a few letters changed)
    Koi8U,
    /// windows-1252, which is also what the labels latin1 / iso-8859-1 mean on the web
    Windows1252,
    /// ISO-8859-5 (Cyrillic)
    Iso8859_5,
}

impl Encoding {
    /// The encoding a label names (WHATWG Encoding Standard labels).
    pub fn for_label(label: &str) -> Option<Encoding> {
        let l = label.trim().trim_matches(|c| c == '"' || c == '\'').to_ascii_lowercase();
        Some(match l.as_str() {
            "utf-8" | "utf8" | "unicode-1-1-utf-8" | "unicode11utf8" | "unicode20utf8" | "x-unicode20utf8" => Encoding::Utf8,
            "utf-16le" | "utf-16" | "ucs-2" | "unicode" | "unicodefeff" | "csunicode" | "iso-10646-ucs-2" => Encoding::Utf16Le,
            "utf-16be" | "unicodefffe" => Encoding::Utf16Be,
            "windows-1251" | "cp1251" | "x-cp1251" => Encoding::Windows1251,
            "koi8-r" | "koi8_r" | "koi8" | "koi" | "cskoi8r" => Encoding::Koi8R,
            "koi8-u" | "koi8-ru" => Encoding::Koi8U,
            "iso-8859-5" | "iso8859-5" | "iso_8859-5" | "cyrillic" | "csisolatincyrillic" | "iso-ir-144" => Encoding::Iso8859_5,
            "windows-1252" | "cp1252" | "x-cp1252" | "iso-8859-1" | "iso8859-1" | "iso_8859-1" | "latin1" | "l1" | "ascii"
            | "us-ascii" | "cp819" | "ibm819" | "iso-ir-100" | "csisolatin1" | "ansi_x3.4-1968" => Encoding::Windows1252,
            _ => return None,
        })
    }

    /// Decode `bytes` (no BOM handling here; see `decode`).
    pub fn decode_bytes(self, bytes: &[u8]) -> String {
        match self {
            Encoding::Utf8 => String::from_utf8_lossy(bytes).into_owned(),
            Encoding::Utf16Le | Encoding::Utf16Be => {
                let units: Vec<u16> = bytes
                    .chunks_exact(2)
                    .map(|p| if self == Encoding::Utf16Le { u16::from_le_bytes([p[0], p[1]]) } else { u16::from_be_bytes([p[0], p[1]]) })
                    .collect();
                String::from_utf16_lossy(&units)
            }
            single => bytes.iter().map(|&b| single_byte(single, b)).collect(),
        }
    }
}

/// The character a byte stands for in a single-byte encoding.
fn single_byte(e: Encoding, b: u8) -> char {
    if b < 0x80 {
        return b as char;
    }
    let cp: u32 = match e {
        Encoding::Windows1251 => match b {
            0xC0..=0xFF => 0x0410 + (b - 0xC0) as u32,
            _ => WINDOWS_1251[(b - 0x80) as usize] as u32,
        },
        Encoding::Koi8R | Encoding::Koi8U => {
            if b >= 0xC0 {
                let lower = KOI8_LETTERS[(b & 0x1F) as usize] as u32;
                if b >= 0xE0 { lower - 0x20 } else { lower }
            } else {
                let mut c = KOI8_R_HIGH[(b - 0x80) as usize] as u32;
                if e == Encoding::Koi8U {
                    c = match b {
                        0xA4 => 0x0454, 0xA6 => 0x0456, 0xA7 => 0x0457, 0xAD => 0x0491,
                        0xB4 => 0x0404, 0xB6 => 0x0406, 0xB7 => 0x0407, 0xBD => 0x0490,
                        _ => c,
                    };
                }
                c
            }
        }
        Encoding::Iso8859_5 => match b {
            0xA0 => 0x00A0,
            0xAD => 0x00AD,
            0xF0 => 0x2116,
            0xFD => 0x00A7,
            0xA1..=0xFF => 0x0401 + (b - 0xA1) as u32,
            _ => b as u32,
        },
        _ => match b {
            0x80..=0x9F => WINDOWS_1252_C1[(b - 0x80) as usize] as u32,
            _ => b as u32,
        },
    };
    char::from_u32(cp).unwrap_or('\u{FFFD}')
}

/// windows-1251 bytes 0x80..0xBF
const WINDOWS_1251: [u16; 64] = [
    0x0402, 0x0403, 0x201A, 0x0453, 0x201E, 0x2026, 0x2020, 0x2021, 0x20AC, 0x2030, 0x0409, 0x2039, 0x040A, 0x040C, 0x040B, 0x040F,
    0x0452, 0x2018, 0x2019, 0x201C, 0x201D, 0x2022, 0x2013, 0x2014, 0x0098, 0x2122, 0x0459, 0x203A, 0x045A, 0x045C, 0x045B, 0x045F,
    0x00A0, 0x040E, 0x045E, 0x0408, 0x00A4, 0x0490, 0x00A6, 0x00A7, 0x0401, 0x00A9, 0x0404, 0x00AB, 0x00AC, 0x00AD, 0x00AE, 0x0407,
    0x00B0, 0x00B1, 0x0406, 0x0456, 0x0491, 0x00B5, 0x00B6, 0x00B7, 0x0451, 0x2116, 0x0454, 0x00BB, 0x0458, 0x0405, 0x0455, 0x0457,
];

/// KOI8-R bytes 0x80..0xBF (box drawing and a few symbols; Ё/ё at 0xB3/0xA3)
const KOI8_R_HIGH: [u16; 64] = [
    0x2500, 0x2502, 0x250C, 0x2510, 0x2514, 0x2518, 0x251C, 0x2524, 0x252C, 0x2534, 0x253C, 0x2580, 0x2584, 0x2588, 0x258C, 0x2590,
    0x2591, 0x2592, 0x2593, 0x2320, 0x25A0, 0x2219, 0x221A, 0x2248, 0x2264, 0x2265, 0x00A0, 0x2321, 0x00B0, 0x00B2, 0x00B7, 0x00F7,
    0x2550, 0x2551, 0x2552, 0x0451, 0x2553, 0x2554, 0x2555, 0x2556, 0x2557, 0x2558, 0x2559, 0x255A, 0x255B, 0x255C, 0x255D, 0x255E,
    0x255F, 0x2560, 0x2561, 0x0401, 0x2562, 0x2563, 0x2564, 0x2565, 0x2566, 0x2567, 0x2568, 0x2569, 0x256A, 0x256B, 0x256C, 0x00A9,
];

/// KOI8 letters 0xC0..0xDF (lower case; 0xE0..0xFF are the same letters upper case)
const KOI8_LETTERS: [u16; 32] = [
    0x044E, 0x0430, 0x0431, 0x0446, 0x0434, 0x0435, 0x0444, 0x0433, 0x0445, 0x0438, 0x0439, 0x043A, 0x043B, 0x043C, 0x043D, 0x043E,
    0x043F, 0x044F, 0x0440, 0x0441, 0x0442, 0x0443, 0x0436, 0x0432, 0x044C, 0x044B, 0x0437, 0x0448, 0x044D, 0x0449, 0x0447, 0x044A,
];

/// windows-1252 bytes 0x80..0x9F (the rest is Latin-1)
const WINDOWS_1252_C1: [u16; 32] = [
    0x20AC, 0x0081, 0x201A, 0x0192, 0x201E, 0x2026, 0x2020, 0x2021, 0x02C6, 0x2030, 0x0160, 0x2039, 0x0152, 0x008D, 0x017D, 0x008F,
    0x0090, 0x2018, 0x2019, 0x201C, 0x201D, 0x2022, 0x2013, 0x2014, 0x02DC, 0x2122, 0x0161, 0x203A, 0x0153, 0x009D, 0x017E, 0x0178,
];

/// The `charset=` of a Content-Type value.
pub fn charset_param(content_type: &str) -> Option<Encoding> {
    content_type.split(';').skip(1).find_map(|p| {
        let (k, v) = p.split_once('=')?;
        k.trim().eq_ignore_ascii_case("charset").then(|| Encoding::for_label(v)).flatten()
    })
}

/// A `<meta charset>` / `<meta http-equiv=content-type content="...charset=...">`
/// in the first 1024 bytes of an HTML document (the prescan, simplified).
pub fn html_meta_charset(bytes: &[u8]) -> Option<Encoding> {
    let head = String::from_utf8_lossy(&bytes[..bytes.len().min(1024)]).to_ascii_lowercase();
    let mut rest = head.as_str();
    while let Some(i) = rest.find("<meta") {
        rest = &rest[i + 5..];
        let tag = &rest[..rest.find('>').unwrap_or(rest.len())];
        if let Some(j) = tag.find("charset") {
            let after = tag[j + 7..].trim_start();
            if let Some(v) = after.strip_prefix('=') {
                let v = v.trim_start().trim_start_matches(['"', '\'']);
                let end = v.find(|c: char| c == '"' || c == '\'' || c == ';' || c == '/' || c.is_whitespace()).unwrap_or(v.len());
                if let Some(e) = Encoding::for_label(&v[..end]) {
                    // a page that says UTF-16 in ASCII bytes is not UTF-16
                    return Some(if matches!(e, Encoding::Utf16Le | Encoding::Utf16Be) { Encoding::Utf8 } else { e });
                }
            }
        }
    }
    None
}

/// Decode a response body: BOM, then the Content-Type charset, then (for
/// HTML) the meta prescan, then `fallback`.
pub fn decode(bytes: &[u8], content_type: &str, html: bool, fallback: Encoding) -> (String, Encoding) {
    if let Some(rest) = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]) {
        return (Encoding::Utf8.decode_bytes(rest), Encoding::Utf8);
    }
    if let Some(rest) = bytes.strip_prefix(&[0xFF, 0xFE]) {
        return (Encoding::Utf16Le.decode_bytes(rest), Encoding::Utf16Le);
    }
    if let Some(rest) = bytes.strip_prefix(&[0xFE, 0xFF]) {
        return (Encoding::Utf16Be.decode_bytes(rest), Encoding::Utf16Be);
    }
    let e = charset_param(content_type)
        .or_else(|| if html { html_meta_charset(bytes) } else { None })
        .unwrap_or(fallback);
    (e.decode_bytes(bytes), e)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cyrillic_single_byte_encodings() {
        // "Привет, Ёж №1" in each
        let w1251 = [0xCF, 0xF0, 0xE8, 0xE2, 0xE5, 0xF2, 0x2C, 0x20, 0xA8, 0xE6, 0x20, 0xB9, 0x31];
        assert_eq!(Encoding::Windows1251.decode_bytes(&w1251), "Привет, Ёж №1");
        let koi8 = [0xF0, 0xD2, 0xC9, 0xD7, 0xC5, 0xD4, 0x2C, 0x20, 0xB3, 0xD6];
        assert_eq!(Encoding::Koi8R.decode_bytes(&koi8), "Привет, Ёж");
        assert_eq!(Encoding::Windows1252.decode_bytes(&[0x80, 0xE9, 0x93]), "€é“");
        assert_eq!(Encoding::Iso8859_5.decode_bytes(&[0xBF, 0xE0, 0xD8]), "При");
    }

    #[test]
    fn picks_the_encoding_like_a_browser() {
        let w1251 = [0xCF, 0xF0, 0xE8];
        assert_eq!(decode(&w1251, "text/html; charset=windows-1251", true, Encoding::Utf8).0, "При");
        assert_eq!(decode(&w1251, "application/json; charset=\"cp1251\"", false, Encoding::Utf8).0, "При");
        let mut page = b"<html><head><meta http-equiv=\"Content-Type\" content=\"text/html; charset=windows-1251\"></head>".to_vec();
        page.extend_from_slice(&w1251);
        assert!(decode(&page, "text/html", true, Encoding::Utf8).0.ends_with("При"));
        let page2 = b"<meta charset='koi8-r'>\xF0".to_vec();
        assert!(decode(&page2, "text/html", true, Encoding::Utf8).0.ends_with('П'));
        // the BOM wins over everything
        assert_eq!(decode(&[0xEF, 0xBB, 0xBF, 0xD0, 0x9F], "text/html; charset=windows-1251", true, Encoding::Utf8).0, "П");
        // nothing said: UTF-8
        assert_eq!(decode("Пр".as_bytes(), "text/html", true, Encoding::Utf8).0, "Пр");
    }
}
