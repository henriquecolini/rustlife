//! Particle system files: `particles/*.pcf`.
//!
//! A PCF is a binary DMX file (`<!-- dmx encoding binary 2 format pcf 1 -->`)
//! holding a graph of elements. The root element's `particleSystemDefinitions`
//! attribute lists one `DmeParticleSystemDefinition` per system; each names its
//! `renderers`, `operators`, `initializers`, `emitters`, `forces`,
//! `constraints` (all `DmeParticleOperator`s, whose `functionName` picks the
//! behaviour and whose other attributes tune it) and `children` (other
//! systems spawned along with it).
//!
//! [`parse`] decodes the file into [`ParticleSystem`]s with typed
//! [`Value`] attributes. It reads DMX binary versions 1 to 5.
//!
//! | Version | Strings | Element names | Counts and indices |
//! |---------|---------|---------------|--------------------|
//! | 1       | inline  | inline        | -                  |
//! | 2, 3    | `u16` dictionary | inline | `u16`           |
//! | 4       | `u32` dictionary | inline | `u32`           |
//! | 5       | `u32` dictionary | dictionary | `u32`       |

use std::collections::BTreeMap;

use thiserror::Error;

/// Errors from [`parse`].
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum PcfError {
    /// The header is not a binary DMX `pcf` header.
    #[error("not a binary DMX particle file")]
    NotPcf,
    /// A DMX encoding version this parser does not read.
    #[error("unsupported DMX binary version {0}")]
    UnsupportedVersion(u32),
    /// The file ends inside a value.
    #[error("file truncated at byte {0}")]
    Truncated(usize),
    /// A string index, element index or attribute type outside its table.
    #[error("invalid {0}")]
    Invalid(&'static str),
}

/// A decoded attribute value.
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    /// An integer.
    Int(i32),
    /// A float.
    Float(f32),
    /// A boolean.
    Bool(bool),
    /// A string.
    Str(String),
    /// A colour, RGBA bytes.
    Color([u8; 4]),
    /// A 2-, 3- or 4-component float vector, also angles and quaternions.
    Vec(Vec<f32>),
    /// A reference to another element by index into [`Pcf::elements`], or
    /// `None` for a null reference.
    Element(Option<usize>),
    /// Raw bytes.
    Binary(Vec<u8>),
    /// An array of values of one type.
    Array(Vec<Value>),
}

impl Value {
    /// The value as a float, converting an integer or boolean.
    #[must_use]
    pub fn as_f32(&self) -> Option<f32> {
        match *self {
            Self::Float(v) => Some(v),
            #[allow(clippy::cast_precision_loss)]
            Self::Int(v) => Some(v as f32),
            Self::Bool(v) => Some(f32::from(u8::from(v))),
            _ => None,
        }
    }

    /// The value as a string.
    #[must_use]
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Self::Str(s) => Some(s),
            _ => None,
        }
    }
}

/// One element of the graph: a typed, named bag of attributes.
#[derive(Debug, Clone, PartialEq)]
pub struct Element {
    /// The element class, such as `DmeParticleOperator`.
    pub class: String,
    /// The element name.
    pub name: String,
    /// Attributes by name.
    pub attributes: BTreeMap<String, Value>,
}

impl Element {
    /// The float attribute `key`, or `default` when absent or not numeric.
    #[must_use]
    pub fn float(&self, key: &str, default: f32) -> f32 {
        self.attributes
            .get(key)
            .and_then(Value::as_f32)
            .unwrap_or(default)
    }

    /// The string attribute `key`.
    #[must_use]
    pub fn string(&self, key: &str) -> Option<&str> {
        self.attributes.get(key).and_then(Value::as_str)
    }
}

/// A decoded file: every element, in file order. Element 0 is the root.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Pcf {
    /// All elements.
    pub elements: Vec<Element>,
}

/// One operator of a particle system: a `functionName` plus its settings.
#[derive(Debug, Clone, PartialEq)]
pub struct Operator {
    /// The behaviour, such as `Lifespan Decay` or `render_animated_sprites`.
    pub function: String,
    /// The operator's settings, by attribute name.
    pub attributes: BTreeMap<String, Value>,
}

impl Operator {
    /// The float setting `key`, or `default`.
    #[must_use]
    pub fn float(&self, key: &str, default: f32) -> f32 {
        self.attributes
            .get(key)
            .and_then(Value::as_f32)
            .unwrap_or(default)
    }
}

/// A child system and the delay before it starts.
#[derive(Debug, Clone, PartialEq)]
pub struct Child {
    /// The child's name.
    pub name: String,
    /// Seconds after the parent starts.
    pub delay: f32,
}

/// One particle system definition.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ParticleSystem {
    /// The system's name, the key game code spawns it by.
    pub name: String,
    /// The material, relative to `materials/`, without the extension.
    pub material: String,
    /// The maximum number of live particles.
    pub max_particles: u32,
    /// Particles created at once when the system starts.
    pub initial_particles: u32,
    /// Particle radius at creation, units.
    pub radius: f32,
    /// Particle colour at creation, RGBA bytes.
    pub color: [u8; 4],
    /// Draw order among systems (`sequence_number`).
    pub sequence: i32,
    /// Renderers.
    pub renderers: Vec<Operator>,
    /// Operators, run each frame.
    pub operators: Vec<Operator>,
    /// Initializers, run once on each new particle.
    pub initializers: Vec<Operator>,
    /// Emitters.
    pub emitters: Vec<Operator>,
    /// Forces.
    pub forces: Vec<Operator>,
    /// Constraints.
    pub constraints: Vec<Operator>,
    /// Child systems.
    pub children: Vec<Child>,
}

impl Pcf {
    /// The particle systems the root element lists, in file order.
    #[must_use]
    pub fn systems(&self) -> Vec<ParticleSystem> {
        let Some(Value::Array(defs)) = self
            .elements
            .first()
            .and_then(|e| e.attributes.get("particleSystemDefinitions"))
        else {
            return Vec::new();
        };
        defs.iter()
            .filter_map(|v| self.element_of(v))
            .map(|e| self.system(e))
            .collect()
    }

    fn element_of(&self, v: &Value) -> Option<&Element> {
        match v {
            Value::Element(Some(i)) => self.elements.get(*i),
            _ => None,
        }
    }

    fn operators(&self, e: &Element, key: &str) -> Vec<Operator> {
        let Some(Value::Array(list)) = e.attributes.get(key) else {
            return Vec::new();
        };
        list.iter()
            .filter_map(|v| self.element_of(v))
            .map(|o| Operator {
                function: o.string("functionName").unwrap_or_default().to_owned(),
                attributes: o.attributes.clone(),
            })
            .collect()
    }

    fn system(&self, e: &Element) -> ParticleSystem {
        let children = match e.attributes.get("children") {
            Some(Value::Array(list)) => list
                .iter()
                .filter_map(|v| self.element_of(v))
                .map(|c| {
                    // A child entry is a `DmeParticleChild` pointing at the
                    // system, with the delay stored beside it.
                    let target = match c.attributes.get("child") {
                        Some(v) => self
                            .element_of(v)
                            .map_or(c.name.as_str(), |t| t.name.as_str()),
                        None => c.name.as_str(),
                    };
                    Child {
                        name: target.to_owned(),
                        delay: c.float("delay", 0.0),
                    }
                })
                .collect(),
            _ => Vec::new(),
        };
        let color = match e.attributes.get("color") {
            Some(Value::Color(c)) => *c,
            _ => [255; 4],
        };
        ParticleSystem {
            name: e.name.clone(),
            material: e.string("material").unwrap_or_default().to_owned(),
            max_particles: count(e.float("max_particles", 1000.0)),
            initial_particles: count(e.float("initial_particles", 0.0)),
            radius: e.float("radius", 5.0),
            color,
            #[allow(clippy::cast_possible_truncation)]
            sequence: e.float("sequence_number", 0.0) as i32,
            renderers: self.operators(e, "renderers"),
            operators: self.operators(e, "operators"),
            initializers: self.operators(e, "initializers"),
            emitters: self.operators(e, "emitters"),
            forces: self.operators(e, "forces"),
            constraints: self.operators(e, "constraints"),
            children,
        }
    }
}

#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn count(v: f32) -> u32 {
    v.max(0.0) as u32
}

struct Reader<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8], PcfError> {
        let end = self.pos.checked_add(n).filter(|&e| e <= self.data.len());
        let end = end.ok_or(PcfError::Truncated(self.pos))?;
        let s = &self.data[self.pos..end];
        self.pos = end;
        Ok(s)
    }

    fn u8(&mut self) -> Result<u8, PcfError> {
        Ok(self.take(1)?[0])
    }

    fn u16(&mut self) -> Result<u16, PcfError> {
        let b = self.take(2)?;
        Ok(u16::from_le_bytes([b[0], b[1]]))
    }

    fn i32(&mut self) -> Result<i32, PcfError> {
        let b = self.take(4)?;
        Ok(i32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }

    fn f32(&mut self) -> Result<f32, PcfError> {
        let b = self.take(4)?;
        Ok(f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }

    /// A non-negative `i32` as a length or index.
    fn len(&mut self) -> Result<usize, PcfError> {
        usize::try_from(self.i32()?).map_err(|_| PcfError::Invalid("negative length"))
    }

    fn cstr(&mut self) -> Result<String, PcfError> {
        let rest = &self.data[self.pos.min(self.data.len())..];
        let n = rest
            .iter()
            .position(|&b| b == 0)
            .ok_or(PcfError::Truncated(self.pos))?;
        let s = String::from_utf8_lossy(&rest[..n]).into_owned();
        self.pos += n + 1;
        Ok(s)
    }

    /// A string-table index or count: `u16` before version 4, else `i32`.
    fn index(&mut self, version: u32) -> Result<usize, PcfError> {
        if version >= 4 {
            self.len()
        } else {
            Ok(usize::from(self.u16()?))
        }
    }
}

/// Bytes after the header line's terminating `\n\0`.
fn header(data: &[u8]) -> Result<(u32, usize), PcfError> {
    let end = data.iter().position(|&b| b == 0).ok_or(PcfError::NotPcf)?;
    let line = std::str::from_utf8(&data[..end]).map_err(|_| PcfError::NotPcf)?;
    let inner = line
        .trim()
        .strip_prefix("<!--")
        .and_then(|l| l.strip_suffix("-->"))
        .ok_or(PcfError::NotPcf)?;
    let words: Vec<&str> = inner.split_whitespace().collect();
    match words.as_slice() {
        ["dmx", "encoding", "binary", v, "format", "pcf", _] => {
            let version = v.parse().map_err(|_| PcfError::NotPcf)?;
            Ok((version, end + 1))
        }
        _ => Err(PcfError::NotPcf),
    }
}

/// Decode a binary DMX particle file.
///
/// # Errors
/// [`PcfError`] when the header is not a binary DMX `pcf` one, the version is
/// not 1-5, or the data is truncated or refers outside its tables.
pub fn parse(data: &[u8]) -> Result<Pcf, PcfError> {
    let (version, start) = header(data)?;
    if !(1..=5).contains(&version) {
        return Err(PcfError::UnsupportedVersion(version));
    }
    let mut r = Reader { data, pos: start };
    let mut strings = Vec::new();
    if version >= 2 {
        for _ in 0..r.index(version)? {
            strings.push(r.cstr()?);
        }
    }
    let lookup = |r: &mut Reader<'_>| -> Result<String, PcfError> {
        if version >= 2 {
            let i = r.index(version)?;
            strings
                .get(i)
                .cloned()
                .ok_or(PcfError::Invalid("string index"))
        } else {
            r.cstr()
        }
    };

    let count = r.len()?;
    // Every element costs at least 16 bytes of id, so a count the file cannot
    // hold is corrupt; reject before allocating.
    if count > data.len() / 16 {
        return Err(PcfError::Invalid("element count"));
    }
    let mut elements = Vec::with_capacity(count);
    for _ in 0..count {
        let class = lookup(&mut r)?;
        let name = if version >= 5 {
            lookup(&mut r)?
        } else {
            r.cstr()?
        };
        r.take(16)?;
        elements.push(Element {
            class,
            name,
            attributes: BTreeMap::new(),
        });
    }
    for element in &mut elements {
        let n = r.len()?;
        for _ in 0..n {
            let key = lookup(&mut r)?;
            let ty = r.u8()?;
            let value = read_value(&mut r, ty, version, count, &lookup)?;
            element.attributes.insert(key, value);
        }
    }
    Ok(Pcf { elements })
}

fn read_value(
    r: &mut Reader<'_>,
    ty: u8,
    version: u32,
    elements: usize,
    lookup: &dyn Fn(&mut Reader<'_>) -> Result<String, PcfError>,
) -> Result<Value, PcfError> {
    if ty > 14 {
        let n = r.len()?;
        if n > r.data.len() {
            return Err(PcfError::Invalid("array length"));
        }
        let mut out = Vec::with_capacity(n);
        for _ in 0..n {
            out.push(read_scalar(r, ty - 14, version, elements, lookup)?);
        }
        return Ok(Value::Array(out));
    }
    read_scalar(r, ty, version, elements, lookup)
}

fn read_scalar(
    r: &mut Reader<'_>,
    ty: u8,
    version: u32,
    elements: usize,
    lookup: &dyn Fn(&mut Reader<'_>) -> Result<String, PcfError>,
) -> Result<Value, PcfError> {
    let floats = |r: &mut Reader<'_>, n: usize| -> Result<Value, PcfError> {
        (0..n)
            .map(|_| r.f32())
            .collect::<Result<_, _>>()
            .map(Value::Vec)
    };
    Ok(match ty {
        1 => {
            let i = r.i32()?;
            match usize::try_from(i) {
                Ok(i) if i < elements => Value::Element(Some(i)),
                // -1 is null, -2 an external reference by id.
                _ if i == -1 => Value::Element(None),
                _ if i == -2 => {
                    r.cstr()?;
                    Value::Element(None)
                }
                _ => return Err(PcfError::Invalid("element index")),
            }
        }
        // Time is stored as an integer too.
        2 | 7 => Value::Int(r.i32()?),
        3 => Value::Float(r.f32()?),
        4 => Value::Bool(r.u8()? != 0),
        5 => {
            // Attribute strings are inline up to version 3.
            Value::Str(if version >= 4 { lookup(r)? } else { r.cstr()? })
        }
        6 => {
            let n = r.len()?;
            Value::Binary(r.take(n)?.to_vec())
        }
        8 => {
            let b = r.take(4)?;
            Value::Color([b[0], b[1], b[2], b[3]])
        }
        9 => floats(r, 2)?,
        10 | 12 => floats(r, 3)?,
        11 | 13 => floats(r, 4)?,
        14 => floats(r, 16)?,
        _ => return Err(PcfError::Invalid("attribute type")),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Builds a version 2 file.
    struct Builder {
        strings: Vec<&'static str>,
        elements: Vec<(usize, &'static str)>,
        body: Vec<u8>,
    }

    impl Builder {
        fn bytes(&self) -> Vec<u8> {
            let mut out = b"<!-- dmx encoding binary 2 format pcf 1 -->\n\0".to_vec();
            out.extend((self.strings.len() as u16).to_le_bytes());
            for s in &self.strings {
                out.extend(s.as_bytes());
                out.push(0);
            }
            out.extend((self.elements.len() as i32).to_le_bytes());
            for (class, name) in &self.elements {
                out.extend((*class as u16).to_le_bytes());
                out.extend(name.as_bytes());
                out.push(0);
                out.extend([0u8; 16]);
            }
            out.extend(&self.body);
            out
        }
    }

    /// Root listing one system with one operator.
    fn sample() -> Vec<u8> {
        let mut b = Builder {
            strings: vec![
                "DmElement",
                "particleSystemDefinitions",
                "DmeParticleSystemDefinition",
                "max_particles",
                "material",
                "operators",
                "DmeParticleOperator",
                "functionName",
                "radius",
                "color",
            ],
            elements: vec![(0, "untitled"), (2, "spark"), (6, "Lifespan Decay")],
            body: Vec::new(),
        };
        // Root: one attribute, an element array [1].
        b.body.extend(1i32.to_le_bytes());
        b.body.extend(1u16.to_le_bytes());
        b.body.push(15);
        b.body.extend(1i32.to_le_bytes());
        b.body.extend(1i32.to_le_bytes());
        // System: max_particles int, material string, operators array,
        // radius float, color.
        b.body.extend(5i32.to_le_bytes());
        b.body.extend(3u16.to_le_bytes());
        b.body.push(2);
        b.body.extend(64i32.to_le_bytes());
        b.body.extend(4u16.to_le_bytes());
        b.body.push(5);
        b.body.extend(b"effects/spark\0");
        b.body.extend(5u16.to_le_bytes());
        b.body.push(15);
        b.body.extend(1i32.to_le_bytes());
        b.body.extend(2i32.to_le_bytes());
        b.body.extend(8u16.to_le_bytes());
        b.body.push(3);
        b.body.extend(2.5f32.to_le_bytes());
        b.body.extend(9u16.to_le_bytes());
        b.body.push(8);
        b.body.extend([255, 128, 0, 255]);
        // Operator: functionName.
        b.body.extend(1i32.to_le_bytes());
        b.body.extend(7u16.to_le_bytes());
        b.body.push(5);
        b.body.extend(b"Lifespan Decay\0");
        b.bytes()
    }

    #[test]
    fn parses_a_system() {
        let pcf = parse(&sample()).unwrap();
        let systems = pcf.systems();
        assert_eq!(systems.len(), 1);
        let s = &systems[0];
        assert_eq!(s.name, "spark");
        assert_eq!(s.material, "effects/spark");
        assert_eq!(s.max_particles, 64);
        assert!((s.radius - 2.5).abs() < f32::EPSILON);
        assert_eq!(s.color, [255, 128, 0, 255]);
        assert_eq!(s.operators.len(), 1);
        assert_eq!(s.operators[0].function, "Lifespan Decay");
    }

    #[test]
    fn rejects_other_files() {
        assert_eq!(parse(b"hello"), Err(PcfError::NotPcf));
        assert_eq!(
            parse(b"<!-- dmx encoding binary 9 format pcf 1 -->\n\0"),
            Err(PcfError::UnsupportedVersion(9))
        );
    }

    #[test]
    fn every_truncation_is_an_error_not_a_panic() {
        let full = sample();
        for n in 0..full.len() {
            assert!(parse(&full[..n]).is_err(), "prefix of {n} bytes parsed");
        }
    }

    #[test]
    fn corrupt_counts_are_rejected() {
        let mut bytes = sample();
        // Overwrite the element count with a huge one.
        let at = bytes.iter().position(|&b| b == 0).unwrap() + 1;
        let strings_end = {
            let mut p = at + 2;
            for _ in 0..10 {
                p += bytes[p..].iter().position(|&b| b == 0).unwrap() + 1;
            }
            p
        };
        bytes[strings_end..strings_end + 4].copy_from_slice(&i32::MAX.to_le_bytes());
        assert!(parse(&bytes).is_err());
    }
}
