//! FBX material mapping: textured slots, opacity, bump, zero scale.

use glam::Vec3;
use melon_core::{AlphaMode, ImportSettings};

use crate::ModelSummary;
use crate::fbx::fixture::Cube;
use crate::fbx::{FbxModel, Sources, import, import_plain, summary};

fn png(colour: [u8; 4]) -> Vec<u8> {
    let mut out = std::io::Cursor::new(Vec::new());
    image::RgbaImage::from_pixel(2, 2, image::Rgba(colour))
        .write_to(&mut out, image::ImageFormat::Png)
        .unwrap();
    out.into_inner()
}

fn load(cube: &Cube) -> FbxModel {
    let read = |_: &str| Err(std::io::ErrorKind::NotFound.into());
    import_plain(
        "assets/models/box.fbx",
        cube.ascii().as_bytes(),
        Some(&Sources::logging(&read, &[])),
    )
    .unwrap()
}

/// A cube whose material 0 has an embedded texture on `slots`.
fn textured(slots: &[&'static str], image: Vec<u8>, props: &[&str]) -> Cube {
    Cube {
        texture: Some("t.png"),
        embedded: Some(image),
        slots: slots.to_vec(),
        props: props.iter().map(|p| p.to_string()).collect(),
        ..Cube::default()
    }
}

const OPACITY: &str = r#"P: "TransparencyFactor", "Number", "", "A",0.0005"#;

#[test]
fn a_loaded_texture_makes_base_colour_and_emissive_white() {
    let model = load(&textured(
        &["DiffuseColor", "EmissiveColor"],
        png([9, 9, 9, 255]),
        &[],
    ));
    let m = &model.materials[0];
    assert!(m.textures[0].is_some() && m.textures[3].is_some());
    assert_eq!(m.asset.base_color, [1.0, 1.0, 1.0, 1.0]);
    assert_eq!(m.asset.emissive, [1.0, 1.0, 1.0]);
}

#[test]
fn without_a_texture_the_colour_factors_stay() {
    let m = &load(&Cube::default()).materials[0];
    assert!((m.asset.base_color[0] - 0.8).abs() < 1e-6);
    assert!((m.asset.emissive[2] - 0.3).abs() < 1e-6);
}

#[test]
fn opacity_just_below_one_stays_opaque_and_a_real_fraction_blends() {
    let model = load(&Cube {
        props: vec![OPACITY.into()],
        ..Cube::default()
    });
    assert_eq!(model.materials[0].asset.alpha_mode, AlphaMode::Opaque);
    let model = load(&Cube {
        props: vec![r#"P: "TransparencyFactor", "Number", "", "A",0.5"#.into()],
        ..Cube::default()
    });
    let asset = &model.materials[0].asset;
    assert_eq!(asset.alpha_mode, AlphaMode::Blend);
    assert!((asset.base_color[3] - 0.5).abs() < 1e-6);
}

#[test]
fn an_opacity_texture_cuts_out_instead_of_blending() {
    let model = load(&textured(&["TransparentColor"], png([255; 4]), &[]));
    assert_eq!(
        model.materials[0].asset.alpha_mode,
        AlphaMode::Mask { cutoff: 0.5 }
    );
}

#[test]
fn a_base_texture_with_see_through_texels_cuts_out_and_an_opaque_one_does_not() {
    let model = load(&textured(&["DiffuseColor"], png([9, 9, 9, 0]), &[]));
    assert_eq!(
        model.materials[0].asset.alpha_mode,
        AlphaMode::Mask { cutoff: 0.5 }
    );
    let model = load(&textured(&["DiffuseColor"], png([9, 9, 9, 255]), &[]));
    assert_eq!(model.materials[0].asset.alpha_mode, AlphaMode::Opaque);
}

#[test]
fn a_bump_texture_is_the_normal_map_when_there_is_no_normal_map() {
    let model = load(&textured(&["Bump"], png([128, 128, 255, 255]), &[]));
    assert!(model.materials[0].textures[2].is_some());
}

#[test]
fn a_zero_scale_node_draws_nothing_and_stays_finite() {
    let model = load(&Cube {
        scaling: [0.0; 3],
        ..Cube::default()
    });
    assert!(model.baked_parts().is_empty());
    assert!(model.part_materials().is_empty());
    assert!(model.bounds().is_none());
}

/// `load` with the warnings it gave.
fn load_warned(cube: &Cube) -> (FbxModel, Vec<String>) {
    let warnings = std::cell::RefCell::new(Vec::new());
    let read = |_: &str| Err(std::io::ErrorKind::NotFound.into());
    let warn = |_: &str, m: String| warnings.borrow_mut().push(m);
    let sources = Sources {
        read: &read,
        index: &[],
        warn: &warn,
    };
    let model = import_plain(
        "assets/models/box.fbx",
        cube.ascii().as_bytes(),
        Some(&sources),
    )
    .unwrap();
    (model, warnings.into_inner())
}

#[test]
fn a_glossiness_texture_is_not_bound_as_roughness_and_says_so_once() {
    let shininess = r#"P: "ShininessExponent", "Number", "", "A",20"#;
    let (model, warnings) = load_warned(&textured(
        &["ShininessExponent"],
        png([9, 9, 9, 255]),
        &[shininess],
    ));
    assert!(model.materials[0].textures[1].is_none());
    assert_eq!(warnings.len(), 1, "{warnings:?}");
    assert!(
        warnings[0].contains("assets/models/box.fbx")
            && warnings[0].contains("'t.png' is not used"),
        "{warnings:?}"
    );
}

#[test]
fn an_opacity_texture_over_an_opaque_base_texture_says_it_is_not_used() {
    let (_, warnings) = load_warned(&textured(
        &["DiffuseColor", "TransparentColor"],
        png([9, 9, 9, 255]),
        &[],
    ));
    assert_eq!(warnings.len(), 1, "{warnings:?}");
    assert!(warnings[0].contains("opacity texture"), "{warnings:?}");
}

#[test]
fn a_zero_scale_node_warns_once() {
    let (_, warnings) = load_warned(&Cube {
        scaling: [0.0; 3],
        ..Cube::default()
    });
    assert_eq!(warnings.len(), 1, "{warnings:?}");
    assert!(warnings[0].contains("zero scale"), "{warnings:?}");
}

#[test]
fn load_errors_name_the_file_and_read_as_text() {
    let error = import_plain("assets/models/bad.fbx", b"not an fbx", None)
        .err()
        .unwrap();
    let text = format!("{error:#}");
    assert!(text.contains("assets/models/bad.fbx"), "{text}");
    assert!(
        !text.contains("stack_size") && !text.contains("ErrorFrame"),
        "{text}"
    );
}

// Import settings (a model's `.meta`).

fn import_with(cube: &Cube, settings: &ImportSettings) -> FbxModel {
    import(
        "assets/models/box.fbx",
        cube.ascii().as_bytes(),
        settings,
        None,
    )
    .unwrap()
}

fn close(a: Vec3, b: Vec3) -> bool {
    a.abs_diff_eq(b, 1e-3)
}

#[test]
fn scale_multiplies_the_model_and_its_node_positions() {
    let settings = ImportSettings {
        scale: 2.0,
        ..Default::default()
    };
    let model = import_with(&Cube::default(), &settings);
    let (lo, hi) = model.bounds().unwrap();
    assert!(close(lo, Vec3::new(1.0, 2.0, 3.0)) && close(hi, Vec3::new(3.0, 6.0, 9.0)));
    assert!(close(model.nodes[0].translation, Vec3::new(2.0, 4.0, 6.0)));
    // `#mesh0` is in mesh space, scaled too.
    let part = &model.meshes[0][0];
    let max = part
        .vertices
        .iter()
        .map(|v| v.position[1])
        .fold(f32::MIN, f32::max);
    assert!((max - 2.0).abs() < 1e-4, "{max}");
}

#[test]
fn file_units_off_takes_the_numbers_as_metres() {
    let cube = Cube {
        z_up_cm: true,
        ..Cube::default()
    };
    let (lo, hi) = import_with(&cube, &ImportSettings::default())
        .bounds()
        .unwrap();
    assert!(close(hi - lo, Vec3::new(1.0, 2.0, 3.0)), "{}", hi - lo);
    let raw = ImportSettings {
        use_file_units: false,
        ..Default::default()
    };
    let (lo, hi) = import_with(&cube, &raw).bounds().unwrap();
    assert!(
        close(hi - lo, Vec3::new(100.0, 200.0, 300.0)),
        "{}",
        hi - lo
    );
}

#[test]
fn forward_flip_turns_the_model_half_a_turn_about_y() {
    let settings = ImportSettings {
        forward_flip: true,
        ..Default::default()
    };
    let model = import_with(&Cube::default(), &settings);
    let (lo, hi) = model.bounds().unwrap();
    assert!(close(lo, Vec3::new(-1.5, 1.0, -4.5)) && close(hi, Vec3::new(-0.5, 3.0, -1.5)));
    assert!(close(
        model.nodes[0].translation,
        Vec3::new(-1.0, 2.0, -3.0)
    ));
    assert!(
        (model.nodes[0].rotation * Vec3::Z).abs_diff_eq(-Vec3::Z, 1e-4),
        "root node turned"
    );
}

#[test]
fn the_summary_agrees_with_the_full_import_under_any_settings() {
    for settings in [
        ImportSettings::default(),
        ImportSettings {
            scale: 0.5,
            forward_flip: true,
            ..Default::default()
        },
    ] {
        for cube in [
            Cube::default(),
            Cube {
                z_up_cm: true,
                scaling: [-1.0, 2.0, 1.0],
                ..Cube::default()
            },
        ] {
            let full = import_with(&cube, &settings);
            let ModelSummary { nodes, bounds } =
                summary(cube.ascii().as_bytes(), &settings).unwrap();
            assert_eq!(nodes, full.nodes);
            let ((lo, hi), (slo, shi)) = (full.bounds().unwrap(), bounds.unwrap());
            assert!(close(lo, slo) && close(hi, shi), "{lo} {hi} vs {slo} {shi}");
        }
    }
}

#[test]
fn flipping_the_normal_map_inverts_green_only() {
    let img = image::DynamicImage::ImageRgba8(image::RgbaImage::from_pixel(
        1,
        1,
        image::Rgba([10, 20, 30, 40]),
    ));
    assert_eq!(
        crate::flip_green(&img).to_rgba8().get_pixel(0, 0).0,
        [10, 235, 30, 40]
    );
}
