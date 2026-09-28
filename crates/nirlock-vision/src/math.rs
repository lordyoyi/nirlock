//! Small vector helpers shared by the embedders and the template matcher.

/// L2-normalises `v` in place. A zero vector is left untouched (same guard
/// as `l2_normalise` in `phase0/pipeline.cpp`: `if (n > 1e-12) row /= n`).
pub fn l2_normalise(v: &mut [f32]) {
    let n = v
        .iter()
        .map(|x| (*x as f64) * (*x as f64))
        .sum::<f64>()
        .sqrt();
    if n > 1e-12 {
        let inv = (1.0 / n) as f32;
        for x in v.iter_mut() {
            *x *= inv;
        }
    }
}

/// Dot product in f64 accumulation. For unit vectors this is the cosine
/// similarity; callers pass already-normalised rows (as fuprobe does).
pub fn dot(a: &[f32], b: &[f32]) -> f64 {
    debug_assert_eq!(a.len(), b.len());
    a.iter()
        .zip(b)
        .map(|(x, y)| (*x as f64) * (*y as f64))
        .sum()
}

/// Cosine similarity of two arbitrary vectors (normalises both sides).
pub fn cosine(a: &[f32], b: &[f32]) -> f64 {
    let na = dot(a, a).sqrt();
    let nb = dot(b, b).sqrt();
    if na <= 1e-12 || nb <= 1e-12 {
        return 0.0;
    }
    dot(a, b) / (na * nb)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalise_and_cosine() {
        let mut v = vec![3.0f32, 4.0];
        l2_normalise(&mut v);
        assert!((v[0] - 0.6).abs() < 1e-6 && (v[1] - 0.8).abs() < 1e-6);
        assert!((cosine(&[1.0, 0.0], &[1.0, 0.0]) - 1.0).abs() < 1e-12);
        assert!(cosine(&[1.0, 0.0], &[0.0, 1.0]).abs() < 1e-12);
        assert!((cosine(&[1.0, 0.0], &[-1.0, 0.0]) + 1.0).abs() < 1e-12);
        let mut z = vec![0.0f32; 4];
        l2_normalise(&mut z);
        assert!(z.iter().all(|x| *x == 0.0));
        assert_eq!(cosine(&z, &[1.0, 2.0, 3.0, 4.0]), 0.0);
    }
}
