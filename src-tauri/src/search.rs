//! Brute-force vector math (vectors are L2-normalized, so dot product = cosine).

pub fn normalize(v: &mut [f32]) {
    let n = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if n > 0.0 {
        v.iter_mut().for_each(|x| *x /= n);
    }
}

#[inline]
pub fn dot(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b).map(|(x, y)| x * y).sum()
}

pub fn to_blob(v: &[f32]) -> Vec<u8> {
    v.iter().flat_map(|x| x.to_le_bytes()).collect()
}

pub fn from_blob(b: &[u8]) -> Vec<f32> {
    b.chunks_exact(4).map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]])).collect()
}

/// Score every candidate, drop those under `threshold`, keep the best `k` (desc).
pub fn top_k<'a, I>(query: &[f32], candidates: I, k: usize, threshold: f32) -> Vec<(i64, f32)>
where
    I: IntoIterator<Item = (i64, &'a [f32])>,
{
    let mut scored: Vec<(i64, f32)> = candidates
        .into_iter()
        .filter(|(_, v)| v.len() == query.len())
        .map(|(id, v)| (id, dot(query, v)))
        .filter(|(_, s)| *s >= threshold)
        .collect();
    let k = k.min(scored.len());
    if k == 0 {
        return Vec::new();
    }
    scored.select_nth_unstable_by(k - 1, |a, b| b.1.total_cmp(&a.1));
    scored.truncate(k);
    scored.sort_by(|a, b| b.1.total_cmp(&a.1));
    scored
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unit(v: &[f32]) -> Vec<f32> {
        let mut v = v.to_vec();
        normalize(&mut v);
        v
    }

    #[test]
    fn blob_roundtrip() {
        let v = vec![0.25f32, -1.5, 3.0];
        assert_eq!(from_blob(&to_blob(&v)), v);
    }

    #[test]
    fn finds_nearest_in_order() {
        let q = unit(&[1.0, 0.0, 0.0]);
        let data = vec![
            (1, unit(&[0.0, 1.0, 0.0])),
            (2, unit(&[1.0, 0.1, 0.0])),
            (3, unit(&[1.0, 1.0, 0.0])),
            (4, unit(&[-1.0, 0.0, 0.0])),
        ];
        let r = top_k(&q, data.iter().map(|(i, v)| (*i, v.as_slice())), 2, 0.0);
        assert_eq!(r.iter().map(|x| x.0).collect::<Vec<_>>(), vec![2, 3]);
        assert!(r[0].1 > r[1].1);
    }

    #[test]
    fn threshold_and_dims() {
        let q = unit(&[1.0, 0.0]);
        let data = vec![(1, unit(&[0.0, 1.0])), (2, vec![1.0, 0.0, 0.0])];
        let r = top_k(&q, data.iter().map(|(i, v)| (*i, v.as_slice())), 5, 0.5);
        assert!(r.is_empty());
    }
}
