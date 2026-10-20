//! Shader fallback blocks in materials: parameters chosen by hardware level.
//!
//! A VMT can hold blocks named after a DirectX level condition, whose
//! parameters apply only on hardware that meets it:
//!
//! ```text
//! "VertexLitGeneric"
//! {
//!     "$bumpmap" "models/combine_soldier/combinesoldier_normal"
//!     ">=dx90_20b" { "$basetexture" "models/combine_soldier/combinesoldier_noalpha" }
//!     "<dx90_20b"  { "$basetexture" "models/combine_soldier/combinesoldiersheet" }
//! }
//! ```
//!
//! `vmt-parser` ignores these blocks, so a material that sets its base
//! texture only inside one has none at all. [`apply`] resolves them the way
//! the engine does for a renderer of [`RENDER_LEVEL`]: the parameters of
//! every block whose condition holds override the shader's own, and the
//! other blocks are dropped. Blocks that are not level conditions (proxies,
//! older shaders' `_DX8` fallbacks) are left alone.

use std::borrow::Cow;
use std::fmt::Write as _;

use source_fs::keyvalues::{self, Block, Pair, Value};

/// The DirectX level rustlife renders as: the most capable Source knows
/// (`dx95`, Shader Model 3), above `dx90_20b`, so every DX9 condition holds.
pub(crate) const RENDER_LEVEL: u32 = 98;

/// `text` with its shader fallback blocks resolved; unchanged (borrowed) if
/// it has none or cannot be read as KeyValues (the VMT parser then reports
/// the problem).
pub(crate) fn apply(text: &str) -> Cow<'_, str> {
    let Ok(document) = keyvalues::parse(text) else {
        return Cow::Borrowed(text);
    };
    let Some(shader) = document.root.iter().next() else {
        return Cow::Borrowed(text);
    };
    let Value::Block(body) = &shader.value else {
        return Cow::Borrowed(text);
    };
    if !body.iter().any(|pair| condition(&pair.key).is_some()) {
        return Cow::Borrowed(text);
    }
    let mut kept: Vec<Pair> = Vec::new();
    let mut overrides: Vec<Pair> = Vec::new();
    for pair in body.iter() {
        match (condition(&pair.key), &pair.value) {
            (Some(true), Value::Block(block)) => overrides.extend(block.iter().cloned()),
            (Some(_), _) => {}
            (None, _) => kept.push(pair.clone()),
        }
    }
    for pair in overrides {
        kept.retain(|existing| !existing.key.eq_ignore_ascii_case(&pair.key));
        kept.push(pair);
    }
    let mut out = String::with_capacity(text.len());
    write_pair(
        &mut out,
        &Pair {
            key: shader.key.clone(),
            value: Value::Block(Block::from_pairs(kept)),
        },
        0,
    );
    Cow::Owned(out)
}

/// Whether a block named `key` applies at [`RENDER_LEVEL`]; `None` if the
/// name is not a DirectX level condition.
fn condition(key: &str) -> Option<bool> {
    let key = key.trim().to_ascii_lowercase();
    let (operator, level) = [">=", "<=", ">", "<"]
        .into_iter()
        .find_map(|op| Some((op, key.strip_prefix(op)?)))?;
    let level = dx_level(level)?;
    Some(match operator {
        ">=" => RENDER_LEVEL >= level,
        "<=" => RENDER_LEVEL <= level,
        ">" => RENDER_LEVEL > level,
        _ => RENDER_LEVEL < level,
    })
}

/// The level of a DirectX name: `dx90_20b` is 95 (between DX9 and DX9 with
/// Shader Model 3), `dx90` and `dx9` are 90, `dx81` is 81, `dx8` is 80.
fn dx_level(name: &str) -> Option<u32> {
    let digits = name.strip_prefix("dx")?;
    if digits == "90_20b" {
        return Some(95);
    }
    let value: u32 = digits.parse().ok()?;
    Some(if value < 10 { value * 10 } else { value })
}

/// Writes `pair` as KeyValues text, indented by `depth` tabs.
fn write_pair(out: &mut String, pair: &Pair, depth: usize) {
    let indent = "\t".repeat(depth);
    match &pair.value {
        Value::String(value) => {
            let _ = writeln!(out, "{indent}\"{}\" \"{value}\"", pair.key);
        }
        Value::Block(block) => {
            let _ = writeln!(out, "{indent}\"{}\"\n{indent}{{", pair.key);
            for inner in block.iter() {
                write_pair(out, inner, depth + 1);
            }
            let _ = writeln!(out, "{indent}}}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SOLDIER: &str = r#""VertexLitGeneric"
{
	"$bumpmap" "models/combine_soldier/combinesoldier_normal"
	"$basetexture" "models/fallback"
	">=dx90_20b"
	{
		"$baseTexture" "Models/Combine_soldier/combinesoldier_noalpha"
	}
	"<dx90_20b"
	{
		"$baseTexture" "Models/Combine_soldier/combinesoldiersheet"
	}
	"Proxies" { "Sine" { "resultVar" "$alpha" } }
}
"#;

    #[test]
    fn matching_blocks_override_and_others_are_dropped() {
        let resolved = apply(SOLDIER);
        let document = keyvalues::parse(&resolved).expect("valid output");
        let body = document.root.get_block("VertexLitGeneric").expect("shader");
        assert_eq!(
            body.get_str("$basetexture"),
            Some("Models/Combine_soldier/combinesoldier_noalpha")
        );
        assert_eq!(body.get_all("$basetexture").count(), 1, "no duplicates");
        assert!(body.get("<dx90_20b").is_none() && body.get(">=dx90_20b").is_none());
        assert!(
            body.get_path(&["Proxies", "Sine"]).is_some(),
            "other blocks kept"
        );
    }

    #[test]
    fn materials_without_fallbacks_are_left_as_they_are() {
        let text = "\"LightmappedGeneric\" { \"$basetexture\" \"a/b\" }";
        assert!(matches!(apply(text), Cow::Borrowed(_)));
        assert!(matches!(apply("{{ broken"), Cow::Borrowed(_)));
    }

    #[test]
    fn levels_compare_as_the_engine_orders_them() {
        assert_eq!(condition(">=dx90"), Some(true));
        assert_eq!(condition(">=DX90_20b"), Some(true));
        assert_eq!(condition("<dx90_20b"), Some(false));
        assert_eq!(condition("<=dx80"), Some(false));
        assert_eq!(condition(">dx8"), Some(true));
        assert_eq!(condition("$basetexture"), None);
        assert_eq!(condition("VertexLitGeneric_DX8"), None);
        assert_eq!(dx_level("dx9"), Some(90));
        assert_eq!(dx_level("dx81"), Some(81));
    }
}
