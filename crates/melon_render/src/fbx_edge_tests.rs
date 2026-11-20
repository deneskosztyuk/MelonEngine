//! Edge-case tests for the FBX importer: a small FBX writer (ASCII, binary
//! 7.4, hand-written 6.1) over a tree of nodes, and the cases the cube
//! fixture cannot reach.

use std::cell::{Cell, RefCell};

use glam::{EulerRot, Mat4, Quat, Vec3};

use crate::Vertex;
use crate::fbx::{FbxModel, import};

enum P {
    I(i32),
    L(i64),
    D(f64),
    S(String),
    Dv(Vec<f64>),
    Iv(Vec<i32>),
}

struct N {
    name: &'static str,
    props: Vec<P>,
    kids: Vec<N>,
}

fn n(name: &'static str, props: Vec<P>, kids: Vec<N>) -> N {
    N { name, props, kids }
}

fn s(v: &str) -> P {
    P::S(v.to_string())
}

fn ascii(nodes: &[N]) -> String {
    fn list<T: ToString>(v: &[T]) -> String {
        let items: Vec<String> = v.iter().map(|x| x.to_string()).collect();
        format!("*{} {{ a: {} }}", items.len(), items.join(","))
    }
    fn node(out: &mut String, n: &N) {
        let props: Vec<String> = n
            .props
            .iter()
            .map(|p| match p {
                P::I(v) => v.to_string(),
                P::L(v) => v.to_string(),
                P::D(v) => v.to_string(),
                P::S(v) => format!("\"{v}\""),
                P::Dv(v) => list(v),
                P::Iv(v) => list(v),
            })
            .collect();
        out.push_str(&format!("{}: {}", n.name, props.join(", ")));
        if n.kids.is_empty() {
            out.push('\n');
        } else {
            out.push_str(" {\n");
            for k in &n.kids {
                node(out, k);
            }
            out.push_str("}\n");
        }
    }
    let mut out = String::from("; FBX 7.3.0 project file\n");
    for x in nodes {
        node(&mut out, x);
    }
    out
}

fn binary(nodes: &[N]) -> Vec<u8> {
    fn node(out: &mut Vec<u8>, n: &N) {
        let start = out.len();
        out.extend([0u8; 12]);
        out.push(n.name.len() as u8);
        out.extend(n.name.as_bytes());
        let props_start = out.len();
        for p in &n.props {
            match p {
                P::I(v) => {
                    out.push(b'I');
                    out.extend(v.to_le_bytes());
                }
                P::L(v) => {
                    out.push(b'L');
                    out.extend(v.to_le_bytes());
                }
                P::D(v) => {
                    out.push(b'D');
                    out.extend(v.to_le_bytes());
                }
                P::S(v) => {
                    out.push(b'S');
                    out.extend((v.len() as u32).to_le_bytes());
                    out.extend(v.as_bytes());
                }
                P::Dv(v) => {
                    out.push(b'd');
                    out.extend((v.len() as u32).to_le_bytes());
                    out.extend(0u32.to_le_bytes());
                    out.extend((v.len() as u32 * 8).to_le_bytes());
                    v.iter().for_each(|x| out.extend(x.to_le_bytes()));
                }
                P::Iv(v) => {
                    out.push(b'i');
                    out.extend((v.len() as u32).to_le_bytes());
                    out.extend(0u32.to_le_bytes());
                    out.extend((v.len() as u32 * 4).to_le_bytes());
                    v.iter().for_each(|x| out.extend(x.to_le_bytes()));
                }
            }
        }
        let props_len = (out.len() - props_start) as u32;
        if !n.kids.is_empty() {
            for k in &n.kids {
                node(out, k);
            }
            out.extend([0u8; 13]);
        }
        let end = out.len() as u32;
        out[start..start + 4].copy_from_slice(&end.to_le_bytes());
        out[start + 4..start + 8].copy_from_slice(&(n.props.len() as u32).to_le_bytes());
        out[start + 8..start + 12].copy_from_slice(&props_len.to_le_bytes());
    }
    let mut out = b"Kaydara FBX Binary  \0\x1a\0".to_vec();
    out.extend(7400u32.to_le_bytes());
    for x in nodes {
        node(&mut out, x);
    }
    out.extend([0u8; 13]);
    out.extend([0u8; 16]);
    out
}

struct Tex {
    material: usize,
    file_name: String,
    relative: String,
}

/// A polygon mesh in one node.
struct Spec {
    vertices: Vec<[f64; 3]>,
    polys: Vec<Vec<usize>>,
    normals: bool,
    uvs: bool,
    /// Write tangents (+X) and binormals (+Y) of the XY quad.
    tangents: bool,
    /// `P:` lines of the model: (name, value).
    model_props: Vec<(&'static str, [f64; 3])>,
    materials: usize,
    textures: Vec<Tex>,
    skin_and_animation: bool,
}

fn p3(name: &str, kind: &str, v: [f64; 3]) -> N {
    n(
        "P",
        vec![
            s(name),
            s(kind),
            s(""),
            s("A"),
            P::D(v[0]),
            P::D(v[1]),
            P::D(v[2]),
        ],
        vec![],
    )
}

fn pi(name: &str, v: i32) -> N {
    n(
        "P",
        vec![s(name), s("int"), s("Integer"), s(""), P::I(v)],
        vec![],
    )
}

impl Spec {
    fn new(vertices: Vec<[f64; 3]>, polys: Vec<Vec<usize>>) -> Self {
        Self {
            vertices,
            polys,
            normals: true,
            uvs: true,
            tangents: false,
            model_props: Vec::new(),
            materials: 1,
            textures: Vec::new(),
            skin_and_animation: false,
        }
    }

    fn tree(&self) -> Vec<N> {
        let mut pvi = Vec::new();
        let mut corners = Vec::new();
        for poly in &self.polys {
            for (k, &i) in poly.iter().enumerate() {
                pvi.push(if k + 1 == poly.len() {
                    -(i as i32) - 1
                } else {
                    i as i32
                });
                corners.push((poly, i));
            }
        }
        let flat = |v: &[[f64; 3]]| v.iter().flatten().copied().collect::<Vec<f64>>();
        let mut geometry = vec![
            n("Vertices", vec![P::Dv(flat(&self.vertices))], vec![]),
            n("PolygonVertexIndex", vec![P::Iv(pvi)], vec![]),
        ];
        let mut layer = Vec::new();
        let mut element = |name: &'static str, data: Vec<N>| {
            let mut kids = vec![
                n("Version", vec![P::I(101)], vec![]),
                n("Name", vec![s("")], vec![]),
            ];
            kids.extend(data);
            geometry.push(n(name, vec![P::I(0)], kids));
            layer.push(n(
                "LayerElement",
                vec![],
                vec![
                    n("Type", vec![s(name)], vec![]),
                    n("TypedIndex", vec![P::I(0)], vec![]),
                ],
            ));
        };
        if self.normals {
            let normals: Vec<[f64; 3]> = corners
                .iter()
                .map(|(poly, _)| {
                    let p = |k: usize| Vec3::from_array(self.vertices[poly[k]].map(|c| c as f32));
                    let mut sum = Vec3::ZERO;
                    for k in 0..poly.len() {
                        sum += p(k).cross(p((k + 1) % poly.len()));
                    }
                    let u = sum.normalize_or_zero();
                    [u.x as f64, u.y as f64, u.z as f64]
                })
                .collect();
            element(
                "LayerElementNormal",
                vec![
                    n("MappingInformationType", vec![s("ByPolygonVertex")], vec![]),
                    n("ReferenceInformationType", vec![s("Direct")], vec![]),
                    n("Normals", vec![P::Dv(flat(&normals))], vec![]),
                ],
            );
        }
        if self.uvs {
            let uv: Vec<f64> = corners
                .iter()
                .flat_map(|&(_, i)| [self.vertices[i][0], self.vertices[i][1]])
                .collect();
            element(
                "LayerElementUV",
                vec![
                    n("MappingInformationType", vec![s("ByPolygonVertex")], vec![]),
                    n("ReferenceInformationType", vec![s("Direct")], vec![]),
                    n("UV", vec![P::Dv(uv)], vec![]),
                ],
            );
        }
        if self.tangents {
            for (name, data, axis) in [
                ("LayerElementTangent", "Tangents", [1.0, 0.0, 0.0]),
                ("LayerElementBinormal", "Binormals", [0.0, 1.0, 0.0]),
            ] {
                let values: Vec<f64> = corners.iter().flat_map(|_| axis).collect();
                element(
                    name,
                    vec![
                        n("MappingInformationType", vec![s("ByPolygonVertex")], vec![]),
                        n("ReferenceInformationType", vec![s("Direct")], vec![]),
                        n(data, vec![P::Dv(values)], vec![]),
                    ],
                );
            }
        }
        if self.materials > 0 {
            element(
                "LayerElementMaterial",
                vec![
                    n("MappingInformationType", vec![s("AllSame")], vec![]),
                    n("ReferenceInformationType", vec![s("IndexToDirect")], vec![]),
                    n("Materials", vec![P::Iv(vec![0])], vec![]),
                ],
            );
        }
        geometry.push(n("Layer", vec![P::I(0)], layer));
        let props = self
            .model_props
            .iter()
            .map(|(name, v)| {
                let kind = match *name {
                    "Lcl Translation" | "Lcl Rotation" | "Lcl Scaling" => *name,
                    _ => "Vector3D",
                };
                p3(name, kind, *v)
            })
            .collect();
        let mut objects = vec![
            n(
                "Geometry",
                vec![P::L(1000), s("Geometry::g"), s("Mesh")],
                geometry,
            ),
            n(
                "Model",
                vec![P::L(2000), s("Model::m"), s("Mesh")],
                vec![n("Properties70", vec![], props)],
            ),
        ];
        let mut connections = vec![conn("OO", 2000, 0, None), conn("OO", 1000, 2000, None)];
        for m in 0..self.materials {
            let id = 3000 + m as i64;
            objects.push(n(
                "Material",
                vec![P::L(id), s(&format!("Material::mat{m}")), s("")],
                vec![n(
                    "Properties70",
                    vec![],
                    vec![p3("DiffuseColor", "Color", [0.5; 3])],
                )],
            ));
            connections.push(conn("OO", id, 2000, None));
        }
        for (i, t) in self.textures.iter().enumerate() {
            let id = 4000 + i as i64;
            objects.push(n(
                "Texture",
                vec![P::L(id), s(&format!("Texture::t{i}")), s("")],
                vec![
                    n("Type", vec![s("TextureVideoClip")], vec![]),
                    n("FileName", vec![s(&t.file_name)], vec![]),
                    n("RelativeFilename", vec![s(&t.relative)], vec![]),
                ],
            ));
            connections.push(conn(
                "OP",
                id,
                3000 + t.material as i64,
                Some("DiffuseColor"),
            ));
        }
        if self.skin_and_animation {
            let count = self.vertices.len();
            let obj = |name: &'static str, id: i64, class: &str, kind: &str, kids| {
                n(name, vec![P::L(id), s(class), s(kind)], kids)
            };
            objects.extend([
                obj("Model", 2100, "Model::bone", "LimbNode", vec![]),
                obj("Deformer", 2200, "Deformer::skin", "Skin", vec![]),
                obj(
                    "Deformer",
                    2300,
                    "SubDeformer::c",
                    "Cluster",
                    vec![
                        n("Indexes", vec![P::Iv((0..count as i32).collect())], vec![]),
                        n("Weights", vec![P::Dv(vec![1.0; count])], vec![]),
                        n("Transform", vec![P::Dv(identity())], vec![]),
                        n("TransformLink", vec![P::Dv(identity())], vec![]),
                    ],
                ),
                obj("AnimationStack", 6000, "AnimStack::s", "", vec![]),
                obj("AnimationLayer", 6100, "AnimLayer::l", "", vec![]),
                obj("AnimationCurveNode", 6200, "AnimCurveNode::T", "", vec![]),
                obj(
                    "AnimationCurve",
                    6300,
                    "AnimCurve::c",
                    "",
                    vec![
                        n("KeyTime", vec![P::Dv(vec![0.0, 46186158000.0])], vec![]),
                        n("KeyValueFloat", vec![P::Dv(vec![0.0, 1.0])], vec![]),
                        n("KeyAttrFlags", vec![P::Iv(vec![24836])], vec![]),
                        n(
                            "KeyAttrDataFloat",
                            vec![P::Dv(vec![0.0, 0.0, 0.0, 0.0])],
                            vec![],
                        ),
                        n("KeyAttrRefCount", vec![P::Iv(vec![2])], vec![]),
                    ],
                ),
            ]);
            connections.extend([
                conn("OO", 2100, 0, None),
                conn("OO", 2200, 1000, None),
                conn("OO", 2300, 2200, None),
                conn("OO", 2100, 2300, None),
                conn("OO", 6100, 6000, None),
                conn("OO", 6200, 6100, None),
                conn("OP", 6200, 2100, Some("Lcl Translation")),
                conn("OP", 6300, 6200, Some("d|X")),
            ]);
        }
        vec![
            n(
                "GlobalSettings",
                vec![],
                vec![n(
                    "Properties70",
                    vec![],
                    vec![
                        pi("UpAxis", 1),
                        pi("FrontAxis", 2),
                        pi("CoordAxis", 0),
                        n(
                            "P",
                            vec![
                                s("UnitScaleFactor"),
                                s("double"),
                                s("Number"),
                                s(""),
                                P::D(100.0),
                            ],
                            vec![],
                        ),
                    ],
                )],
            ),
            n("Objects", vec![], objects),
            n("Connections", vec![], connections),
        ]
    }
}

fn identity() -> Vec<f64> {
    (0..16)
        .map(|i| if i % 5 == 0 { 1.0 } else { 0.0 })
        .collect()
}

fn conn(kind: &str, a: i64, b: i64, prop: Option<&str>) -> N {
    let mut props = vec![s(kind), P::L(a), P::L(b)];
    if let Some(p) = prop {
        props.push(s(p));
    }
    n("C", props, vec![])
}

fn load(spec: &Spec) -> FbxModel {
    import("assets/models/m.fbx", ascii(&spec.tree()).as_bytes(), None).unwrap()
}

fn quad() -> Spec {
    Spec::new(
        vec![
            [0.0, 0.0, 0.0],
            [1.0, 0.0, 0.0],
            [1.0, 1.0, 0.0],
            [0.0, 1.0, 0.0],
        ],
        vec![vec![0, 1, 2, 3]],
    )
}

fn vd(v: &Vertex) -> ([f32; 3], [f32; 3], [f32; 2], [f32; 4]) {
    (v.position, v.normal, v.uv, v.tangent)
}

fn finite(model: &FbxModel) {
    for (vertices, _, _) in model.baked_parts() {
        for v in vertices {
            let Vertex {
                position,
                normal,
                uv,
                tangent,
            } = v;
            for x in position.iter().chain(&normal).chain(&uv).chain(&tangent) {
                assert!(x.is_finite(), "non finite vertex value in {:?}", vd(&v));
            }
        }
    }
}

fn triangle_count(model: &FbxModel) -> usize {
    model.baked_parts().iter().map(|p| p.1.len() / 3).sum()
}

fn close(a: Vec3, b: Vec3) -> bool {
    a.abs_diff_eq(b, 1e-3 * (1.0 + b.abs().max_element()))
}

#[test]
fn ascii_and_binary_7x_load_alike() {
    let spec = quad();
    let a = load(&spec);
    let b = import("m.fbx", &binary(&spec.tree()), None).unwrap();
    assert_eq!(triangle_count(&a), 2);
    assert_eq!(triangle_count(&b), 2);
    assert_eq!(a.bounds(), b.bounds());
    let (lo, hi) = b.bounds().unwrap();
    assert!(
        close(lo, Vec3::ZERO) && close(hi, Vec3::new(1.0, 1.0, 0.0)),
        "{lo} {hi}"
    );
}

#[test]
fn fbx_6_1_ascii_loads() {
    let text = "; FBX 6.1.0 project file
FBXHeaderExtension:  {
	FBXHeaderVersion: 1003
	FBXVersion: 6100
}
GlobalSettings:  {
	Version: 1000
	Properties60:  {
		Property: \"UpAxis\", \"int\", \"\",1
		Property: \"UpAxisSign\", \"int\", \"\",1
		Property: \"FrontAxis\", \"int\", \"\",2
		Property: \"FrontAxisSign\", \"int\", \"\",1
		Property: \"CoordAxis\", \"int\", \"\",0
		Property: \"CoordAxisSign\", \"int\", \"\",1
		Property: \"UnitScaleFactor\", \"double\", \"\",100
	}
}
Objects:  {
	Model: \"Model::Tri\", \"Mesh\" {
		Version: 232
		Properties60:  {
			Property: \"Lcl Translation\", \"Lcl Translation\", \"A\",0,0,0
			Property: \"Lcl Rotation\", \"Lcl Rotation\", \"A\",0,0,0
			Property: \"Lcl Scaling\", \"Lcl Scaling\", \"A\",1,1,1
		}
		Vertices: 0,0,0,2,0,0,0,3,0
		PolygonVertexIndex: 0,1,-3
		GeometryVersion: 124
		LayerElementNormal: 0 {
			Version: 101
			Name: \"\"
			MappingInformationType: \"ByVertice\"
			ReferenceInformationType: \"Direct\"
			Normals: 0,0,1,0,0,1,0,0,1
		}
		Layer: 0 {
			Version: 100
			LayerElement:  {
				Type: \"LayerElementNormal\"
				TypedIndex: 0
			}
		}
	}
}
Relations:  {
	Model: \"Model::Tri\", \"Mesh\" {
	}
}
Connections:  {
	Connect: \"OO\", \"Model::Tri\", \"Model::Scene\"
}
";
    let model = import("t.fbx", text.as_bytes(), None).unwrap();
    assert_eq!(triangle_count(&model), 1);
    let (lo, hi) = model.bounds().unwrap();
    assert!(
        close(lo, Vec3::ZERO) && close(hi, Vec3::new(2.0, 3.0, 0.0)),
        "{lo} {hi}"
    );
}

#[test]
fn no_materials_no_uvs_no_normals() {
    let mut spec = quad();
    spec.materials = 0;
    spec.uvs = false;
    spec.normals = false;
    let model = load(&spec);
    assert!(model.materials.is_empty());
    assert_eq!(model.meshes[0].len(), 1);
    assert_eq!(model.meshes[0][0].material, None);
    finite(&model);
    for v in &model.meshes[0][0].vertices {
        assert!(
            (Vec3::from_array(v.normal).length() - 1.0).abs() < 1e-3,
            "{:?}",
            vd(v)
        );
        assert!(v.normal[2].abs() > 0.99, "{:?}", vd(v));
        assert_eq!(v.uv, [0.0, 0.0]);
    }
}

#[test]
fn ngon_and_concave_polygons_triangulate_to_their_area() {
    let pent: Vec<[f64; 3]> = (0..5)
        .map(|i| {
            let a = i as f64 * std::f64::consts::TAU / 5.0;
            [a.cos(), a.sin(), 0.0]
        })
        .collect();
    let model = load(&Spec::new(pent, vec![vec![0, 1, 2, 3, 4]]));
    assert_eq!(triangle_count(&model), 3);
    let l = vec![
        [0.0, 0.0, 0.0],
        [2.0, 0.0, 0.0],
        [2.0, 1.0, 0.0],
        [1.0, 1.0, 0.0],
        [1.0, 2.0, 0.0],
        [0.0, 2.0, 0.0],
    ];
    let model = load(&Spec::new(l, vec![vec![0, 1, 2, 3, 4, 5]]));
    assert_eq!(triangle_count(&model), 4);
    let (vertices, indices, _) = model.baked_parts().remove(0);
    let (mut signed, mut abs) = (0.0f32, 0.0f32);
    for t in indices.chunks(3) {
        let p = |i: u32| Vec3::from_array(vertices[i as usize].position);
        let z = (p(t[1]) - p(t[0])).cross(p(t[2]) - p(t[0])).z / 2.0;
        signed += z;
        abs += z.abs();
    }
    assert!(
        (signed - 3.0).abs() < 1e-4 && (abs - 3.0).abs() < 1e-4,
        "{signed} {abs}"
    );
}

#[test]
fn non_uniform_scale_pivot_and_geometric_transform_land_where_the_file_says() {
    let mut spec = quad();
    spec.model_props = vec![
        ("Lcl Translation", [10.0, 0.0, 0.0]),
        ("Lcl Rotation", [0.0, 90.0, 0.0]),
        ("Lcl Scaling", [2.0, 1.0, 3.0]),
        ("RotationPivot", [1.0, 0.0, 0.0]),
        ("GeometricTranslation", [0.0, 5.0, 0.0]),
        ("GeometricScaling", [2.0, 2.0, 2.0]),
    ];
    let model = load(&spec);
    let world = Mat4::from_translation(Vec3::new(10.0, 0.0, 0.0))
        * Mat4::from_translation(Vec3::X)
        * Mat4::from_quat(Quat::from_euler(
            EulerRot::ZYX,
            0.0,
            90f32.to_radians(),
            0.0,
        ))
        * Mat4::from_translation(-Vec3::X)
        * Mat4::from_scale(Vec3::new(2.0, 1.0, 3.0))
        * Mat4::from_translation(Vec3::new(0.0, 5.0, 0.0))
        * Mat4::from_scale(Vec3::splat(2.0));
    let (lo, hi) = model.bounds().unwrap();
    let corners = [[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]]
        .map(|[x, y]| world.transform_point3(Vec3::new(x, y, 0.0)));
    let want_lo = corners.iter().fold(Vec3::MAX, |a, b| a.min(*b));
    let want_hi = corners.iter().fold(Vec3::MIN, |a, b| a.max(*b));
    assert!(
        close(lo, want_lo) && close(hi, want_hi),
        "{lo} {hi} vs {want_lo} {want_hi}"
    );
}

#[test]
fn normals_follow_a_non_uniform_scale_on_a_tilted_surface() {
    // A 45 degree plane scaled 4x along Y: the normal turns (inverse
    // transpose); rotating it like a position would leave it off the plane.
    let mut spec = Spec::new(
        vec![
            [0.0, 0.0, 0.0],
            [1.0, 0.0, 0.0],
            [1.0, 1.0, 1.0],
            [0.0, 1.0, 1.0],
        ],
        vec![vec![0, 1, 2, 3]],
    );
    spec.model_props = vec![("Lcl Scaling", [1.0, 4.0, 1.0])];
    let model = load(&spec);
    let (vertices, indices, _) = model.baked_parts().remove(0);
    let p = |i: u32| Vec3::from_array(vertices[i as usize].position);
    let face = (p(indices[1]) - p(indices[0]))
        .cross(p(indices[2]) - p(indices[0]))
        .normalize();
    for v in &vertices {
        assert!(
            Vec3::from_array(v.normal).dot(face) > 0.999,
            "{:?} vs {face}",
            vd(v)
        );
    }
}

/// Tangent, bitangent sign and the UV layout agree: cross(n, t) * w points
/// against the way V grows (the engine's V runs down, as in glTF).
fn assert_tangent_frames(model: &FbxModel) {
    for (vertices, indices, _) in model.baked_parts() {
        for t in indices.chunks(3) {
            let p = |i: u32| Vec3::from_array(vertices[i as usize].position);
            let uv = |i: u32| glam::Vec2::from_array(vertices[i as usize].uv);
            let (e1, e2) = (p(t[1]) - p(t[0]), p(t[2]) - p(t[0]));
            let (d1, d2) = (uv(t[1]) - uv(t[0]), uv(t[2]) - uv(t[0]));
            let det = d1.x * d2.y - d2.x * d1.y;
            if det.abs() < 1e-9 {
                continue;
            }
            let bitan = (e2 * d1.x - e1 * d2.x) / det;
            for &i in t {
                let v = &vertices[i as usize];
                let n = Vec3::from_array(v.normal);
                let tan = Vec3::new(v.tangent[0], v.tangent[1], v.tangent[2]);
                assert!(
                    n.dot(tan).abs() < 1e-3,
                    "tangent off the surface: {:?}",
                    vd(v)
                );
                let b = n.cross(tan) * v.tangent[3];
                assert!(b.dot(bitan) < 0.0, "bitangent sign wrong: {:?}", vd(v));
            }
        }
    }
}

#[test]
fn tangent_sign_is_right_with_and_without_a_mirror() {
    for tangents in [false, true] {
        for scale in [[1.0, 1.0, 1.0], [-1.0, 1.0, 1.0], [1.0, -1.0, -1.0]] {
            let mut spec = quad();
            spec.tangents = tangents;
            spec.model_props = vec![("Lcl Scaling", scale), ("Lcl Rotation", [30.0, 40.0, 50.0])];
            assert_tangent_frames(&load(&spec));
        }
    }
}

#[test]
fn degenerate_triangles_and_huge_coordinates_stay_finite() {
    let spec = Spec::new(
        vec![
            [0.0, 0.0, 0.0],
            [0.0, 0.0, 0.0],
            [0.0, 0.0, 0.0],
            [1e9, 0.0, 0.0],
            [2e9, 0.0, 0.0],
            [3e9, 1e9, 0.0],
        ],
        vec![vec![0, 1, 2], vec![3, 4, 3], vec![0, 4, 5]],
    );
    let model = load(&spec);
    finite(&model);
    let (_, hi) = model.bounds().unwrap();
    assert!((hi.x - 3e9).abs() < 3e9 * 1e-5, "{hi}");
}

#[test]
fn empty_scene_loads_with_nothing_in_it() {
    let spec = Spec::new(vec![], vec![]);
    let mut tree = spec.tree();
    tree[1].kids.clear();
    tree[2].kids.clear();
    let model = import("e.fbx", ascii(&tree).as_bytes(), None).unwrap();
    assert!(model.nodes.is_empty() && model.meshes.is_empty());
    assert!(model.bounds().is_none());
}

#[test]
fn skin_and_animation_data_still_load_a_static_mesh() {
    let mut spec = quad();
    spec.skin_and_animation = true;
    let model = load(&spec);
    assert_eq!(triangle_count(&model), 2);
    finite(&model);
    assert!(model.nodes.iter().any(|n| n.mesh.is_none()), "bone node");
}

fn tex_spec(textures: Vec<Tex>, materials: usize) -> Spec {
    let mut spec = quad();
    spec.materials = materials;
    spec.textures = textures;
    spec
}

#[test]
fn foreign_absolute_texture_path_falls_back_to_the_same_folder() {
    for file in [
        r"C:\Users\someone\Pictures\skin.png",
        "/home/someone/skin.png",
    ] {
        let spec = tex_spec(
            vec![Tex {
                material: 0,
                file_name: file.into(),
                relative: String::new(),
            }],
            1,
        );
        let tried = RefCell::new(Vec::new());
        let model = import(
            "assets/models/m.fbx",
            ascii(&spec.tree()).as_bytes(),
            Some(&|p| {
                tried.borrow_mut().push(p.to_string());
                if p == "assets/models/skin.png" {
                    Ok(b"x".to_vec())
                } else {
                    Err(std::io::ErrorKind::NotFound.into())
                }
            }),
        )
        .unwrap();
        assert!(
            model.materials[0].textures[0].is_some(),
            "{file}: tried {tried:?}"
        );
    }
}

#[test]
fn spaces_and_non_ascii_in_texture_names_survive() {
    let name = "Fell Br\u{e4}un \u{fc}.png";
    let spec = tex_spec(
        vec![Tex {
            material: 0,
            file_name: format!("D:\\Art\\{name}"),
            relative: format!("tex/{name}"),
        }],
        1,
    );
    let model = import(
        "assets/models/m.fbx",
        ascii(&spec.tree()).as_bytes(),
        Some(&|p| {
            if p == format!("assets/models/tex/{name}") {
                Ok(b"x".to_vec())
            } else {
                Err(std::io::ErrorKind::NotFound.into())
            }
        }),
    )
    .unwrap();
    assert_eq!(model.textures[0].file_name, name);
}

#[test]
fn a_texture_used_by_two_materials_is_read_once() {
    let spec = tex_spec(
        vec![Tex {
            material: 0,
            file_name: "t.png".into(),
            relative: "t.png".into(),
        }],
        2,
    );
    let mut tree = spec.tree();
    tree[2]
        .kids
        .push(conn("OP", 4000, 3001, Some("DiffuseColor")));
    let reads = Cell::new(0);
    let model = import(
        "assets/models/m.fbx",
        ascii(&tree).as_bytes(),
        Some(&|_| {
            reads.set(reads.get() + 1);
            Ok(b"x".to_vec())
        }),
    )
    .unwrap();
    assert_eq!(model.textures.len(), 1);
    assert_eq!(
        model.materials[0].textures[0],
        model.materials[1].textures[0]
    );
    assert!(model.materials[1].textures[0].is_some());
    assert_eq!(reads.get(), 1);
}

#[test]
fn big_binary_file_loads_in_reasonable_time() {
    const N: usize = 400;
    let mut vertices = Vec::new();
    for y in 0..=N {
        for x in 0..=N {
            vertices.push([
                x as f64 * 0.01,
                ((x * y) % 7) as f64 * 0.001,
                y as f64 * 0.01,
            ]);
        }
    }
    let mut polys = Vec::new();
    for y in 0..N {
        for x in 0..N {
            let i = y * (N + 1) + x;
            polys.push(vec![i, i + 1, i + N + 2, i + N + 1]);
        }
    }
    let spec = Spec::new(vertices, polys);
    let bytes = binary(&spec.tree());
    assert!(bytes.len() > 4_000_000, "{}", bytes.len());
    let start = std::time::Instant::now();
    let model = import("big.fbx", &bytes, None).unwrap();
    let ms = start.elapsed().as_millis();
    println!(
        "big fbx: {} bytes, {} triangles, {ms} ms",
        bytes.len(),
        triangle_count(&model)
    );
    assert_eq!(triangle_count(&model), N * N * 2);
    assert!(ms < 20_000, "{ms} ms");
}

/// Loads every .fbx / .gltf / .glb under `$MELON_ASSET_CORPUS` through the
/// CPU side of the engine's load path and prints a summary (no GPU upload).
/// `MELON_ASSET_CORPUS=<dir> cargo test -p melon_render corpus -- --ignored --nocapture`
#[test]
#[ignore]
fn corpus_loads_without_panics() {
    use std::collections::BTreeSet;
    use std::path::{Path, PathBuf};
    use std::time::Instant;

    let Some(root) = std::env::var_os("MELON_ASSET_CORPUS") else {
        println!("MELON_ASSET_CORPUS not set");
        return;
    };
    let root = PathBuf::from(root);
    fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for e in entries.flatten() {
            let p = e.path();
            if p.is_dir() {
                walk(&p, out);
            } else if p.extension().is_some_and(|x| {
                ["fbx", "gltf", "glb"].contains(&x.to_string_lossy().to_lowercase().as_str())
            }) {
                out.push(p);
            }
        }
    }
    let mut files = Vec::new();
    walk(&root, &mut files);
    files.sort();

    type Loaded = Result<(usize, Option<(Vec3, Vec3)>, usize), String>;
    struct Done {
        name: String,
        ms: u128,
        triangles: usize,
        extent: Option<Vec3>,
        missing: usize,
    }
    let (mut ok, mut failures) = (Vec::<Done>::new(), Vec::<(String, String)>::new());
    let mut missing_names = Vec::<String>::new();
    for file in &files {
        let name = file
            .strip_prefix(&root)
            .unwrap_or(file)
            .to_string_lossy()
            .replace(std::path::MAIN_SEPARATOR, "/");
        let start = Instant::now();
        let result = std::panic::catch_unwind(|| -> Loaded {
            let full = file.to_string_lossy().to_string();
            if crate::fbx::is_fbx(&full) {
                let bytes = std::fs::read(file).map_err(|e| e.to_string())?;
                let found = RefCell::new(BTreeSet::new());
                let lost = RefCell::new(BTreeSet::new());
                let model = import(
                    &full,
                    &bytes,
                    Some(&|p| {
                        let base = p.rsplit('/').next().unwrap_or(p).to_string();
                        match std::fs::read(p) {
                            Ok(b) => {
                                found.borrow_mut().insert(base);
                                Ok(b)
                            }
                            Err(e) => {
                                lost.borrow_mut().insert(base);
                                Err(e)
                            }
                        }
                    }),
                )
                .map_err(|e| format!("{e:#}"))?;
                let missing: Vec<String> =
                    lost.borrow().difference(&found.borrow()).cloned().collect();
                for m in &missing {
                    println!(
                        "  missing texture {m} ({})",
                        file.file_name().unwrap().to_string_lossy()
                    );
                }
                let tris = triangle_count(&model);
                // Every texture bound must at least decode.
                for t in &model.textures {
                    if let Err(e) = image::load_from_memory(&t.bytes) {
                        println!("  undecodable texture {}: {e}", t.file_name);
                    }
                }
                Ok((tris, model.bounds(), missing.len()))
            } else {
                let (doc, buffers, images) = gltf::import(file).map_err(|e| e.to_string())?;
                let _ = images;
                let mut tris = 0;
                for mesh in doc.meshes() {
                    for prim in mesh.primitives() {
                        let reader = prim.reader(|b| Some(&buffers[b.index()]));
                        tris += reader
                            .read_indices()
                            .map(|i| i.into_u32().count())
                            .or_else(|| reader.read_positions().map(|p| p.count()))
                            .unwrap_or(0)
                            / 3;
                    }
                }
                let bounds = crate::gltf_bounds(file).map_err(|e| format!("{e:#}"))?;
                Ok((tris, bounds, 0))
            }
        });
        let ms = start.elapsed().as_millis();
        match result {
            Ok(Ok((triangles, bounds, missing))) => ok.push(Done {
                name,
                ms,
                triangles,
                extent: bounds.map(|(lo, hi)| hi - lo),
                missing,
            }),
            Ok(Err(e)) => failures.push((name, e)),
            Err(_) => failures.push((name, "PANIC".into())),
        }
    }
    println!(
        "\n=== corpus: {} files, {} ok, {} failed ===",
        files.len(),
        ok.len(),
        failures.len()
    );
    for (n, e) in &failures {
        println!("FAIL {n}: {e}");
    }
    let missing: usize = ok.iter().map(|d| d.missing).sum();
    for d in ok.iter().filter(|d| d.missing > 0) {
        missing_names.push(d.name.clone());
    }
    println!(
        "missing textures: {missing} in {} files",
        missing_names.len()
    );
    let mut slow: Vec<&Done> = ok.iter().collect();
    slow.sort_by_key(|d| std::cmp::Reverse(d.ms));
    println!("slowest:");
    for d in slow.iter().take(10) {
        println!("  {} ms  {} ({} tris)", d.ms, d.name, d.triangles);
    }
    println!("slowest fbx:");
    for d in slow
        .iter()
        .filter(|d| d.name.to_lowercase().ends_with(".fbx"))
        .take(5)
    {
        println!("  {} ms  {} ({} tris)", d.ms, d.name, d.triangles);
    }
    let mut tris: Vec<usize> = ok.iter().map(|d| d.triangles).collect();
    tris.sort();
    if !tris.is_empty() {
        println!(
            "triangles min/median/max: {} / {} / {}",
            tris[0],
            tris[tris.len() / 2],
            tris[tris.len() - 1]
        );
    }
    for d in &ok {
        match d.extent {
            None => println!("NO MESH: {}", d.name),
            Some(e) => {
                let m = e.max_element();
                if !(0.01..=100.0).contains(&m) {
                    println!("SUSPECT SIZE {m:.4} m: {} ({e})", d.name);
                }
            }
        }
    }
}
