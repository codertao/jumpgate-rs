//! The stock client's collision geometry, rebuilt the way the client builds it
//! (physics.md §6 "Test primitives"):
//!
//! - triangles come from the model's **render meshes** (the `.x` file, plus
//!   a station's `_tun.x` tunnel), read at runtime from the user's own client
//!   folder. Nothing from the install is copied or committed;
//! - they are filtered by the **hand-authored volumes** hard-coded in the exe
//!   (`extracts/collision_volumes.csv`): `build_collision_tris` (`0xa612d0`);
//! - every test is a swept segment p → p + d against those triangles, gated by
//!   the volumes: `collide_segment_vs_model` (`0x409a40`) with the one-sided
//!   `collide_segment_triangle` (`0x407430`). The nearest hit wins.
//!
//! Models are placed like a contact slot: world = pos + x·right + y·up + z·fwd.

use crate::ship::{Vec3, normalize};
use std::collections::HashMap;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

fn sub(a: Vec3, b: Vec3) -> Vec3 {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}
fn add(a: Vec3, b: Vec3) -> Vec3 {
    [a[0] + b[0], a[1] + b[1], a[2] + b[2]]
}
fn scale(a: Vec3, k: f32) -> Vec3 {
    [a[0] * k, a[1] * k, a[2] * k]
}
fn dot(a: Vec3, b: Vec3) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}
fn cross(a: Vec3, b: Vec3) -> Vec3 {
    [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]]
}

// ---- volumes ----

/// One 0x40-byte volume block: an oriented ellipsoid in model space.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Volume {
    /// Axis A (+0x00), axis B (+0x0c), and A × B.
    pub axes: [Vec3; 3],
    /// Full extents along the three axes (+0x18): diameters.
    pub ext: Vec3,
    pub centre: Vec3,
}

impl Volume {
    pub fn new(a: Vec3, b: Vec3, ext: Vec3, centre: Vec3) -> Volume {
        Volume { axes: [a, b, cross(a, b)], ext, centre }
    }

    /// A sub-volume with `ext.x == 0` is skipped by the triangle builder.
    pub fn is_empty(&self) -> bool {
        self.ext[0] == 0.0
    }

    /// `point_in_volume` (`0x408130`): Σ 4·(dᵢ/extᵢ)² ≤ 1.
    pub fn contains(&self, p: Vec3) -> bool {
        let d = sub(p, self.centre);
        let mut s = 0.0;
        for k in 0..3 {
            let q = dot(d, self.axes[k]) / self.ext[k];
            s += 4.0 * q * q;
        }
        s <= 1.0
    }

    /// Does the segment p → p + d touch the ellipsoid (model space)? The
    /// client's `collide_bounds` (`0x407b00`) gates with this volume before
    /// testing triangles; this exact segment test passes everything it does.
    pub fn touches_segment(&self, p: Vec3, d: Vec3) -> bool {
        // Into the frame where the ellipsoid is the unit sphere.
        let mut q = [0.0; 3];
        let mut e = [0.0; 3];
        let rel = sub(p, self.centre);
        for k in 0..3 {
            let half = self.ext[k] * 0.5;
            if half <= 0.0 {
                return false;
            }
            q[k] = dot(rel, self.axes[k]) / half;
            e[k] = dot(d, self.axes[k]) / half;
        }
        // Closest point on the segment to the origin.
        let ee = dot(e, e);
        let t = if ee > 0.0 { (-dot(q, e) / ee).clamp(0.0, 1.0) } else { 0.0 };
        let c = add(q, scale(e, t));
        dot(c, c) <= 1.0
    }
}

// ---- triangles ----

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Tri(pub [Vec3; 3]);

/// `collide_segment_triangle` (`0x407430`): one-sided. The winding defines the
/// front, `n = normalize((v1 − v0) × (v2 − v1))`; the segment must start in
/// front (or on) and end behind (or on). Returns the hit parameter t ∈ [0, 1].
pub fn segment_triangle(p: Vec3, d: Vec3, t: &Tri) -> Option<f32> {
    let [v0, v1, v2] = t.0;
    let n = cross(sub(v1, v0), sub(v2, v1));
    if dot(n, n) == 0.0 {
        return None;
    }
    let n = normalize(n);
    let a = dot(n, sub(p, v0));
    if a < 0.0 {
        return None;
    }
    let b = dot(n, sub(add(p, d), v0));
    if b > 0.0 {
        return None;
    }
    let nd = dot(n, d);
    if nd == 0.0 {
        return None;
    }
    let s = -a / nd;
    let hit = add(p, scale(d, s));
    for (vi, vj) in [(v0, v1), (v1, v2), (v2, v0)] {
        if dot(cross(sub(vj, vi), sub(hit, vi)), n) < 0.0 {
            return None;
        }
    }
    Some(s)
}

// ---- models ----

/// A model's collision geometry: the whole-model volume, and either one
/// triangle list or one per non-empty sub-volume.
#[derive(Clone, Debug, Default)]
pub struct Model {
    pub whole: Option<Volume>,
    /// Used when there are no sub-volumes.
    pub tris: Vec<Tri>,
    pub subs: Vec<(Volume, Vec<Tri>)>,
}

impl Model {
    /// `build_collision_tris` (`0xa612d0`): if sub-volume 1 is empty, keep each
    /// triangle with at least one vertex inside the whole-model volume;
    /// otherwise file it under every non-empty sub-volume that holds one of
    /// its vertices. Triangles outside every volume are dropped.
    pub fn build(volumes: &[(u8, Volume)], tris: &[Tri]) -> Model {
        let whole = volumes.iter().find(|(k, _)| *k == 0).map(|(_, v)| *v);
        let mut subs: Vec<(Volume, Vec<Tri>)> = Vec::new();
        for k in 1..=20u8 {
            if let Some((_, v)) = volumes.iter().find(|(b, _)| *b == k) {
                if !v.is_empty() {
                    subs.push((*v, Vec::new()));
                }
            }
        }
        let sub1_empty = !volumes.iter().any(|(k, v)| *k == 1 && !v.is_empty());
        let mut m = Model { whole, tris: Vec::new(), subs: Vec::new() };
        let Some(w) = whole else { return m };
        if sub1_empty {
            m.tris = tris.iter().filter(|t| t.0.iter().any(|&v| w.contains(v))).copied().collect();
        } else {
            for t in tris {
                for (v, list) in subs.iter_mut() {
                    if t.0.iter().any(|&p| v.contains(p)) {
                        list.push(*t);
                    }
                }
            }
            m.subs = subs;
        }
        m
    }

    pub fn triangle_count(&self) -> usize {
        self.tris.len() + self.subs.iter().map(|(_, l)| l.len()).sum::<usize>()
    }

    /// `collide_segment_vs_model` (`0x409a40`) in model space: the nearest hit
    /// parameter over all triangles whose volume the segment touches.
    pub fn sweep(&self, p: Vec3, d: Vec3) -> Option<f32> {
        let w = self.whole?;
        if !w.touches_segment(p, d) {
            return None;
        }
        let best = |acc: Option<f32>, t: Option<f32>| match (acc, t) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        };
        let mut hit = None;
        for t in &self.tris {
            hit = best(hit, segment_triangle(p, d, t));
        }
        for (v, list) in &self.subs {
            if v.touches_segment(p, d) {
                for t in list {
                    hit = best(hit, segment_triangle(p, d, t));
                }
            }
        }
        hit
    }
}

/// Where a model sits: position and orthonormal axes (right = up × fwd, the
/// convention checked against the captured launches, `Station::to_world`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Pose {
    pub pos: Vec3,
    pub right: Vec3,
    pub up: Vec3,
    pub fwd: Vec3,
}

impl Pose {
    pub fn new(pos: Vec3, fwd: Vec3, up: Vec3) -> Pose {
        Pose { pos, right: cross(up, fwd), up, fwd }
    }
    pub fn at(pos: Vec3) -> Pose {
        Pose::new(pos, [0.0, 0.0, 1.0], [0.0, 1.0, 0.0])
    }
    pub fn to_model(&self, p: Vec3) -> Vec3 {
        let r = sub(p, self.pos);
        [dot(r, self.right), dot(r, self.up), dot(r, self.fwd)]
    }
    pub fn dir_to_model(&self, d: Vec3) -> Vec3 {
        [dot(d, self.right), dot(d, self.up), dot(d, self.fwd)]
    }
    pub fn to_world(&self, p: Vec3) -> Vec3 {
        add(self.pos, add(scale(self.right, p[0]), add(scale(self.up, p[1]), scale(self.fwd, p[2]))))
    }
}

/// A hit: segment parameter and contact point in world coordinates.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Contact {
    pub t: f32,
    pub point: Vec3,
}

/// Sweep a world-space segment against a placed model.
pub fn sweep(model: &Model, pose: &Pose, p: Vec3, d: Vec3) -> Option<Contact> {
    let t = model.sweep(pose.to_model(p), pose.dir_to_model(d))?;
    Some(Contact { t, point: add(p, scale(d, t)) })
}

/// Collision models by model id: volumes from the extract, meshes loaded
/// lazily from the client folder and cached. Without a client folder (or for
/// a model with no volume or no file) a model has no triangles and nothing
/// collides with it, which is also what the client does for models it has
/// no volume for.
pub struct Models {
    client_dir: Option<PathBuf>,
    /// Lower-cased file name → actual name in the client folder.
    listing: HashMap<String, String>,
    files: HashMap<u16, (String, Option<String>)>,
    volumes: HashMap<u16, Vec<(u8, Volume)>>,
    cache: Mutex<HashMap<u16, Arc<Model>>>,
}

impl Models {
    /// `extracts`: the folder with models.csv and collision_volumes.csv.
    /// `client_dir`: the user's Jumpgate install (the `.x` files), if any.
    pub fn load(extracts: &Path, client_dir: Option<&Path>) -> io::Result<Models> {
        let mut files = HashMap::new();
        if let Ok(t) = std::fs::read_to_string(extracts.join("models.csv")) {
            for row in t.lines().skip(1) {
                let f: Vec<&str> = row.split(',').collect();
                if let (Some(id), Some(file)) = (f.first().and_then(|v| v.parse().ok()), f.get(1)) {
                    let tun = f.get(2).map(|s| s.trim()).filter(|s| !s.is_empty()).map(str::to_string);
                    files.insert(id, (file.trim().to_string(), tun));
                }
            }
        }
        let mut volumes: HashMap<u16, Vec<(u8, Volume)>> = HashMap::new();
        if let Ok(t) = std::fs::read_to_string(extracts.join("collision_volumes.csv")) {
            for row in t.lines().skip(1) {
                let f: Vec<f32> = row.split(',').filter_map(|v| v.trim().parse().ok()).collect();
                if f.len() == 14 {
                    let v3 = |i: usize| [f[i], f[i + 1], f[i + 2]];
                    let vol = Volume::new(v3(2), v3(5), v3(8), v3(11));
                    volumes.entry(f[0] as u16).or_default().push((f[1] as u8, vol));
                }
            }
        }
        let mut listing = HashMap::new();
        if let Some(dir) = client_dir {
            for e in std::fs::read_dir(dir)? {
                let name = e?.file_name().to_string_lossy().into_owned();
                listing.insert(name.to_lowercase(), name);
            }
        }
        Ok(Models { client_dir: client_dir.map(Path::to_path_buf), listing, files, volumes, cache: Mutex::new(HashMap::new()) })
    }

    /// Volumes only, no meshes (tests, or no client folder).
    pub fn has_meshes(&self) -> bool {
        self.client_dir.is_some()
    }

    pub fn volumes(&self, model: u16) -> &[(u8, Volume)] {
        self.volumes.get(&model).map(Vec::as_slice).unwrap_or(&[])
    }

    /// The model's collision geometry (empty if unknown or unreadable).
    pub fn get(&self, model: u16) -> Arc<Model> {
        if let Some(m) = self.cache.lock().unwrap().get(&model) {
            return m.clone();
        }
        let m = Arc::new(self.build(model).unwrap_or_default());
        self.cache.lock().unwrap().insert(model, m.clone());
        m
    }

    fn build(&self, model: u16) -> Option<Model> {
        let vols = self.volumes.get(&model)?;
        let dir = self.client_dir.as_ref()?;
        let (file, tun) = self.files.get(&model)?;
        let mut tris = Vec::new();
        for name in std::iter::once(file).chain(tun.iter()) {
            let real = self.listing.get(&name.to_lowercase())?;
            let bytes = std::fs::read(dir.join(real)).ok()?;
            tris.extend(xfile::triangles(&bytes)?);
        }
        Some(Model::build(vols, &tris))
    }
}

/// Binary DirectX `.x` files (`xof 0302bin 0032`), as the game ships them;
/// a few are MSZIP-compressed (`bzip`). Reads every `Mesh` (positions and
/// faces, n-gons as fans, file winding kept) with frame transforms applied,
/// the way the loader flattens them into one mesh.
pub mod xfile {
    use super::{Tri, Vec3};

    fn inflate_mszip(d: &[u8]) -> Option<Vec<u8>> {
        use miniz_oxide::inflate::core::{decompress, inflate_flags, DecompressorOxide};
        use miniz_oxide::inflate::TINFLStatus;
        // Header 16 B + u32 total size; then blocks [u16 raw][u16 csize]['CK'
        // deflate…]. Each block may refer back into earlier output (MSZIP's
        // dictionary), so decompress into one buffer that keeps growing.
        let total = u32::from_le_bytes(d.get(16..20)?.try_into().ok()?) as usize;
        let mut out = vec![0u8; total.saturating_sub(16)];
        let mut pos = 0usize;
        let mut p = 20usize;
        while p + 4 <= d.len() && pos < out.len() {
            let raw = u16::from_le_bytes([d[p], d[p + 1]]) as usize;
            let cs = u16::from_le_bytes([d[p + 2], d[p + 3]]) as usize;
            p += 4;
            let block = d.get(p + 2..p + cs)?;
            let mut r = DecompressorOxide::new();
            let flags = inflate_flags::TINFL_FLAG_USING_NON_WRAPPING_OUTPUT_BUF;
            let (st, _, n) = decompress(&mut r, block, &mut out[..(pos + raw).min(total)], pos, flags);
            if !matches!(st, TINFLStatus::Done | TINFLStatus::HasMoreOutput) {
                return None;
            }
            pos += n;
            p += cs;
        }
        out.truncate(pos);
        Some(out)
    }

    enum Tok {
        Name(String),
        Ints(Vec<u32>),
        Floats(Vec<f32>),
        Open,
        Close,
        Other,
    }

    fn tokens(b: &[u8], f64s: bool) -> Option<Vec<Tok>> {
        let u16_at = |p: usize| b.get(p..p + 2).map(|s| u16::from_le_bytes([s[0], s[1]]));
        let u32_at = |p: usize| b.get(p..p + 4).map(|s| u32::from_le_bytes(s.try_into().unwrap()));
        let mut out = Vec::new();
        let mut p = 0;
        while p + 2 <= b.len() {
            let t = u16_at(p)?;
            p += 2;
            match t {
                1 => {
                    let n = u32_at(p)? as usize;
                    p += 4;
                    out.push(Tok::Name(String::from_utf8_lossy(b.get(p..p + n)?).into_owned()));
                    p += n;
                }
                2 => {
                    let n = u32_at(p)? as usize;
                    p += 4 + n + 2; // string, then its terminator token
                    out.push(Tok::Other);
                }
                3 => {
                    out.push(Tok::Ints(vec![u32_at(p)?]));
                    p += 4;
                }
                5 => {
                    p += 16;
                    out.push(Tok::Other);
                }
                6 => {
                    let n = u32_at(p)? as usize;
                    p += 4;
                    let v = (0..n).map(|i| u32_at(p + 4 * i)).collect::<Option<Vec<_>>>()?;
                    p += 4 * n;
                    out.push(Tok::Ints(v));
                }
                7 => {
                    let n = u32_at(p)? as usize;
                    p += 4;
                    let sz = if f64s { 8 } else { 4 };
                    let mut v = Vec::with_capacity(n);
                    for i in 0..n {
                        let s = b.get(p + sz * i..p + sz * (i + 1))?;
                        v.push(if f64s { f64::from_le_bytes(s.try_into().unwrap()) as f32 } else { f32::from_le_bytes(s.try_into().unwrap()) });
                    }
                    p += sz * n;
                    out.push(Tok::Floats(v));
                }
                10 => out.push(Tok::Open),
                11 => out.push(Tok::Close),
                _ => out.push(Tok::Other),
            }
        }
        Some(out)
    }

    /// Row-vector D3D matrix: p' = p · M.
    fn apply(m: &[f32], p: Vec3) -> Vec3 {
        [
            p[0] * m[0] + p[1] * m[4] + p[2] * m[8] + m[12],
            p[0] * m[1] + p[1] * m[5] + p[2] * m[9] + m[13],
            p[0] * m[2] + p[1] * m[6] + p[2] * m[10] + m[14],
        ]
    }

    /// All triangles of all meshes in the file, in model space.
    pub fn triangles(file: &[u8]) -> Option<Vec<Tri>> {
        if file.len() < 16 || &file[..4] != b"xof " {
            return None;
        }
        let body: Vec<u8> = match &file[8..12] {
            b"bin " => file[16..].to_vec(),
            b"bzip" => inflate_mszip(file)?,
            _ => return None,
        };
        let f64s = &file[12..16] == b"0064";
        let toks = tokens(&body, f64s)?;

        // Template-name stack; frame matrices with the depth they apply at.
        let mut stack: Vec<String> = Vec::new();
        let mut pending: Option<String> = None;
        let mut mats: Vec<(usize, Vec<f32>)> = Vec::new();
        let mut out = Vec::new();
        // Mesh parse state: vertex count, vertices, face ints.
        let mut verts: Vec<Vec3> = Vec::new();
        let mut nverts: Option<usize> = None;
        let mut faces: Vec<u32> = Vec::new();
        let mut in_mesh_faces = false;

        let flush_mesh = |verts: &[Vec3], faces: &[u32], mats: &[(usize, Vec<f32>)], out: &mut Vec<Tri>| {
            let xf = |p: Vec3| mats.iter().rev().fold(p, |q, (_, m)| apply(m, q));
            let Some((&nf, rest)) = faces.split_first() else { return };
            let mut i = 0;
            for _ in 0..nf {
                let Some(&n) = rest.get(i) else { return };
                let idx = &rest[i + 1..(i + 1 + n as usize).min(rest.len())];
                i += 1 + n as usize;
                for k in 1..idx.len().saturating_sub(1) {
                    let g = |j: u32| verts.get(j as usize).copied().map(xf);
                    if let (Some(a), Some(b), Some(c)) = (g(idx[0]), g(idx[k]), g(idx[k + 1])) {
                        out.push(Tri([a, b, c]));
                    }
                }
            }
        };

        for t in toks {
            match t {
                // `Mesh single_mesh_object {`: the template name comes first.
                Tok::Name(n) => {
                    pending.get_or_insert(n);
                }
                Tok::Open => {
                    let name = pending.take().unwrap_or_default();
                    if name == "Mesh" {
                        verts.clear();
                        faces.clear();
                        nverts = None;
                        in_mesh_faces = false;
                    }
                    stack.push(name);
                }
                Tok::Close => {
                    if let Some(name) = stack.pop() {
                        if name == "Mesh" {
                            flush_mesh(&verts, &faces, &mats, &mut out);
                        }
                        if name == "Frame" && mats.last().is_some_and(|(d, _)| *d == stack.len()) {
                            mats.pop();
                        }
                    }
                }
                Tok::Floats(v) => {
                    pending = None;
                    match stack.last().map(String::as_str) {
                        Some("FrameTransformMatrix") if v.len() >= 16 && stack.len() >= 2 => {
                            mats.push((stack.len() - 2, v[..16].to_vec()));
                        }
                        Some("Mesh") if nverts.is_some() && verts.is_empty() => {
                            let n = nverts.unwrap();
                            verts = v.chunks_exact(3).take(n).map(|c| [c[0], c[1], c[2]]).collect();
                            in_mesh_faces = true;
                        }
                        _ => {}
                    }
                }
                Tok::Ints(v) => {
                    pending = None;
                    if stack.last().map(String::as_str) == Some("Mesh") {
                        if nverts.is_none() {
                            nverts = v.first().map(|&n| n as usize);
                        } else if in_mesh_faces {
                            faces.extend(v);
                        }
                    }
                }
                Tok::Other => pending = None,
            }
        }
        Some(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unit_volume(ext: f32) -> Volume {
        Volume::new([1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [ext; 3], [0.0; 3])
    }

    #[test]
    fn ellipsoid_extents_are_diameters() {
        let v = Volume::new([1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [20.0, 10.0, 4.0], [0.0; 3]);
        assert!(v.contains([9.9, 0.0, 0.0]) && !v.contains([10.1, 0.0, 0.0]));
        assert!(v.contains([0.0, 4.9, 0.0]) && !v.contains([0.0, 5.1, 0.0]));
        assert!(v.contains([0.0, 0.0, 1.9]) && !v.contains([0.0, 0.0, 2.1]));
        assert!(v.touches_segment([-50.0, 0.0, 0.0], [100.0, 0.0, 0.0]));
        assert!(!v.touches_segment([-50.0, 6.0, 0.0], [100.0, 0.0, 0.0]));
        assert!(!v.touches_segment([-50.0, 0.0, 0.0], [30.0, 0.0, 0.0]), "stops short");
    }

    /// The triangle (0,0,0) (0,1,0) (1,0,0): n = (v1−v0)×(v2−v1) = (0,0,−1).
    fn tri() -> Tri {
        Tri([[0.0, 0.0, 0.0], [0.0, 1.0, 0.0], [1.0, 0.0, 0.0]])
    }

    #[test]
    fn triangles_are_one_sided() {
        // From the front (−z side) through it: hit halfway.
        assert_eq!(segment_triangle([0.2, 0.2, -1.0], [0.0, 0.0, 2.0], &tri()), Some(0.5));
        // From behind: no hit.
        assert_eq!(segment_triangle([0.2, 0.2, 1.0], [0.0, 0.0, -2.0], &tri()), None);
        // Short of the plane, or beside the triangle.
        assert_eq!(segment_triangle([0.2, 0.2, -1.0], [0.0, 0.0, 0.5], &tri()), None);
        assert_eq!(segment_triangle([0.8, 0.8, -1.0], [0.0, 0.0, 2.0], &tri()), None);
    }

    #[test]
    fn builder_files_triangles_by_volume() {
        let far = Tri([[100.0, 0.0, 0.0], [100.0, 1.0, 0.0], [101.0, 0.0, 0.0]]);
        let whole_only = Model::build(&[(0, unit_volume(10.0))], &[tri(), far]);
        assert_eq!((whole_only.tris.len(), whole_only.subs.len()), (1, 0));
        // With sub-volumes: each triangle under each sub-volume holding a
        // vertex; an empty sub-volume (ext.x = 0) is skipped.
        let s1 = unit_volume(4.0);
        let s2 = Volume::new([1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [4.0; 3], [100.0, 0.0, 0.0]);
        let empty = Volume::new([1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 9.0, 9.0], [0.0; 3]);
        let m = Model::build(&[(0, unit_volume(1000.0)), (1, s1), (2, empty), (3, s2)], &[tri(), far]);
        assert!(m.tris.is_empty());
        assert_eq!(m.subs.iter().map(|(_, l)| l.len()).collect::<Vec<_>>(), vec![1, 1]);
        assert_eq!(m.triangle_count(), 2);
    }

    #[test]
    fn placed_model_sweep_reports_the_nearest_contact() {
        // Two facing-the-mover triangles at z = 5 and z = 2 (model space).
        let at = |z: f32| Tri([[-1.0, -1.0, z], [-1.0, 1.0, z], [1.0, -1.0, z]]);
        let m = Model::build(&[(0, unit_volume(100.0))], &[at(5.0), at(2.0)]);
        // Model turned 90° (fwd = +x): model z is world x.
        let pose = Pose::new([10.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]);
        let c = sweep(&m, &pose, [0.0, -0.2, 0.2], [20.0, 0.0, 0.0]).expect("hit");
        assert!((c.point[0] - 12.0).abs() < 1e-4, "{c:?}");
        assert!(sweep(&m, &pose, [0.0, 50.0, 0.0], [20.0, 0.0, 0.0]).is_none());
        assert_eq!(pose.to_model(pose.to_world([1.0, 2.0, 3.0])), [1.0, 2.0, 3.0]);
    }

    #[test]
    fn xfile_reads_a_mesh_with_quads_as_fans() {
        // xof 0302bin 0032: Mesh { [4] verts(12 floats) [1, 4, 0,1,2,3] }
        let mut b = b"xof 0302bin 0032".to_vec();
        let name = |b: &mut Vec<u8>, s: &str| {
            b.extend(1u16.to_le_bytes());
            b.extend((s.len() as u32).to_le_bytes());
            b.extend(s.as_bytes());
        };
        let ints = |b: &mut Vec<u8>, v: &[u32]| {
            b.extend(6u16.to_le_bytes());
            b.extend((v.len() as u32).to_le_bytes());
            v.iter().for_each(|x| b.extend(x.to_le_bytes()));
        };
        name(&mut b, "Mesh");
        b.extend(10u16.to_le_bytes());
        ints(&mut b, &[4]);
        b.extend(7u16.to_le_bytes());
        b.extend(12u32.to_le_bytes());
        for f in [0.0f32, 0.0, 0.0, 0.0, 1.0, 0.0, 1.0, 1.0, 0.0, 1.0, 0.0, 0.0] {
            b.extend(f.to_le_bytes());
        }
        ints(&mut b, &[1, 4, 0, 1, 2, 3]);
        b.extend(11u16.to_le_bytes());
        let t = xfile::triangles(&b).unwrap();
        assert_eq!(t.len(), 2);
        assert_eq!(t[1].0, [[0.0, 0.0, 0.0], [1.0, 1.0, 0.0], [1.0, 0.0, 0.0]]);
    }

    fn client_dir() -> Option<PathBuf> {
        let d = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        d.join("station3b_body.x").exists().then_some(d)
    }

    /// With the user's install next to the repo: real meshes load, MSZIP
    /// included, and the volume filter keeps a sensible share.
    #[test]
    fn real_client_meshes() {
        let Some(dir) = client_dir() else {
            eprintln!("client folder not found next to the repo; skipped");
            return;
        };
        let ex = Path::new(env!("CARGO_MANIFEST_DIR")).join("../notes/extracts");
        let models = Models::load(&ex, Some(&dir)).unwrap();
        for (id, min) in [(323u16, 500usize), (15, 20), (11, 20), (63, 20), (380, 200)] {
            let m = models.get(id);
            assert!(m.triangle_count() >= min, "model {id}: {} triangles", m.triangle_count());
        }
        // Quantar Depot's body (380) and an asteroid (15): rays from outside
        // hit the near surface from every side, rays from the centre never
        // hit (outward normals, one-sided).
        let dirs = [[0.0, 0.0, 1.0], [0.0, 0.0, -1.0], [1.0, 0.0, 0.0], [-1.0, 0.0, 0.0], [0.0, -1.0, 0.0], [0.0, 1.0, 0.0]];
        let pose = Pose::at([0.0; 3]);
        for (id, radius) in [(380u16, 800.0), (15, 64.0)] {
            let m = models.get(id);
            for d in dirs {
                let c = sweep(&m, &pose, scale(d, -5000.0), scale(d, 5000.0)).unwrap_or_else(|| panic!("{id}: no hit along {d:?}"));
                assert!(dot(c.point, d) < 0.0 && crate::ship::length(c.point) < radius, "{id} along {d:?}: {c:?}");
                if id == 15 {
                    assert!(sweep(&m, &pose, [0.0; 3], scale(d, 5000.0)).is_none());
                }
            }
        }
        // Quantar Core (323) is built from sub-volumes and has openings where
        // none covers the mesh (the client drops those triangles too).
        let st = models.get(323);
        let hits = dirs.iter().filter(|&&d| sweep(&st, &pose, scale(d, -5000.0), scale(d, 5000.0)).is_some()).count();
        assert!(hits >= 4, "{hits}/6");
        // Bb05 (hull 63) is MSZIP-compressed.
        assert!(xfile::triangles(&std::fs::read(dir.join("bb05.x")).unwrap()).unwrap().len() > 100);
    }
}
