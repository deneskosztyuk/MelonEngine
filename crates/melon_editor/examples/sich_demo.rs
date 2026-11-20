//! Writes `assets/scenes/sich_demo.ron`: a Sich village clearing built from
//! the models in `assets/demo/sich`, showing materials, sun and local-light
//! shadows, GI, physics (a log pile that tumbles on Play) and spatial audio.
//! Run from the repo root: `cargo run -p melon_editor --example sich_demo`.

use std::path::Path;

use glam::{Quat, Vec3};
use melon_core::{
    Aabb, AlphaMode, AssetServer, AudioSource, Collider, ColliderShape, Core, Handle,
    MaterialAsset, MaterialSlot, MeshMaterials, Model, Name, PointLight, RigidBody, RigidBodyKind,
    SpotLight, Transform,
};

const DIR: &str = "assets/demo/sich";

/// One placed model: file, position on the ground, yaw in degrees, uniform scale,
/// and its unscaled size (from the model's bounds) for the picking box.
struct Prop {
    file: &'static str,
    at: (f32, f32),
    yaw: f32,
    scale: f32,
    size: Vec3,
    /// Static triangle-mesh collider (buildings, big rocks).
    solid: bool,
}

fn prop(
    file: &'static str,
    at: (f32, f32),
    yaw: f32,
    scale: f32,
    size: [f32; 3],
    solid: bool,
) -> Prop {
    Prop {
        file,
        at,
        yaw,
        scale,
        size: Vec3::from(size),
        solid,
    }
}

fn pose(x: f32, y: f32, z: f32, yaw: f32, scale: f32) -> Transform {
    Transform {
        translation: Vec3::new(x, y, z),
        rotation: Quat::from_rotation_y(yaw.to_radians()),
        scale: Vec3::splat(scale),
    }
}

fn model(core: &mut Core, file: &str) -> Handle<Model> {
    core.world
        .resource_mut::<AssetServer>()
        .load(&format!("{DIR}/{file}"))
}

fn spawn_prop(core: &mut Core, name: &str, p: &Prop) {
    let handle = model(core, p.file);
    let mut e = core.world.spawn((
        Name(name.to_string()),
        pose(p.at.0, 0.0, p.at.1, p.yaw, p.scale),
        handle,
        Aabb {
            half_extents: p.size * 0.5,
        },
    ));
    if p.solid {
        e.insert(Collider {
            shape: ColliderShape::Mesh,
            ..Default::default()
        });
    }
}

fn material(path: &str, base: [f32; 4], roughness: f32, alpha: AlphaMode) {
    let m = MaterialAsset {
        base_color: base,
        metallic: 0.0,
        roughness,
        alpha_mode: alpha,
        ..Default::default()
    };
    m.save(Path::new(path)).expect("write material");
}

fn main() -> anyhow::Result<()> {
    let root = Path::new(".");
    std::fs::create_dir_all(format!("{DIR}/materials"))?;
    std::fs::create_dir_all("assets/scenes")?;
    material(
        &format!("{DIR}/materials/meadow.mat"),
        [0.30, 0.42, 0.17, 1.0],
        0.95,
        AlphaMode::Opaque,
    );
    material(
        &format!("{DIR}/materials/pond.mat"),
        [0.05, 0.14, 0.17, 0.85],
        0.05,
        AlphaMode::Blend,
    );

    let mut core = Core::new();
    let mut server = AssetServer::new(root.to_path_buf());
    // Write `.meta` files for the copied models, sounds and new materials so
    // the scene stores GUIDs, as an editor run would.
    melon_editor::asset_db::reconcile(&mut server);
    server.scan_metas();
    core.world.insert_resource(server);
    let cube = model_builtin(&mut core);

    // Ground: a 60 x 60 m meadow slab, top at y = 0, static box collider.
    let meadow = slot(&mut core, "materials/meadow.mat");
    core.world.spawn((
        Name("Meadow".into()),
        Transform {
            translation: Vec3::new(0.0, -0.5, 0.0),
            rotation: Quat::IDENTITY,
            scale: Vec3::new(60.0, 1.0, 60.0),
        },
        cube,
        Aabb {
            half_extents: Vec3::splat(0.5),
        },
        RigidBody {
            kind: RigidBodyKind::Fixed,
            ..Default::default()
        },
        Collider::default(),
        MeshMaterials {
            parts: vec![meadow],
        },
    ));
    // Golden hour into dusk: a low warm sun in the west-southwest, a deep
    // blue-violet zenith over an amber horizon, light warm haze.
    core.world.spawn((
        Name("Golden Hour Lighting".into()),
        Transform::default(),
        melon_core::SceneLighting {
            sun_elevation: 7.0,
            sun_azimuth: -115.0,
            sun_color: Vec3::new(1.35, 0.68, 0.32),
            ambient: 0.3,
            sky_ambient: Vec3::new(0.55, 0.52, 0.85),
            ground_ambient: Vec3::new(0.6, 0.4, 0.26),
            zenith_color: Vec3::new(0.09, 0.1, 0.3),
            horizon_color: Vec3::new(1.05, 0.55, 0.3),
            fog_density: 0.0025,
            bloom_intensity: 0.12,
        },
    ));
    // Pond: a thin blended slab with reeds and lily pads.
    let pond = slot(&mut core, "materials/pond.mat");
    core.world.spawn((
        Name("Pond".into()),
        Transform {
            translation: Vec3::new(13.5, 0.02, 14.0),
            rotation: Quat::from_rotation_y(0.4),
            scale: Vec3::new(8.0, 0.02, 5.5),
        },
        cube,
        Aabb {
            half_extents: Vec3::splat(0.5),
        },
        MeshMaterials { parts: vec![pond] },
    ));

    // Buildings (scaled to a consistent ~2 m door height), static mesh colliders.
    let buildings = [
        (
            "Khata",
            prop(
                "khata.glb",
                (-10.0, -6.0),
                30.0,
                4.5,
                [1.43, 1.36, 1.88],
                true,
            ),
        ),
        (
            "Barracks",
            prop(
                "barracks.glb",
                (8.5, -11.0),
                -15.0,
                1.0,
                [11.1, 7.01, 7.45],
                true,
            ),
        ),
        (
            "Stockhouse",
            prop(
                "stockhouse.glb",
                (11.5, 3.0),
                -90.0,
                1.0,
                [6.27, 4.1, 4.29],
                true,
            ),
        ),
        (
            "Tool Workshop",
            prop(
                "tool_workshop.glb",
                (-11.0, 9.0),
                125.0,
                1.0,
                [8.03, 5.33, 8.01],
                true,
            ),
        ),
        (
            "Woodcutter Hut",
            prop(
                "woodcutter_hut.glb",
                (2.0, 12.5),
                180.0,
                2.0,
                [2.88, 2.15, 2.46],
                true,
            ),
        ),
        (
            "Hunter Hut",
            prop(
                "hunter_hut/hunter_hut.gltf",
                (-2.5, -14.5),
                10.0,
                1.0,
                [5.78, 5.31, 4.76],
                true,
            ),
        ),
        (
            "Gatherer Hut",
            prop(
                "gatherer_hut/gatherer_hut.gltf",
                (-17.0, -1.0),
                90.0,
                1.0,
                [5.81, 5.31, 4.79],
                true,
            ),
        ),
        (
            "Angler Hut",
            prop(
                "angler_hut.glb",
                (17.5, 9.0),
                -110.0,
                0.8,
                [7.1, 5.32, 7.19],
                true,
            ),
        ),
        (
            "Well",
            prop("well.glb", (0.0, 0.0), 0.0, 1.0, [2.16, 2.73, 1.54], true),
        ),
        (
            "Wood Splitting Station",
            prop(
                "wood_splitting_station.glb",
                (-4.5, 5.5),
                20.0,
                1.0,
                [1.99, 1.79, 0.77],
                true,
            ),
        ),
        (
            "Starter Stockpile",
            prop(
                "starter_stockpile.glb",
                (4.5, -3.5),
                -30.0,
                1.6,
                [0.95, 0.46, 0.73],
                true,
            ),
        ),
    ];
    for (name, p) in &buildings {
        spawn_prop(&mut core, name, p);
    }

    // Trees around the clearing, inside the DDGI volume (about +-20 m).
    let trees = [
        (
            "Oak",
            "oak_tree.glb",
            (-18.0, -16.0),
            0.0,
            2.3,
            [4.26, 4.38, 4.47],
        ),
        (
            "Oak",
            "oak_tree.glb",
            (17.5, -18.0),
            70.0,
            2.1,
            [4.26, 4.38, 4.47],
        ),
        (
            "Oak",
            "oak_tree.glb",
            (-18.5, 15.5),
            140.0,
            2.4,
            [4.26, 4.38, 4.47],
        ),
        (
            "Alder",
            "alder_tree.glb",
            (-7.0, -18.5),
            30.0,
            2.2,
            [3.42, 4.37, 3.33],
        ),
        (
            "Alder",
            "alder_tree.glb",
            (18.5, -4.0),
            200.0,
            2.0,
            [3.42, 4.37, 3.33],
        ),
        (
            "Birch",
            "birch_tree.glb",
            (4.0, -18.0),
            0.0,
            2.4,
            [1.97, 4.37, 1.91],
        ),
        (
            "Birch",
            "birch_tree.glb",
            (-15.0, 6.0),
            90.0,
            2.2,
            [1.97, 4.37, 1.91],
        ),
        (
            "Birch",
            "birch_tree.glb",
            (-5.5, 18.0),
            45.0,
            2.5,
            [1.97, 4.37, 1.91],
        ),
        (
            "Weeping Willow",
            "weeping_willow.glb",
            (9.0, 18.5),
            0.0,
            2.3,
            [3.84, 4.38, 3.9],
        ),
        (
            "Weeping Willow",
            "weeping_willow.glb",
            (19.0, 17.0),
            120.0,
            2.0,
            [3.84, 4.38, 3.9],
        ),
    ];
    for (name, file, at, yaw, scale, size) in trees {
        spawn_prop(&mut core, name, &prop(file, at, yaw, scale, size, false));
    }

    // Ground cover and small props.
    let small = [
        (
            "Berry Bush",
            "berry_bush.glb",
            (-6.0, -9.0),
            0.0,
            1.2,
            [1.08, 0.88, 1.19],
        ),
        (
            "Berry Bush",
            "berry_bush.glb",
            (6.5, 7.5),
            60.0,
            1.0,
            [1.08, 0.88, 1.19],
        ),
        (
            "Shrub",
            "sich_woodland_shrub.glb",
            (-14.0, -10.5),
            0.0,
            1.3,
            [0.91, 1.04, 0.92],
        ),
        (
            "Shrub",
            "sich_woodland_shrub.glb",
            (14.5, -9.0),
            90.0,
            1.1,
            [0.91, 1.04, 0.92],
        ),
        (
            "Shrub",
            "sich_woodland_shrub.glb",
            (-8.5, 15.0),
            30.0,
            1.2,
            [0.91, 1.04, 0.92],
        ),
        (
            "Fern",
            "sich_woodland_fern.glb",
            (-16.5, -14.0),
            0.0,
            1.2,
            [1.4, 0.37, 1.11],
        ),
        (
            "Fern",
            "sich_woodland_fern.glb",
            (15.5, -16.0),
            50.0,
            1.2,
            [1.4, 0.37, 1.11],
        ),
        (
            "Fern",
            "sich_woodland_fern.glb",
            (-17.0, 13.0),
            100.0,
            1.3,
            [1.4, 0.37, 1.11],
        ),
        (
            "Wildflowers",
            "sich_wildflowers_clover.glb",
            (3.0, 6.0),
            0.0,
            1.5,
            [0.53, 0.38, 0.5],
        ),
        (
            "Wildflowers",
            "sich_wildflowers_clover.glb",
            (-3.0, -7.5),
            80.0,
            1.5,
            [0.53, 0.38, 0.5],
        ),
        (
            "Wildflowers",
            "sich_wildflowers_clover.glb",
            (7.5, -5.5),
            160.0,
            1.5,
            [0.53, 0.38, 0.5],
        ),
        (
            "Mushrooms",
            "woodland_mushroom.glb",
            (-17.0, -12.0),
            0.0,
            1.0,
            [0.75, 0.42, 0.74],
        ),
        (
            "Mushrooms",
            "woodland_mushroom.glb",
            (16.5, -15.5),
            40.0,
            1.0,
            [0.75, 0.42, 0.74],
        ),
        (
            "Mossy Log",
            "sich_broken_mossy_log.glb",
            (-12.5, -14.0),
            25.0,
            1.4,
            [1.1, 0.42, 0.29],
        ),
        (
            "Fallen Branch",
            "sich_forked_fallen_branch.glb",
            (12.0, -14.5),
            70.0,
            1.4,
            [0.83, 0.32, 0.4],
        ),
        (
            "Twigs",
            "sich_twig_cluster.glb",
            (-3.5, 4.5),
            0.0,
            1.0,
            [0.66, 0.08, 0.3],
        ),
        (
            "Reeds",
            "sich_reeds.glb",
            (10.0, 12.0),
            0.0,
            1.3,
            [0.8, 1.3, 0.76],
        ),
        (
            "Reeds",
            "sich_reeds.glb",
            (16.5, 16.5),
            60.0,
            1.3,
            [0.8, 1.3, 0.76],
        ),
        (
            "Cattails",
            "sich_cattails.glb",
            (11.0, 16.5),
            0.0,
            1.3,
            [0.7, 1.2, 0.7],
        ),
        (
            "Cattails",
            "sich_cattails.glb",
            (17.0, 12.5),
            90.0,
            1.2,
            [0.7, 1.2, 0.7],
        ),
        (
            "Deer",
            "deer.glb",
            (-15.5, -11.5),
            140.0,
            1.0,
            [0.62, 1.92, 1.55],
        ),
        (
            "Wild Boar",
            "wild_boar.glb",
            (7.0, 15.0),
            -60.0,
            0.75,
            [0.85, 1.26, 1.95],
        ),
    ];
    for (name, file, at, yaw, scale, size) in small {
        spawn_prop(&mut core, name, &prop(file, at, yaw, scale, size, false));
    }
    let rocks = [
        ((-6.5, 10.5), 20.0, 1.6),
        ((14.0, -3.0), 110.0, 1.3),
        ((-19.0, 4.0), 60.0, 1.8),
    ];
    for (at, yaw, scale) in rocks {
        spawn_prop(
            &mut core,
            "Rocks",
            &prop(
                "terrain_rocks/steppe_woodland_rocks.gltf",
                at,
                yaw,
                scale,
                [3.47, 0.25, 1.66],
                true,
            ),
        );
    }
    // Lily pads float on the pond.
    for (i, (x, z)) in [(12.0, 13.0), (14.5, 15.0), (15.0, 12.8)]
        .into_iter()
        .enumerate()
    {
        let handle = model(&mut core, "sich_lily_pads.glb");
        core.world.spawn((
            Name("Lily Pads".into()),
            pose(x, 0.035, z, i as f32 * 70.0, 1.2),
            handle,
            Aabb {
                half_extents: Vec3::new(0.45, 0.01, 0.37),
            },
        ));
    }
    // Tools at the splitting station and barracks.
    let tools = [
        (
            "Axe",
            "axe.glb",
            Vec3::new(-4.0, 0.0, 6.4),
            80.0,
            [0.34, 1.0, 0.37],
        ),
        (
            "Hammer",
            "hammer.glb",
            Vec3::new(4.2, 0.82, -3.3),
            30.0,
            [0.06, 0.51, 0.18],
        ),
        (
            "Musket",
            "musket.glb",
            Vec3::new(5.2, 0.22, -7.6),
            -15.0,
            [0.16, 0.43, 1.58],
        ),
    ];
    for (name, file, at, yaw, size) in tools {
        let handle = model(&mut core, file);
        core.world.spawn((
            Name(name.into()),
            pose(at.x, at.y, at.z, yaw, 1.0),
            handle,
            Aabb {
                half_extents: Vec3::from(size) * 0.5,
            },
        ));
    }

    // Physics: a pile of cut logs held loosely in the air; press Play and it
    // tumbles. Each log is a body with a box collider; the mesh is a child,
    // lowered so the log sits centred in its box (the model's origin is at
    // its base).
    let log_size = Vec3::new(0.73, 0.21, 0.22) * 1.6;
    for i in 0..9 {
        let row = (i / 3) as f32;
        let col = (i % 3) as f32;
        let body = core
            .world
            .spawn((
                Name(format!("Firewood {}", i + 1)),
                Transform {
                    translation: Vec3::new(
                        -6.2 + (row * 0.13),
                        1.6 + row * 0.9,
                        3.2 + col * 0.42 + (row * 0.08),
                    ),
                    rotation: Quat::from_euler(
                        glam::EulerRot::YXZ,
                        (row * 31.0 + col * 17.0).to_radians(),
                        (col * 12.0 - 12.0).to_radians(),
                        (row * 9.0).to_radians(),
                    ),
                    scale: Vec3::ONE,
                },
                RigidBody {
                    mass: 12.0,
                    ..Default::default()
                },
                Collider {
                    shape: ColliderShape::Box {
                        half_extents: log_size * 0.5,
                    },
                    friction: 0.8,
                    ..Default::default()
                },
            ))
            .id();
        let handle = model(&mut core, "sich_short_cut_log.glb");
        let mesh = core
            .world
            .spawn((
                Name("Log Mesh".into()),
                pose(0.0, -log_size.y * 0.5, 0.0, 0.0, 1.6),
                handle,
                Aabb {
                    half_extents: Vec3::new(0.365, 0.105, 0.11),
                },
            ))
            .id();
        core.world
            .entity_mut(mesh)
            .insert(melon_core::ChildOf(body));
    }

    // Local lights: warm lanterns with shadows, one spot over the barracks door.
    let lantern = Vec3::new(1.0, 0.62, 0.3);
    for (name, at) in [
        ("Well Lantern", Vec3::new(1.8, 1.9, 1.4)),
        ("Khata Lantern", Vec3::new(-5.6, 2.1, -1.8)),
        ("Workshop Lantern", Vec3::new(-6.6, 2.3, 4.9)),
    ] {
        core.world.spawn((
            Name(name.into()),
            Transform::from_translation(at),
            PointLight {
                color: lantern,
                intensity: 34.0,
                range: 9.0,
                cast_shadows: true,
            },
        ));
    }
    core.world.spawn((
        Name("Barracks Spotlight".into()),
        Transform {
            translation: Vec3::new(7.5, 5.5, -6.0),
            rotation: Quat::from_rotation_x(-70f32.to_radians()),
            scale: Vec3::ONE,
        },
        SpotLight {
            color: Vec3::new(1.0, 0.85, 0.6),
            intensity: 80.0,
            range: 14.0,
            inner_angle: 0.3,
            outer_angle: 0.55,
            cast_shadows: true,
        },
    ));

    // Audio: forest ambience everywhere, a crackling lantern at the well and
    // a stream at the pond (both spatial).
    let audio = |clip: &str| format!("{DIR}/audio/{clip}");
    core.world.spawn((
        Name("Forest Ambience".into()),
        Transform::default(),
        AudioSource {
            clip: audio("forest_day.ogg"),
            volume: 0.45,
            looping: true,
            bus: melon_core::audio::MUSIC.into(),
            ..Default::default()
        },
    ));
    core.world.spawn((
        Name("Torch Crackle".into()),
        Transform::from_translation(Vec3::new(1.8, 1.9, 1.4)),
        AudioSource {
            clip: audio("torch_loop.ogg"),
            volume: 0.8,
            looping: true,
            spatial: true,
            min_distance: 1.5,
            max_distance: 14.0,
            ..Default::default()
        },
    ));
    core.world.spawn((
        Name("Stream".into()),
        Transform::from_translation(Vec3::new(13.5, 0.3, 14.0)),
        AudioSource {
            clip: audio("river_stream.ogg"),
            volume: 0.7,
            looping: true,
            spatial: true,
            min_distance: 2.0,
            max_distance: 22.0,
            ..Default::default()
        },
    ));

    // Drop the log pile for 5 s of fixed steps, then save the poses it
    // settles in, so the editor shows a natural pile instead of logs in the
    // air (they rest on the meadow; nothing sinks or flies off).
    core.world
        .insert_resource(melon_physics::PhysicsWorld::new());
    melon_physics::add_physics_systems(&mut core);
    core.world
        .insert_resource(melon_core::SimulationState::Playing);
    for _ in 0..300 {
        core.world
            .resource_mut::<melon_core::Time>()
            .update(1.0 / 60.0);
        core.update();
    }
    core.world
        .insert_resource(melon_core::SimulationState::Editing);
    let mut query = core.world.query::<(&Name, &Transform)>();
    for (name, t) in query.iter(&core.world) {
        if name.0.starts_with("Firewood") {
            println!(
                "{}: ({:.2}, {:.2}, {:.2})",
                name.0, t.translation.x, t.translation.y, t.translation.z
            );
        }
    }

    melon_core::scene::save_scene(&mut core.world, Path::new("assets/scenes/sich_demo.ron"))?;
    println!("wrote assets/scenes/sich_demo.ron");
    Ok(())
}

fn model_builtin(core: &mut Core) -> Handle<Model> {
    core.world
        .resource_mut::<AssetServer>()
        .load("builtin:cube")
}

fn slot(core: &mut Core, rel: &str) -> MaterialSlot {
    MaterialSlot::for_path(
        core.world.resource::<AssetServer>(),
        &format!("{DIR}/{rel}"),
    )
}
