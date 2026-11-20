//! Planning an import: which of the files the player picked are copied into
//! the browser's storage, and where.
//!
//! The player picks a folder; the browser lists everything in it. The rules
//! (`crates/rl_web/data/import.ron`) say which of those files the game needs, grouped
//! so the optional bulk (voice-over) can be left out. Planning is pure: it
//! turns a list of files into a [`Plan`], which the browser-side copier then
//! carries out.

use std::collections::BTreeSet;

use serde::Deserialize;
use source_fs::path::{Glob, normalize};

use crate::manifest::Manifest;

/// The rules shipped with the game, `crates/rl_web/data/import.ron`.
const BUILTIN_RULES: &str = include_str!("../data/import.ron");

/// What to import, as data.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ImportRules {
    /// The mod folder that holds `gameinfo.txt`; where the install is.
    pub mod_dir: String,
    /// The groups of files, in the order they are claimed and copied.
    pub groups: Vec<Group>,
}

/// A set of files imported (or left out) together.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Group {
    /// A stable identifier, used to say which optional groups are enabled.
    pub id: String,
    /// What the player is shown.
    pub title: String,
    /// Always imported; without it the game cannot run.
    pub required: bool,
    /// For optional groups, whether they start enabled.
    pub default_on: bool,
    /// Patterns (see `source_fs::path::Glob`) over paths inside the install.
    pub include: Vec<String>,
    /// Patterns that take a file back out of this group.
    pub exclude: Vec<String>,
}

impl ImportRules {
    /// The rules that ship with the game.
    ///
    /// # Panics
    ///
    /// Never in a build that passes its tests: the shipped file is checked.
    #[must_use]
    #[expect(
        clippy::expect_used,
        reason = "the shipped rules are a tested constant"
    )]
    pub fn builtin() -> Self {
        Self::parse(BUILTIN_RULES).expect("data/import.ron is valid")
    }

    /// Parses rules from RON text.
    ///
    /// # Errors
    ///
    /// Fails on syntax errors or unknown fields.
    pub fn parse(text: &str) -> Result<Self, ron::error::SpannedError> {
        ron::from_str(text)
    }

    /// The ids of the optional groups that start enabled.
    #[must_use]
    pub fn default_enabled(&self) -> BTreeSet<String> {
        self.groups
            .iter()
            .filter(|group| !group.required && group.default_on)
            .map(|group| group.id.clone())
            .collect()
    }
}

/// A file the player picked: its path inside the picked folder, and size.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    /// Slash-separated, as the browser reports it (it starts with the picked
    /// folder's own name when the picker is an `<input>`).
    pub path: String,
    /// Length in bytes.
    pub size: u64,
}

/// One file to copy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlannedFile {
    /// The file as the picker named it.
    pub source: String,
    /// Where it goes: its path inside the install folder.
    pub dest: String,
    /// Length in bytes.
    pub size: u64,
    /// Index of its group in [`ImportRules::groups`].
    pub group: usize,
}

/// What one group amounts to in the picked folder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GroupTotals {
    /// The group's id.
    pub id: String,
    /// What the player is shown.
    pub title: String,
    /// Whether the group is required.
    pub required: bool,
    /// Whether the plan includes it.
    pub enabled: bool,
    /// Files in the picked folder that belong to it.
    pub files: usize,
    /// Their total size.
    pub bytes: u64,
}

/// What will be copied.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Plan {
    /// The picked folder's prefix up to the install folder, removed from
    /// every path (empty when the picked folder is the install).
    pub root: String,
    /// Files to copy, group by group in path order.
    pub files: Vec<PlannedFile>,
    /// Every group's totals, enabled or not, so the player can see what an
    /// optional group would cost.
    pub groups: Vec<GroupTotals>,
}

/// Why a picked folder cannot be imported.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PlanError {
    /// There is no `<mod>/gameinfo.txt` in what was picked.
    #[error(
        "this folder does not look like Half-Life 2: Deathmatch: there is no {mod_dir}/gameinfo.txt \
         in it. Pick the install folder (the one that contains {mod_dir}), usually \
         steamapps/common/Half-Life 2 Deathmatch"
    )]
    NotAnInstall {
        /// The mod folder that was looked for.
        mod_dir: String,
    },
    /// A required group has no files, so the install is incomplete.
    #[error("the install is missing files the game needs: {title}")]
    MissingRequired {
        /// The group's title.
        title: String,
    },
}

impl Plan {
    /// Bytes to copy.
    #[must_use]
    pub fn total_bytes(&self) -> u64 {
        self.files.iter().map(|file| file.size).sum()
    }

    /// The files an earlier, interrupted import has not copied yet: those
    /// missing from `done`, or recorded with another size.
    #[must_use]
    pub fn pending(&self, done: &Manifest) -> Vec<&PlannedFile> {
        self.files
            .iter()
            .filter(|file| done.size(&file.dest) != Some(file.size))
            .collect()
    }
}

/// The prefix of `candidates` that leads to the install: the shortest one
/// after which `<mod_dir>/gameinfo.txt` follows. `None` if there is none.
#[must_use]
pub fn find_root(rules: &ImportRules, candidates: &[Candidate]) -> Option<String> {
    let suffix = format!("{}/gameinfo.txt", normalize(&rules.mod_dir));
    candidates
        .iter()
        .filter_map(|candidate| {
            let path = candidate.path.trim_start_matches('/');
            let normalised = normalize(path);
            let head = normalised.strip_suffix(&suffix)?;
            if !(head.is_empty() || head.ends_with('/')) {
                return None;
            }
            // Same length in bytes as the lowercase form for ASCII paths; for
            // others, fall back to counting segments, which always agrees.
            let segments = head.split('/').filter(|s| !s.is_empty()).count();
            let root: Vec<&str> = path.split('/').take(segments).collect();
            Some(root.join("/"))
        })
        .min_by_key(|root| {
            (
                root.split('/').filter(|s| !s.is_empty()).count(),
                root.clone(),
            )
        })
}

/// Plans an import of `candidates` under `rules`, including the optional
/// groups named in `enabled`.
///
/// # Errors
///
/// [`PlanError::NotAnInstall`] when no install folder can be found, and
/// [`PlanError::MissingRequired`] when a required group matches nothing.
pub fn plan(
    rules: &ImportRules,
    candidates: &[Candidate],
    enabled: &BTreeSet<String>,
) -> Result<Plan, PlanError> {
    let root = find_root(rules, candidates).ok_or_else(|| PlanError::NotAnInstall {
        mod_dir: rules.mod_dir.clone(),
    })?;
    let strip = if root.is_empty() {
        String::new()
    } else {
        format!("{root}/")
    };
    let compiled: Vec<(Vec<Glob>, Vec<Glob>)> = rules
        .groups
        .iter()
        .map(|group| {
            (
                group.include.iter().map(|p| Glob::new(p)).collect(),
                group.exclude.iter().map(|p| Glob::new(p)).collect(),
            )
        })
        .collect();

    let mut files = Vec::new();
    let mut groups: Vec<GroupTotals> = rules
        .groups
        .iter()
        .map(|group| GroupTotals {
            id: group.id.clone(),
            title: group.title.clone(),
            required: group.required,
            enabled: group.required || enabled.contains(&group.id),
            files: 0,
            bytes: 0,
        })
        .collect();
    for candidate in candidates {
        let source = candidate.path.trim_start_matches('/');
        let Some(dest) = source.strip_prefix(&strip) else {
            continue;
        };
        let canonical = normalize(dest);
        let claimed = compiled.iter().position(|(include, exclude)| {
            include.iter().any(|glob| glob.matches(&canonical))
                && !exclude.iter().any(|glob| glob.matches(&canonical))
        });
        let Some(group) = claimed else {
            continue;
        };
        groups[group].files += 1;
        groups[group].bytes += candidate.size;
        if groups[group].enabled {
            files.push(PlannedFile {
                source: candidate.path.clone(),
                dest: dest.to_owned(),
                size: candidate.size,
                group,
            });
        }
    }
    if let Some(missing) = groups
        .iter()
        .find(|group| group.required && group.files == 0)
    {
        return Err(PlanError::MissingRequired {
            title: missing.title.clone(),
        });
    }
    files.sort_by(|a, b| (a.group, &a.dest).cmp(&(b.group, &b.dest)));
    files.dedup_by(|a, b| a.dest == b.dest);
    Ok(Plan {
        root,
        files,
        groups,
    })
}

// These run natively; the browser build is tested in `tests/browser.rs`.
#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use proptest::prelude::*;

    use super::*;

    fn candidates(paths: &[(&str, u64)]) -> Vec<Candidate> {
        paths
            .iter()
            .map(|(path, size)| Candidate {
                path: (*path).to_owned(),
                size: *size,
            })
            .collect()
    }

    /// A small install, as the picker lists it, under `prefix`.
    fn install(prefix: &str) -> Vec<Candidate> {
        let files = [
            ("hl2mp/gameinfo.txt", 1_000),
            ("hl2mp/maps/dm_lockdown.bsp", 20_000_000),
            ("hl2mp/hl2mp_pak_dir.vpk", 100),
            ("hl2mp/hl2mp_pak_000.vpk", 5_000_000),
            ("hl2mp/bin/server.so", 18_000_000),
            ("hl2mp/bin/linux64/client.so", 18_000_000),
            ("hl2mp/hl2mp.log", 10),
            ("hl2/hl2_textures_dir.vpk", 400),
            ("hl2/hl2_textures_000.vpk", 100_000_000),
            ("hl2/hl2_misc_dir.vpk", 300),
            ("hl2/hl2_misc_000.vpk", 110_000_000),
            ("hl2/hl2_sound_misc_dir.vpk", 300),
            ("hl2/hl2_sound_misc_000.vpk", 128_000_000),
            ("hl2/hl2_sound_vo_english_000.vpk", 114_000_000),
            ("hl2_complete/hl2_complete_misc_dir.vpk", 50),
            ("hl2/cfg/config.cfg", 9),
            ("bin/engine.so", 80_000_000),
            ("platform/platform_misc_dir.vpk", 33_000_000),
            ("steam_appid.txt", 4),
        ];
        files
            .iter()
            .map(|(path, size)| Candidate {
                path: format!("{prefix}{path}"),
                size: *size,
            })
            .collect()
    }

    fn dests(plan: &Plan) -> Vec<&str> {
        plan.files.iter().map(|file| file.dest.as_str()).collect()
    }

    #[test]
    fn the_shipped_rules_are_valid() {
        let rules = ImportRules::builtin();
        assert_eq!(rules.mod_dir, "hl2mp");
        let ids: BTreeSet<_> = rules.groups.iter().map(|g| g.id.as_str()).collect();
        assert_eq!(ids.len(), rules.groups.len(), "group ids are unique");
        assert!(rules.groups.iter().any(|g| g.required));
        assert!(
            rules
                .groups
                .iter()
                .all(|g| !g.include.is_empty() && !g.title.is_empty())
        );
        // The voice-over is offered but starts off.
        assert!(!rules.default_enabled().contains("voice"));
        assert!(rules.default_enabled().contains("combined"));
    }

    #[test]
    fn unknown_fields_in_the_rules_are_rejected() {
        assert!(ImportRules::parse("(mod_dir: \"x\", groups: [], extra: 1)").is_err());
        assert!(ImportRules::parse("(mod_dir: \"x\", groups: [])").is_ok());
    }

    #[test]
    fn the_install_is_found_under_any_prefix() {
        let rules = ImportRules::builtin();
        for (prefix, root) in [
            ("", ""),
            ("Half-Life 2 Deathmatch/", "Half-Life 2 Deathmatch"),
            (
                "common/Half-Life 2 Deathmatch/",
                "common/Half-Life 2 Deathmatch",
            ),
        ] {
            assert_eq!(
                find_root(&rules, &install(prefix)).as_deref(),
                Some(root),
                "prefix {prefix:?}"
            );
        }
    }

    #[test]
    fn the_install_is_found_whatever_the_case() {
        let rules = ImportRules::builtin();
        let picked = candidates(&[("My Games/HL2MP/GameInfo.TXT", 1)]);
        assert_eq!(find_root(&rules, &picked).as_deref(), Some("My Games"));
    }

    #[test]
    fn the_nearest_install_wins_when_there_are_several() {
        let rules = ImportRules::builtin();
        let picked = candidates(&[
            ("lib/old/hl2mp/gameinfo.txt", 1),
            ("lib/new/hl2mp/gameinfo.txt", 1),
            ("lib/hl2mp/gameinfo.txt", 1),
        ]);
        assert_eq!(find_root(&rules, &picked).as_deref(), Some("lib"));
        // A look-alike folder name is not the mod folder.
        let picked = candidates(&[("lib/not_hl2mp/gameinfo.txt", 1)]);
        assert_eq!(find_root(&rules, &picked), None);
    }

    #[test]
    fn a_folder_that_is_not_the_install_is_refused_with_advice() {
        let rules = ImportRules::builtin();
        let error = plan(
            &rules,
            &candidates(&[("Downloads/cat.png", 5)]),
            &BTreeSet::new(),
        )
        .unwrap_err();
        assert!(matches!(error, PlanError::NotAnInstall { .. }));
        assert!(error.to_string().contains("steamapps/common"), "{error}");
    }

    #[test]
    fn the_plan_takes_what_the_game_needs_and_nothing_else() {
        let rules = ImportRules::builtin();
        let picked = install("Half-Life 2 Deathmatch/");
        let plan = plan(&rules, &picked, &rules.default_enabled()).unwrap();
        assert_eq!(plan.root, "Half-Life 2 Deathmatch");
        let dests = dests(&plan);
        for wanted in [
            "hl2mp/gameinfo.txt",
            "hl2mp/maps/dm_lockdown.bsp",
            "hl2mp/hl2mp_pak_000.vpk",
            "hl2/hl2_textures_000.vpk",
            "hl2/hl2_misc_dir.vpk",
            "hl2/hl2_sound_misc_000.vpk",
            "hl2_complete/hl2_complete_misc_dir.vpk",
        ] {
            assert!(dests.contains(&wanted), "{wanted} is imported: {dests:?}");
        }
        for unwanted in [
            "hl2mp/bin/server.so",
            "hl2mp/bin/linux64/client.so",
            "hl2mp/hl2mp.log",
            "hl2/hl2_sound_vo_english_000.vpk",
            "hl2/cfg/config.cfg",
            "bin/engine.so",
            "platform/platform_misc_dir.vpk",
            "steam_appid.txt",
        ] {
            assert!(!dests.contains(&unwanted), "{unwanted} is left out");
        }
        // Group by group: the small mod folder first, so the game can be
        // recognised early.
        assert_eq!(plan.files[0].group, 0);
        assert!(plan.files.windows(2).all(|w| w[0].group <= w[1].group));
    }

    #[test]
    fn an_optional_group_is_in_the_plan_only_when_enabled() {
        let rules = ImportRules::builtin();
        let picked = install("");
        let without = plan(&rules, &picked, &BTreeSet::new()).unwrap();
        let with = plan(&rules, &picked, &BTreeSet::from(["voice".to_owned()])).unwrap();
        let voice = "hl2/hl2_sound_vo_english_000.vpk";
        assert!(!dests(&without).contains(&voice));
        assert!(dests(&with).contains(&voice));
        assert_eq!(with.total_bytes(), without.total_bytes() + 114_000_000);
        // Either way the totals show what the group would cost.
        let group = |plan: &Plan| {
            plan.groups
                .iter()
                .find(|g| g.id == "voice")
                .cloned()
                .unwrap()
        };
        assert_eq!(group(&without).bytes, 114_000_000);
        assert!(!group(&without).enabled);
        assert!(group(&with).enabled);
    }

    #[test]
    fn a_required_group_with_no_files_makes_the_install_incomplete() {
        let rules = ImportRules::builtin();
        let picked = candidates(&[
            ("hl2mp/gameinfo.txt", 1),
            ("hl2mp/maps/a.bsp", 1),
            ("hl2/hl2_misc_dir.vpk", 1),
            ("hl2/hl2_sound_misc_dir.vpk", 1),
        ]);
        let error = plan(&rules, &picked, &BTreeSet::new()).unwrap_err();
        assert_eq!(
            error,
            PlanError::MissingRequired {
                title: "Textures".to_owned()
            }
        );
    }

    #[test]
    fn an_interrupted_import_resumes_where_it_stopped() {
        let rules = ImportRules::builtin();
        let plan = plan(&rules, &install(""), &rules.default_enabled()).unwrap();
        assert_eq!(plan.pending(&Manifest::new()).len(), plan.files.len());

        let mut done = Manifest::new();
        done.record("hl2mp/gameinfo.txt", 1_000);
        done.record("hl2mp/maps/dm_lockdown.bsp", 20_000_000);
        // Recorded with the wrong size: the copy must have been cut short.
        done.record("hl2mp/hl2mp_pak_dir.vpk", 7);
        let pending = plan.pending(&done);
        assert_eq!(pending.len(), plan.files.len() - 2);
        assert!(pending.iter().any(|f| f.dest == "hl2mp/hl2mp_pak_dir.vpk"));
        assert!(pending.iter().all(|f| f.dest != "hl2mp/gameinfo.txt"));
    }

    proptest! {
        /// Whatever is picked, every planned file is claimed by exactly one
        /// group, listed once, and the totals add up.
        #[test]
        fn plans_are_consistent(
            extra in proptest::collection::vec(("[a-z]{1,5}(/[a-z0-9_]{1,8}){0,3}\\.[a-z]{1,4}", 0_u64..1_000_000), 0..30),
        ) {
            let rules = ImportRules::builtin();
            let mut picked = install("");
            picked.extend(extra.into_iter().map(|(path, size)| Candidate { path, size }));
            let enabled = rules.default_enabled();
            let plan = plan(&rules, &picked, &enabled).unwrap();
            let mut seen = BTreeSet::new();
            for file in &plan.files {
                prop_assert!(seen.insert(normalize(&file.dest)), "{} twice", file.dest);
                prop_assert!(plan.groups[file.group].enabled);
            }
            let enabled_bytes: u64 = plan.groups.iter().filter(|g| g.enabled).map(|g| g.bytes).sum();
            // Files picked twice under different case collapse to one, so the
            // plan never copies more than the groups hold.
            prop_assert!(plan.total_bytes() <= enabled_bytes);
        }
    }
}
