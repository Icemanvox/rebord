//! Les curseurs Windows en RGBA : portable, donc testé partout.
//!
//! GetDIBits rend chaque bitmap en BGRA 32 bits, de haut en bas. Un curseur couleur a une image
//! couleur, avec ou sans alpha, et un masque ET ; un curseur monochrome n'a qu'un masque de double
//! hauteur : la moitié haute est le masque ET, la basse le masque OU exclusif.

/// Un pixel du masque est « allumé » s'il est blanc.
fn on(mask: &[u8], i: usize) -> bool {
    mask.get(i * 4).copied().unwrap_or(0) > 127
}

/// Compose l'image RGBA d'un curseur de `w`×`h` pixels.
pub fn compose(w: usize, h: usize, color: Option<&[u8]>, mask: &[u8]) -> Vec<u8> {
    let n = w * h;
    let mut out = vec![0u8; n * 4];
    match color {
        Some(c) => {
            let has_alpha = c.chunks_exact(4).take(n).any(|p| p[3] != 0);
            for i in 0..n {
                let (b, g, r, a) = (c[i * 4], c[i * 4 + 1], c[i * 4 + 2], c[i * 4 + 3]);
                let alpha = if has_alpha {
                    a
                } else if on(mask, i) {
                    // ET allumé : transparent, sauf une couleur non noire, qui inverse l'écran ;
                    // l'app ne sait pas inverser, on la montre opaque.
                    if r | g | b != 0 { 255 } else { 0 }
                } else {
                    255
                };
                out[i * 4..i * 4 + 4].copy_from_slice(&[r, g, b, alpha]);
            }
        }
        None => {
            // Monochrome (le I des champs de texte, souvent) : ET=0 OU=0 noir, ET=0 OU=1 blanc,
            // ET=1 OU=0 transparent, ET=1 OU=1 inversion, montrée en noir opaque pour rester
            // visible sur fond clair comme sombre... le noir l'est sur fond clair, le cas courant.
            for i in 0..n {
                let and = on(mask, i);
                let xor = on(mask, i + n);
                let px = match (and, xor) {
                    (false, false) => [0, 0, 0, 255],
                    (false, true) => [255, 255, 255, 255],
                    (true, false) => [0, 0, 0, 0],
                    (true, true) => [0, 0, 0, 255],
                };
                out[i * 4..i * 4 + 4].copy_from_slice(&px);
            }
        }
    }
    out
}

/// La plus petite boîte qui contient les pixels visibles, pour ne pas envoyer un carré de 32×32 ou
/// 48×48 presque vide : (x, y, largeur, hauteur). `None` si tout est transparent.
pub fn visible_box(w: usize, h: usize, rgba: &[u8]) -> Option<(usize, usize, usize, usize)> {
    let (mut x0, mut y0, mut x1, mut y1) = (usize::MAX, usize::MAX, 0, 0);
    for y in 0..h {
        for x in 0..w {
            if rgba[(y * w + x) * 4 + 3] != 0 {
                x0 = x0.min(x);
                y0 = y0.min(y);
                x1 = x1.max(x);
                y1 = y1.max(y);
            }
        }
    }
    (x0 != usize::MAX).then(|| (x0, y0, x1 - x0 + 1, y1 - y0 + 1))
}

pub fn crop(w: usize, rgba: &[u8], b: (usize, usize, usize, usize)) -> Vec<u8> {
    let (x, y, cw, ch) = b;
    let mut out = Vec::with_capacity(cw * ch * 4);
    for row in y..y + ch {
        out.extend_from_slice(&rgba[(row * w + x) * 4..(row * w + x + cw) * 4]);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn px(v: u8) -> [u8; 4] {
        [v, v, v, 0]
    }

    #[test]
    fn monochrome_cursor() {
        // 2×1 : ET = [0, 1], OU = [1, 0] → blanc opaque, transparent.
        let mask: Vec<u8> = [px(0), px(255), px(255), px(0)].concat();
        let out = compose(2, 1, None, &mask);
        assert_eq!(out, vec![255, 255, 255, 255, 0, 0, 0, 0]);
    }

    #[test]
    fn color_with_and_without_alpha() {
        // Avec alpha : il est gardé, BGRA → RGBA.
        let c = [10u8, 20, 30, 128, 0, 0, 0, 0];
        assert_eq!(compose(2, 1, Some(&c), &[]), vec![30, 20, 10, 128, 0, 0, 0, 0]);
        // Sans alpha : le masque décide.
        let c = [10u8, 20, 30, 0, 0, 0, 0, 0];
        let mask: Vec<u8> = [px(0), px(255)].concat();
        assert_eq!(compose(2, 1, Some(&c), &mask), vec![30, 20, 10, 255, 0, 0, 0, 0]);
    }

    #[test]
    fn box_and_crop() {
        let mut rgba = vec![0u8; 4 * 4 * 4];
        rgba[(4 + 1) * 4 + 3] = 255; // (1,1)
        rgba[(2 * 4 + 2) * 4 + 3] = 255; // (2,2)
        let b = visible_box(4, 4, &rgba).unwrap();
        assert_eq!(b, (1, 1, 2, 2));
        assert_eq!(crop(4, &rgba, b).len(), 2 * 2 * 4);
        assert!(visible_box(2, 2, &[0u8; 16]).is_none());
    }
}
