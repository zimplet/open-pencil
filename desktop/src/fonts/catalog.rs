//! System font discovery through fontique, which reads each face's attributes from the font
//! files the platform reports.

use fontique::{Blob, Collection, CollectionOptions, FontInfo, FontStyle, SourceCache};
use std::collections::BTreeMap;
use std::sync::{Mutex, OnceLock};

use super::style::named_weight;

struct Catalog {
    collection: Collection,
    cache: SourceCache,
}

static CATALOG: OnceLock<Mutex<Catalog>> = OnceLock::new();

fn with_catalog<T>(f: impl FnOnce(&mut Catalog) -> T) -> T {
    let catalog = CATALOG.get_or_init(|| {
        Mutex::new(Catalog {
            collection: Collection::new(CollectionOptions {
                shared: false,
                system_fonts: true,
            }),
            cache: SourceCache::default(),
        })
    });
    let mut catalog = catalog
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    f(&mut catalog)
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FaceTraits {
    pub weight: f32,
    pub italic: bool,
    /// The range of a variable font's `wght` axis.
    pub weight_axis: Option<(f32, f32)>,
}

impl FaceTraits {
    fn of(font: &FontInfo) -> Self {
        Self {
            weight: font.weight().value(),
            italic: !matches!(font.style(), FontStyle::Normal),
            weight_axis: font
                .axes()
                .iter()
                .find(|axis| axis.tag == *b"wght")
                .map(|axis| (axis.min, axis.max)),
        }
    }
}

pub struct Face {
    pub data: Blob<u8>,
    pub index: u32,
}

/// Picks the face listed under the requested named weight and slant, or a variable face whose
/// weight axis covers the request. Other static weights are not substituted, so callers can report
/// the substitution.
pub fn choose_face(faces: &[FaceTraits], weight: f32, italic: bool) -> Option<usize> {
    let same_slant = |face: &&FaceTraits| face.italic == italic;
    faces
        .iter()
        .position(|face| same_slant(&face) && named_weight(face.weight) == weight)
        .or_else(|| {
            faces.iter().position(|face| {
                same_slant(&face)
                    && face
                        .weight_axis
                        .is_some_and(|(min, max)| (min..=max).contains(&weight))
            })
        })
}

/// Widths a design file names in a family, by the ratio fontique reads from `usWidthClass`.
const WIDTH_NAMES: [(f32, &str); 8] = [
    (0.5, "Ultra Condensed"),
    (0.625, "Extra Condensed"),
    (0.75, "Condensed"),
    (0.875, "Semi Condensed"),
    (1.125, "Semi Expanded"),
    (1.25, "Expanded"),
    (1.5, "Extra Expanded"),
    (2.0, "Ultra Expanded"),
];

/// Width ratios closer than this are the same width.
const WIDTH_TOLERANCE: f32 = 0.01;

fn width_name(ratio: f32) -> Option<&'static str> {
    WIDTH_NAMES
        .iter()
        .find(|(named, _)| (named - ratio).abs() < WIDTH_TOLERANCE)
        .map(|(_, name)| *name)
}

/// A face as the catalog sees it: its width next to the traits `choose_face` weighs.
#[derive(Clone, Copy, Debug, PartialEq)]
struct PlatformFace {
    width: f32,
    traits: FaceTraits,
}

/// A family as the platform lists it, with its faces in the platform's order.
type PlatformFamily = (String, Vec<PlatformFace>);

/// Splits a platform family into the families a design file names. Faces of normal width keep the
/// family's name; a named width is appended to it, so the extra condensed faces a platform lists
/// under `Proxima Nova` become `Proxima Nova Extra Condensed`, which is what the font's own
/// typographic family says. A name that already says the width is kept, and a family whose faces
/// all share one width is not split. Returns each name with the indices of its faces.
fn split_by_width(family: &str, widths: &[f32]) -> Vec<(String, Vec<usize>)> {
    let mixed = widths
        .windows(2)
        .any(|pair| (pair[0] - pair[1]).abs() >= WIDTH_TOLERANCE);
    let mut groups: Vec<(String, Vec<usize>)> = Vec::new();
    for (index, width) in widths.iter().enumerate() {
        let name = match width_name(*width) {
            Some(label) if mixed && !family.to_lowercase().contains(&label.to_lowercase()) => {
                format!("{family} {label}")
            }
            _ => family.to_string(),
        };
        match groups.iter_mut().find(|(existing, _)| *existing == name) {
            Some((_, indices)) => indices.push(index),
            None => groups.push((name, vec![index])),
        }
    }
    groups
}

/// The platform families whose faces can sit under a design file's family name: the name itself,
/// and the family a width was appended to.
fn source_families(name: &str) -> Vec<&str> {
    let mut sources = vec![name];
    for (_, width) in WIDTH_NAMES {
        if let Some(base) = name
            .strip_suffix(width)
            .and_then(|rest| rest.strip_suffix(' '))
        {
            sources.push(base);
        }
    }
    sources
}

/// Family names as a design file knows them, with the traits of each face.
fn advertised(platform: &[PlatformFamily]) -> BTreeMap<String, Vec<FaceTraits>> {
    let mut families: BTreeMap<String, Vec<FaceTraits>> = BTreeMap::new();
    for (name, faces) in platform {
        let widths: Vec<f32> = faces.iter().map(|face| face.width).collect();
        for (display, indices) in split_by_width(name, &widths) {
            families
                .entry(display)
                .or_default()
                .extend(indices.into_iter().map(|index| faces[index].traits));
        }
    }
    families
}

/// The faces behind `requested` as platform family, index and traits. `platform` holds the
/// families `source_families` names for it.
fn resolved(requested: &str, platform: &[PlatformFamily]) -> Vec<(String, usize, FaceTraits)> {
    let mut faces = Vec::new();
    for (name, platform_faces) in platform {
        let widths: Vec<f32> = platform_faces.iter().map(|face| face.width).collect();
        for (display, indices) in split_by_width(name, &widths) {
            if display == requested {
                faces.extend(
                    indices
                        .into_iter()
                        .map(|index| (name.clone(), index, platform_faces[index].traits)),
                );
            }
        }
    }
    faces
}

fn platform_family(catalog: &mut Catalog, name: &str) -> Option<PlatformFamily> {
    let family = catalog.collection.family_by_name(name)?;
    let faces = family
        .fonts()
        .iter()
        .map(|font| PlatformFace {
            width: font.width().ratio(),
            traits: FaceTraits::of(font),
        })
        .collect();
    Some((name.to_string(), faces))
}

/// Visible family names with the traits of each face.
pub fn families() -> Vec<(String, Vec<FaceTraits>)> {
    with_catalog(|catalog| {
        let names: Vec<String> = catalog
            .collection
            .family_names()
            .filter(|name| !name.starts_with('.'))
            .map(String::from)
            .collect();
        let platform: Vec<PlatformFamily> = names
            .iter()
            .filter_map(|name| platform_family(catalog, name))
            .collect();
        advertised(&platform).into_iter().collect()
    })
}

pub fn load_face(family: &str, weight: f32, italic: bool) -> Option<Face> {
    with_catalog(|catalog| {
        let platform: Vec<PlatformFamily> = source_families(family)
            .into_iter()
            .filter_map(|name| platform_family(catalog, name))
            .collect();
        let faces = resolved(family, &platform);
        let traits: Vec<FaceTraits> = faces.iter().map(|(_, _, traits)| *traits).collect();
        let (name, index, _) = &faces[choose_face(&traits, weight, italic)?];
        let family = catalog.collection.family_by_name(name)?;
        let font = &family.fonts()[*index];
        Some(Face {
            data: font.load(Some(&mut catalog.cache))?,
            index: font.index(),
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn face(weight: f32, italic: bool) -> FaceTraits {
        FaceTraits {
            weight,
            italic,
            weight_axis: None,
        }
    }

    #[test]
    fn chooses_the_exact_weight_and_slant() {
        let faces = [
            face(400.0, false),
            face(400.0, true),
            face(700.0, false),
            face(700.0, true),
        ];
        assert_eq!(choose_face(&faces, 700.0, true), Some(3));
        assert_eq!(choose_face(&faces, 400.0, false), Some(0));
    }

    #[test]
    fn does_not_substitute_a_nearby_static_weight() {
        assert_eq!(choose_face(&[face(400.0, false)], 700.0, false), None);
        assert_eq!(choose_face(&[face(400.0, true)], 400.0, false), None);
    }

    #[test]
    fn chooses_static_faces_by_the_style_they_are_listed_under() {
        assert_eq!(choose_face(&[face(450.0, false)], 400.0, false), Some(0));
        assert_eq!(choose_face(&[face(450.0, false)], 500.0, false), None);
        assert_eq!(choose_face(&[face(510.0, false)], 500.0, false), Some(0));
    }

    #[test]
    fn uses_a_variable_face_whose_weight_axis_covers_the_request() {
        let variable = FaceTraits {
            weight_axis: Some((1.0, 1000.0)),
            ..face(400.0, false)
        };
        let italic = FaceTraits {
            italic: true,
            ..variable
        };
        assert_eq!(choose_face(&[variable, italic], 500.0, false), Some(0));
        assert_eq!(choose_face(&[variable, italic], 700.0, true), Some(1));
        let narrow = FaceTraits {
            weight_axis: Some((300.0, 600.0)),
            ..variable
        };
        assert_eq!(choose_face(&[narrow], 700.0, false), None);
    }

    fn platform_face(width: f32, weight: f32, italic: bool) -> PlatformFace {
        PlatformFace {
            width,
            traits: face(weight, italic),
        }
    }

    fn entry(name: &str, indices: &[usize]) -> (String, Vec<usize>) {
        (name.to_string(), indices.to_vec())
    }

    /// The faces of Proxima Nova on Windows: the platform lists the upright extra condensed faces
    /// under `Proxima Nova`, and the italic ones, some of normal width, under their own name.
    fn proxima() -> Vec<PlatformFamily> {
        vec![
            (
                "Proxima Nova".to_string(),
                vec![
                    platform_face(1.0, 400.0, false),
                    platform_face(1.0, 900.0, false),
                    platform_face(0.625, 400.0, false),
                    platform_face(0.625, 600.0, false),
                    platform_face(0.625, 900.0, false),
                ],
            ),
            (
                "Proxima Nova Extra Condensed".to_string(),
                vec![
                    platform_face(1.0, 900.0, true),
                    platform_face(0.625, 600.0, true),
                ],
            ),
        ]
    }

    fn traits_of(faces: &[(String, usize, FaceTraits)]) -> Vec<FaceTraits> {
        faces.iter().map(|(_, _, traits)| *traits).collect()
    }

    #[test]
    fn leaves_a_family_of_one_width_alone() {
        assert_eq!(
            split_by_width("Inter", &[1.0, 1.0, 1.0]),
            [entry("Inter", &[0, 1, 2])]
        );
        assert_eq!(
            split_by_width("Arial Narrow", &[0.75, 0.75]),
            [entry("Arial Narrow", &[0, 1])]
        );
        assert!(split_by_width("Empty", &[]).is_empty());
    }

    #[test]
    fn names_a_width_that_shares_a_family_with_normal_faces() {
        assert_eq!(
            split_by_width("Proxima Nova", &[1.0, 1.0, 0.625, 0.625]),
            [
                entry("Proxima Nova", &[0, 1]),
                entry("Proxima Nova Extra Condensed", &[2, 3])
            ]
        );
    }

    #[test]
    fn does_not_repeat_a_width_the_name_already_has() {
        assert_eq!(
            split_by_width("Proxima Nova Extra Condensed", &[1.0, 0.625, 0.625]),
            [entry("Proxima Nova Extra Condensed", &[0, 1, 2])]
        );
    }

    #[test]
    fn keeps_widths_without_a_name_in_the_family() {
        assert_eq!(split_by_width("Foo", &[1.0, 0.9]), [entry("Foo", &[0, 1])]);
    }

    #[test]
    fn finds_the_family_a_width_was_appended_to() {
        assert_eq!(
            source_families("Foo Semi Condensed"),
            ["Foo Semi Condensed", "Foo Semi", "Foo"]
        );
        assert_eq!(source_families("Inter"), ["Inter"]);
        assert_eq!(source_families("Condensed"), ["Condensed"]);
    }

    #[test]
    fn looks_up_every_family_a_name_could_have_been_built_from() {
        // `Condensed` is a width of its own, so `Proxima Nova Extra` is a candidate next to
        // `Proxima Nova`; it only matters when such a family exists.
        assert_eq!(
            source_families("Proxima Nova Extra Condensed"),
            [
                "Proxima Nova Extra Condensed",
                "Proxima Nova",
                "Proxima Nova Extra"
            ]
        );
    }

    #[test]
    fn advertises_condensed_faces_next_to_the_italics_that_carry_the_name() {
        let families = advertised(&proxima());
        assert_eq!(families["Proxima Nova"].len(), 2);
        assert_eq!(families["Proxima Nova Extra Condensed"].len(), 5);
    }

    #[test]
    fn resolves_an_upright_condensed_face_from_the_family_it_is_listed_under() {
        let faces = resolved("Proxima Nova Extra Condensed", &proxima());
        let traits = traits_of(&faces);
        let (name, index, _) = &faces[choose_face(&traits, 900.0, false).unwrap()];
        assert_eq!((name.as_str(), *index), ("Proxima Nova", 4));
        let (name, index, _) = &faces[choose_face(&traits, 900.0, true).unwrap()];
        assert_eq!((name.as_str(), *index), ("Proxima Nova Extra Condensed", 0));
    }

    #[test]
    fn keeps_the_normal_width_family_to_its_own_faces() {
        let faces = resolved("Proxima Nova", &proxima());
        let traits = traits_of(&faces);
        let (name, index, _) = &faces[choose_face(&traits, 900.0, false).unwrap()];
        assert_eq!((name.as_str(), *index), ("Proxima Nova", 1));
        assert_eq!(choose_face(&traits, 600.0, false), None);
    }

    #[test]
    fn resolves_every_face_it_advertises() {
        let platform = proxima();
        for (family, advertised_faces) in advertised(&platform) {
            let traits = traits_of(&resolved(&family, &platform));
            for advertised_face in advertised_faces {
                let chosen = choose_face(&traits, advertised_face.weight, advertised_face.italic);
                assert!(chosen.is_some(), "{family} {advertised_face:?}");
            }
        }
    }
}
