//! The eyeballs of a Source model, which a character's `Eyes` material needs.
//!
//! A character's eyes are spheres, each with a `$basetexture` of the
//! eyeball (the white of it) and an `$iris`. The game does not draw the
//! iris from the sphere's own texture coordinates: its shader projects the
//! iris onto the part of the sphere that faces the way the eye looks, from
//! the numbers the model compiler stored with each eyeball. `vmdl` does not
//! read them, so this module does, from the `.mdl`'s bytes.
//!
//! An eyeball sits in the space of a bone (the head's); its vertices must be
//! brought into that space before [`Eyeball::iris_uv`] is asked about them.

/// Offset in the `.mdl` header of the body part count; the table offset
/// follows it.
const BODY_PARTS_AT: usize = 232;

/// Bytes of one body part (`mstudiobodyparts_t`).
const BODY_PART_SIZE: usize = 16;

/// Bytes of one model of a body part (`mstudiomodel_t`).
const MODEL_SIZE: usize = 148;

/// Offset in a model of its eyeball count; the table offset follows it.
const MODEL_EYEBALLS_AT: usize = 100;

/// Bytes of one eyeball (`mstudioeyeball_t`).
const EYEBALL_SIZE: usize = 172;

/// More eyeballs than any model has; the bound keeps a corrupt count from
/// reading on and on.
const MAX_EYEBALLS: usize = 64;

/// An eye of a model: where it is and which way it looks.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Eyeball {
    /// The bone it is fixed to.
    pub bone: usize,
    /// Its centre, in that bone's space, Source units.
    pub origin: [f32; 3],
    /// The sphere's radius.
    pub radius: f32,
    /// Which way is up for the iris, a unit vector in the bone's space.
    pub up: [f32; 3],
    /// Which way the eye looks, a unit vector in the bone's space.
    pub forward: [f32; 3],
    /// How many times larger than the iris texture's own square the sphere's
    /// width is: the larger, the smaller the iris.
    pub iris_scale: f32,
}

impl Eyeball {
    /// Where on the iris texture `point` (in the eye's bone's space) falls,
    /// `0..=1` across it, or `None` for a point on the back half of the
    /// sphere, which the iris never reaches. Outside `0..=1` the point is
    /// beyond the iris: the white of the eye.
    #[must_use]
    pub fn iris_uv(&self, point: [f32; 3]) -> Option<[f32; 2]> {
        let offset = sub(point, self.origin);
        if dot(offset, self.forward) <= 0.0 {
            return None;
        }
        // The iris texture spans `radius × 2 / iris_scale` across.
        let scale = self.iris_scale / (2.0 * self.radius);
        let right = cross(self.up, self.forward);
        Some([
            0.5 + dot(offset, right) * scale,
            0.5 - dot(offset, self.up) * scale,
        ])
    }
}

fn sub(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

fn dot(a: [f32; 3], b: [f32; 3]) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

fn cross(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

/// A little-endian reader over the `.mdl`'s bytes that says `None` rather
/// than panics when what it is asked for is not there.
struct Bytes<'a>(&'a [u8]);

impl Bytes<'_> {
    fn word(&self, at: usize) -> Option<[u8; 4]> {
        self.0.get(at..at.checked_add(4)?)?.try_into().ok()
    }

    fn int(&self, at: usize) -> Option<i32> {
        self.word(at).map(i32::from_le_bytes)
    }

    fn index(&self, at: usize) -> Option<usize> {
        usize::try_from(self.int(at)?).ok()
    }

    fn float(&self, at: usize) -> Option<f32> {
        self.word(at).map(f32::from_le_bytes)
    }

    fn vector(&self, at: usize) -> Option<[f32; 3]> {
        Some([self.float(at)?, self.float(at + 4)?, self.float(at + 8)?])
    }
}

/// The eyeballs of every model of `mdl`, in file order. A model that has
/// none, or whose tables cannot be read, gives none: never an error, as a
/// model without readable eyes still draws.
#[must_use]
pub fn read(mdl: &[u8]) -> Vec<Eyeball> {
    read_all(&Bytes(mdl)).unwrap_or_default()
}

fn read_all(mdl: &Bytes<'_>) -> Option<Vec<Eyeball>> {
    let parts = mdl.index(BODY_PARTS_AT)?;
    let table = mdl.index(BODY_PARTS_AT + 4)?;
    let mut eyes = Vec::new();
    for part in 0..parts.min(MAX_EYEBALLS) {
        let at = table.checked_add(part * BODY_PART_SIZE)?;
        let models = mdl.index(at + 4)?;
        let first = at.checked_add(mdl.index(at + 12)?)?;
        for model in 0..models.min(MAX_EYEBALLS) {
            let model = first.checked_add(model * MODEL_SIZE)?;
            let count = mdl.index(model + MODEL_EYEBALLS_AT)?;
            let list = model.checked_add(mdl.index(model + MODEL_EYEBALLS_AT + 4)?)?;
            for eye in 0..count.min(MAX_EYEBALLS) {
                eyes.push(read_eyeball(mdl, list.checked_add(eye * EYEBALL_SIZE)?)?);
            }
        }
    }
    Some(eyes)
}

fn read_eyeball(mdl: &Bytes<'_>, at: usize) -> Option<Eyeball> {
    Some(Eyeball {
        bone: mdl.index(at + 4)?,
        origin: mdl.vector(at + 8)?,
        radius: mdl.float(at + 24)?,
        up: mdl.vector(at + 28)?,
        forward: mdl.vector(at + 40)?,
        iris_scale: mdl.float(at + 60)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A `.mdl` of one body part with one model with the given eyeballs.
    fn mdl_with(eyes: &[Eyeball]) -> Vec<u8> {
        let table = 300;
        let model = table + BODY_PART_SIZE;
        let list = model + MODEL_SIZE;
        let mut bytes = vec![0u8; list + eyes.len() * EYEBALL_SIZE];
        let mut put = |at: usize, word: [u8; 4]| bytes[at..at + 4].copy_from_slice(&word);
        put(BODY_PARTS_AT, 1i32.to_le_bytes());
        put(
            BODY_PARTS_AT + 4,
            i32::try_from(table).unwrap().to_le_bytes(),
        );
        // One model, `MODEL_SIZE` after the body part.
        put(table + 4, 1i32.to_le_bytes());
        put(
            table + 12,
            i32::try_from(model - table).unwrap().to_le_bytes(),
        );
        let count = i32::try_from(eyes.len()).unwrap();
        put(model + MODEL_EYEBALLS_AT, count.to_le_bytes());
        put(
            model + MODEL_EYEBALLS_AT + 4,
            i32::try_from(list - model).unwrap().to_le_bytes(),
        );
        for (index, eye) in eyes.iter().enumerate() {
            let at = list + index * EYEBALL_SIZE;
            put(at + 4, i32::try_from(eye.bone).unwrap().to_le_bytes());
            for (axis, value) in eye.origin.iter().enumerate() {
                put(at + 8 + axis * 4, value.to_le_bytes());
            }
            put(at + 24, eye.radius.to_le_bytes());
            for axis in 0..3 {
                put(at + 28 + axis * 4, eye.up[axis].to_le_bytes());
                put(at + 40 + axis * 4, eye.forward[axis].to_le_bytes());
            }
            put(at + 60, eye.iris_scale.to_le_bytes());
        }
        bytes
    }

    fn eye() -> Eyeball {
        Eyeball {
            bone: 6,
            origin: [3.0, -3.5, 1.25],
            radius: 0.5,
            up: [0.0, 0.0, 1.0],
            forward: [0.0, -1.0, 0.0],
            iris_scale: 1.6,
        }
    }

    #[test]
    fn eyeballs_are_read_from_the_model() {
        let other = Eyeball {
            bone: 2,
            origin: [3.0, -3.5, -1.25],
            ..eye()
        };
        assert_eq!(read(&mdl_with(&[eye(), other])), vec![eye(), other]);
    }

    #[test]
    fn a_model_without_eyeballs_or_with_cut_off_ones_has_none() {
        assert!(read(&mdl_with(&[])).is_empty());
        let mut cut = mdl_with(&[eye()]);
        cut.truncate(cut.len() - 120);
        assert!(read(&cut).is_empty());
        assert!(read(&[]).is_empty());
        assert!(read(&[0xff; 400]).is_empty());
    }

    #[test]
    fn the_iris_is_centred_on_where_the_eye_looks() {
        let eye = eye();
        let front = [3.0, -3.5 - eye.radius, 1.25];
        let [u, v] = eye.iris_uv(front).unwrap();
        assert!((u - 0.5).abs() < 1e-5 && (v - 0.5).abs() < 1e-5, "{u} {v}");
    }

    #[test]
    fn the_iris_is_a_little_less_than_the_width_of_the_eye() {
        let eye = eye();
        // Straight up the sphere's front is 1/1.6 of the radius from the
        // centre of the texture, on its half a side.
        let [_, v] = eye
            .iris_uv([3.0, -3.5 - 0.1, 1.25 + eye.radius * 0.5])
            .unwrap();
        assert!((v - (0.5 - 0.5 * 0.5 * 1.6)).abs() < 1e-5, "{v}");
        // Sideways, the same distance puts it as far from the centre.
        let [u, _] = eye
            .iris_uv([3.0 + eye.radius * 0.5, -3.5 - 0.1, 1.25])
            .unwrap();
        assert!(((u - 0.5).abs() - 0.5 * 0.5 * 1.6).abs() < 1e-5, "{u}");
    }

    #[test]
    fn the_back_of_the_eye_has_no_iris() {
        let eye = eye();
        assert_eq!(eye.iris_uv([3.0, -3.5 + eye.radius, 1.25]), None);
        assert_eq!(
            eye.iris_uv([3.0, -3.5, 1.25 + eye.radius]),
            None,
            "its edge"
        );
    }
}
