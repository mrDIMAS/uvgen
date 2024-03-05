//! UV Map generator. Used to generate second texture coordinates for light maps.
//!
//! Current implementation uses simple tri-planar mapping.
//!
//! ## Example
//!
//! ```rust
//! # use nalgebra::{Vector3, Vector2};
//!
//! #[derive(Copy, Clone, Debug, Default, PartialEq)]
//! pub struct Vertex {
//!     pub position: Vector3<f32>,
//!     pub tex_coord: Vector2<f32>,
//! }
//!
//! impl Vertex {
//!     fn new(x: f32, y: f32, z: f32) -> Self {
//!         Self {
//!             position: Vector3::new(x, y, z),
//!             tex_coord: Default::default(),
//!         }
//!     }
//! }
//!
//! // Create cube geometry.
//! let mut vertices = vec![
//!     Vertex::new(-0.5, -0.5, 0.5),
//!     Vertex::new(-0.5, 0.5, 0.5),
//!     Vertex::new(0.5, 0.5, 0.5),
//!     Vertex::new(0.5, -0.5, 0.5),
//!     Vertex::new(-0.5, -0.5, -0.5),
//!     Vertex::new(-0.5, 0.5, -0.5),
//!     Vertex::new(0.5, 0.5, -0.5),
//!     Vertex::new(0.5, -0.5, -0.5),
//! ];
//!
//! let mut triangles = vec![
//!     // Front
//!     [2, 1, 0],
//!     [3, 2, 0],
//!     // Back
//!     [4, 5, 6],
//!     [4, 6, 7],
//!     // Right
//!     [7, 6, 2],
//!     [2, 3, 7],
//!     // Left
//!     [0, 1, 5],
//!     [0, 5, 4],
//!     // Top
//!     [5, 1, 2],
//!     [5, 2, 6],
//!     // Bottom
//!     [3, 0, 4],
//!     [7, 3, 4],
//! ];
//!
//! let patch = uvgen::generate_uvs(
//!     vertices.iter().map(|v| v.position),
//!     triangles.iter().cloned(),
//!     0.005,
//! ).unwrap();
//!
//! // Apply patch to the initial data.
//! triangles = patch.triangles;
//! for &vertex_index in &patch.additional_vertices {
//!     let vertex = vertices[vertex_index as usize];
//!     vertices.push(vertex);
//! }
//!
//! // Assign generated texture coordinates.
//! for (vertex, tex_coord) in vertices.iter_mut().zip(&patch.second_tex_coords) {
//!     vertex.tex_coord = *tex_coord;
//! }
//! ```

use nalgebra::{Vector2, Vector3};
use rectutils::pack::RectPacker;
use std::cmp::Ordering;

#[derive(Copy, Clone)]
enum PlaneClass {
    XY,
    YZ,
    XZ,
}

#[inline]
#[allow(clippy::useless_let_if_seq)]
fn classify_plane(normal: Vector3<f32>) -> PlaneClass {
    let mut longest = 0.0f32;
    let mut class = PlaneClass::XY;

    if normal.x.abs() > longest {
        longest = normal.x.abs();
        class = PlaneClass::YZ;
    }

    if normal.y.abs() > longest {
        longest = normal.y.abs();
        class = PlaneClass::XZ;
    }

    if normal.z.abs() > longest {
        class = PlaneClass::XY;
    }

    class
}

#[derive(Debug)]
struct UvMesh {
    // Array of indices of triangles.
    triangles: Vec<usize>,
    uv_max: Vector2<f32>,
    uv_min: Vector2<f32>,
}

impl UvMesh {
    fn new(first_triangle: usize) -> Self {
        Self {
            triangles: vec![first_triangle],
            uv_max: Vector2::new(-f32::MAX, -f32::MAX),
            uv_min: Vector2::new(f32::MAX, f32::MAX),
        }
    }

    // Returns total width of the mesh.
    fn width(&self) -> f32 {
        self.uv_max.x - self.uv_min.x
    }

    // Returns total height of the mesh.
    fn height(&self) -> f32 {
        self.uv_max.y - self.uv_min.y
    }

    // Returns total area of the mesh.
    fn area(&self) -> f32 {
        self.width() * self.height()
    }
}

/// A set of faces with triangles belonging to faces.
#[derive(Default, Debug)]
struct UvBox {
    px: Vec<usize>,
    nx: Vec<usize>,
    py: Vec<usize>,
    ny: Vec<usize>,
    pz: Vec<usize>,
    nz: Vec<usize>,
    projections: Vec<[Vector2<f32>; 3]>,
}

fn face_vs_face(
    vertices: &mut Vec<Vector3<f32>>,
    triangles: &mut Vec<[u32; 3]>,
    face_triangles: &[usize],
    other_face_triangles: &[usize],
    patch: &mut SurfaceDataPatch,
) {
    for other_triangle_index in other_face_triangles.iter() {
        let other_triangle = triangles[*other_triangle_index];
        for triangle_index in face_triangles.iter() {
            'outer_loop: for vertex_index in triangles[*triangle_index].iter_mut() {
                for other_vertex_index in other_triangle {
                    if *vertex_index == other_vertex_index {
                        // We have adjacency, add new vertex and fix current index.
                        patch.additional_vertices.push(other_vertex_index);
                        *vertex_index = vertices.len() as u32;
                        let vertex = vertices[other_vertex_index as usize];
                        vertices.push(vertex);
                        continue 'outer_loop;
                    }
                }
            }
        }
    }
}

fn make_seam(
    vertices: &mut Vec<Vector3<f32>>,
    triangles: &mut Vec<[u32; 3]>,
    current_face: usize,
    faces: &[&[usize]],
    patch: &mut SurfaceDataPatch,
) {
    for (face_index, &other_face_triangles) in faces.iter().enumerate() {
        if face_index == current_face {
            continue;
        }

        face_vs_face(
            vertices,
            triangles,
            &faces[current_face],
            other_face_triangles,
            patch,
        );
    }
}

/// A patch for surface data that contains secondary texture coordinates and new topology for data.
/// It is needed for serialization: during the UV generation, generator could multiply vertices to
/// make seams, it adds new data to existing vertices. The problem is that we do not serialize
/// surface data - we store only a "link" to resource from which we'll load surface data on
/// deserialization. But freshly loaded resource is not suitable for generated lightmap - in most
/// cases it just does not have secondary texture coordinates. So we have to patch data after loading
/// somehow with required data, this is where `SurfaceDataPatch` comes into play.
#[derive(Clone, Debug, Default)]
pub struct SurfaceDataPatch {
    /// A surface data id. Usually it is just a hash of surface data. Can be ignored completely, if
    /// you don't need to save patches.
    pub data_id: u64,
    /// List of indices of vertices, that must be cloned and pushed into vertices array one by one at
    /// the end.
    pub additional_vertices: Vec<u32>,
    /// New topology for surface data. Old topology must be replaced with new, because UV generator
    /// splits vertices at UV map seams.
    pub triangles: Vec<[u32; 3]>,
    /// List of second texture coordinates used for light maps. This list includes all the vertices
    /// **added** by the generation step.
    pub second_tex_coords: Vec<Vector2<f32>>,
}

/// Maps each triangle from surface to appropriate side of box. This is so called
/// box mapping.
fn generate_uv_box(vertices: &[Vector3<f32>], triangles: &[[u32; 3]]) -> Option<UvBox> {
    let mut uv_box = UvBox::default();
    for (i, triangle) in triangles.iter().enumerate() {
        let a = vertices.get(triangle[0] as usize)?;
        let b = vertices.get(triangle[1] as usize)?;
        let c = vertices.get(triangle[2] as usize)?;
        let normal = (b - a).cross(&(c - a));
        let class = classify_plane(normal);
        match class {
            PlaneClass::XY => {
                if normal.z < 0.0 {
                    uv_box.nz.push(i);
                    uv_box.projections.push([a.yx(), b.yx(), c.yx()])
                } else {
                    uv_box.pz.push(i);
                    uv_box.projections.push([a.xy(), b.xy(), c.xy()]);
                }
            }
            PlaneClass::XZ => {
                if normal.y < 0.0 {
                    uv_box.ny.push(i);
                    uv_box.projections.push([a.xz(), b.xz(), c.xz()])
                } else {
                    uv_box.py.push(i);
                    uv_box.projections.push([a.zx(), b.zx(), c.zx()])
                }
            }
            PlaneClass::YZ => {
                if normal.x < 0.0 {
                    uv_box.nx.push(i);
                    uv_box.projections.push([a.zy(), b.zy(), c.zy()])
                } else {
                    uv_box.px.push(i);
                    uv_box.projections.push([a.yz(), b.yz(), c.yz()])
                }
            }
        }
    }
    Some(uv_box)
}

// Generates a set of UV meshes.
fn generate_uv_meshes(
    uv_box: &UvBox,
    data_id: u64,
    vertices: &mut Vec<Vector3<f32>>,
    triangles: &mut Vec<[u32; 3]>,
) -> (Vec<UvMesh>, SurfaceDataPatch) {
    let mut mesh_patch = SurfaceDataPatch {
        data_id,
        ..Default::default()
    };

    // Step 1. Split vertices at boundary between each face. This step multiplies the
    // number of vertices at boundary so we'll get separate texture coordinates at
    // seams.
    for face_index in 0..6 {
        make_seam(
            vertices,
            triangles,
            face_index,
            &[
                &uv_box.px, &uv_box.nx, &uv_box.py, &uv_box.ny, &uv_box.pz, &uv_box.nz,
            ],
            &mut mesh_patch,
        );
    }

    // Step 2. Find separate "meshes" on uv map. After box mapping we will most likely
    // end up with set of faces, some of them may form meshes and each such mesh must
    // be moved with all faces it has.
    let mut meshes = Vec::new();
    let mut removed_triangles = vec![false; triangles.len()];
    for triangle_index in 0..triangles.len() {
        if !removed_triangles[triangle_index] {
            // Start off random triangle and continue gather adjacent triangles one by one.
            let mut mesh = UvMesh::new(triangle_index);
            removed_triangles[triangle_index] = true;

            let mut last_triangle = 1;
            let mut i = 0;
            while i < last_triangle {
                let triangle = &triangles[mesh.triangles[i]];
                // Push all adjacent triangles into mesh. This is brute force implementation.
                for (other_triangle_index, other_triangle) in triangles.iter().enumerate() {
                    if !removed_triangles[other_triangle_index] {
                        'vertex_loop: for &vertex_index in triangle {
                            for &other_vertex_index in other_triangle {
                                if vertex_index == other_vertex_index {
                                    mesh.triangles.push(other_triangle_index);
                                    removed_triangles[other_triangle_index] = true;
                                    // Push border further to continue iterating from added
                                    // triangle. This is needed because we checking one triangle
                                    // after another and we must continue if new triangles have
                                    // some adjacent ones.
                                    last_triangle += 1;
                                    break 'vertex_loop;
                                }
                            }
                        }
                    }
                }
                i += 1;
            }

            // Calculate bounds.
            for &triangle_index in mesh.triangles.iter() {
                let [a, b, c] = uv_box.projections[triangle_index];
                mesh.uv_min = a.inf(&b).inf(&c).inf(&mesh.uv_min);
                mesh.uv_max = a.sup(&b).sup(&c).sup(&mesh.uv_max);
            }
            meshes.push(mesh);
        }
    }

    (meshes, mesh_patch)
}

/// Generates UV map for the given vertices and triangles.
///
/// # Performance
///
/// This method utilizes lots of "brute force" algorithms, so it is not fast as it could be in
/// ideal case. It also allocates some memory for internal needs.
pub fn generate_uvs(
    vertices: impl Iterator<Item = Vector3<f32>>,
    triangles: impl Iterator<Item = [u32; 3]>,
    spacing: f32,
) -> Option<SurfaceDataPatch> {
    let mut vertices = vertices.collect::<Vec<_>>();
    let mut triangles = triangles.collect::<Vec<_>>();

    let uv_box = generate_uv_box(&vertices, &triangles)?;

    let (mut meshes, mut patch) = generate_uv_meshes(&uv_box, 0, &mut vertices, &mut triangles);

    // Step 4. Arrange and scale all meshes on uv map so it fits into [0;1] range.
    let area = meshes.iter().fold(0.0, |area, mesh| area + mesh.area());
    let square_side = area.sqrt() + spacing * meshes.len() as f32;

    meshes.sort_unstable_by(|a, b| b.area().partial_cmp(&a.area()).unwrap_or(Ordering::Equal));

    let mut rects = Vec::new();

    let twice_spacing = spacing * 2.0;

    // Some empiric coefficient that large enough to make size big enough for all meshes.
    // This should be large enough to fit all meshes, but small to prevent losing of space.
    // We'll use iterative approach to pack everything as tight as possible: at each iteration
    // scale will be increased until packer is able to pack everything.
    let mut empiric_scale = 1.1;
    let mut scale = 1.0;
    let mut packer = RectPacker::new(1.0, 1.0);
    'try_loop: for _ in 0..100 {
        rects.clear();

        // Calculate size of atlas for packer, we'll scale it later on.
        scale = 1.0 / (square_side * empiric_scale);

        // We'll pack into 1.0 square, our UVs must be in [0;1] range, no wrapping is allowed.
        packer.clear();
        for mesh in meshes.iter() {
            if let Some(rect) = packer.find_free(
                mesh.width() * scale + twice_spacing,
                mesh.height() * scale + twice_spacing,
            ) {
                rects.push(rect);
            } else {
                // I don't know how to pass this by without iterative approach :(
                empiric_scale *= 1.33;
                continue 'try_loop;
            }
        }
    }

    patch.second_tex_coords = vec![Vector2::default(); vertices.len()];
    for (i, rect) in rects.into_iter().enumerate() {
        let mesh = &meshes[i];

        for &triangle_index in mesh.triangles.iter() {
            for (&vertex_index, &projection) in triangles[triangle_index]
                .iter()
                .zip(&uv_box.projections[triangle_index])
            {
                let second_tex_coord = patch.second_tex_coords.get_mut(vertex_index as usize)?;

                *second_tex_coord = (projection - mesh.uv_min).scale(scale)
                    + Vector2::new(spacing, spacing)
                    + rect.position;
            }
        }
    }

    patch.triangles = triangles;

    Some(patch)
}

#[cfg(test)]
mod test {
    use nalgebra::{Vector2, Vector3};

    #[derive(Copy, Clone, Debug, Default, PartialEq)]
    pub struct Vertex {
        pub position: Vector3<f32>,
        pub tex_coord: Vector2<f32>,
    }

    impl Vertex {
        fn new(x: f32, y: f32, z: f32) -> Self {
            Self {
                position: Vector3::new(x, y, z),
                tex_coord: Default::default(),
            }
        }
    }

    #[test]
    fn test_uv_gen() {
        // Create cube geometry.
        let mut vertices = vec![
            Vertex::new(-0.5, -0.5, 0.5),
            Vertex::new(-0.5, 0.5, 0.5),
            Vertex::new(0.5, 0.5, 0.5),
            Vertex::new(0.5, -0.5, 0.5),
            Vertex::new(-0.5, -0.5, -0.5),
            Vertex::new(-0.5, 0.5, -0.5),
            Vertex::new(0.5, 0.5, -0.5),
            Vertex::new(0.5, -0.5, -0.5),
        ];

        let mut triangles = vec![
            // Front
            [2, 1, 0],
            [3, 2, 0],
            // Back
            [4, 5, 6],
            [4, 6, 7],
            // Right
            [7, 6, 2],
            [2, 3, 7],
            // Left
            [0, 1, 5],
            [0, 5, 4],
            // Top
            [5, 1, 2],
            [5, 2, 6],
            // Bottom
            [3, 0, 4],
            [7, 3, 4],
        ];

        let patch = super::generate_uvs(
            vertices.iter().map(|v| v.position),
            triangles.iter().cloned(),
            0.005,
        )
        .expect("Generation must be successful!");

        // Apply patch.
        triangles = patch.triangles;
        for &vertex_index in &patch.additional_vertices {
            let vertex = vertices[vertex_index as usize];
            vertices.push(vertex);
        }
        for (vertex, tex_coord) in vertices.iter_mut().zip(&patch.second_tex_coords) {
            vertex.tex_coord = *tex_coord;
        }

        assert_eq!(
            triangles,
            [
                [2, 1, 0,],
                [3, 2, 0,],
                [4, 5, 6,],
                [4, 6, 7,],
                [12, 10, 8,],
                [9, 11, 13,],
                [17, 14, 15,],
                [18, 16, 19,],
                [23, 20, 21,],
                [24, 22, 25,],
                [27, 26, 29,],
                [31, 28, 30,],
            ]
        );

        assert_eq!(
            vertices,
            [
                Vertex {
                    position: Vector3::new(-0.5, -0.5, 0.5),
                    tex_coord: Vector2::new(0.005, 0.005),
                },
                Vertex {
                    position: Vector3::new(-0.5, 0.5, 0.5),
                    tex_coord: Vector2::new(0.005, 0.21778576),
                },
                Vertex {
                    position: Vector3::new(0.5, 0.5, 0.5),
                    tex_coord: Vector2::new(0.21778576, 0.21778576),
                },
                Vertex {
                    position: Vector3::new(0.5, -0.5, 0.5),
                    tex_coord: Vector2::new(0.21778576, 0.005),
                },
                Vertex {
                    position: Vector3::new(-0.5, -0.5, -0.5),
                    tex_coord: Vector2::new(0.22778577, 0.005),
                },
                Vertex {
                    position: Vector3::new(-0.5, 0.5, -0.5),
                    tex_coord: Vector2::new(0.44057155, 0.005),
                },
                Vertex {
                    position: Vector3::new(0.5, 0.5, -0.5),
                    tex_coord: Vector2::new(0.44057155, 0.21778576),
                },
                Vertex {
                    position: Vector3::new(0.5, -0.5, -0.5),
                    tex_coord: Vector2::new(0.22778577, 0.21778576),
                },
                Vertex {
                    position: Vector3::new(0.5, 0.5, 0.5),
                    tex_coord: Vector2::new(0.21778576, 0.44057155),
                },
                Vertex {
                    position: Vector3::new(0.5, 0.5, 0.5),
                    tex_coord: Vector2::new(0.6633573, 0.21778576),
                },
                Vertex {
                    position: Vector3::new(0.5, 0.5, -0.5),
                    tex_coord: Vector2::new(0.21778576, 0.22778577),
                },
                Vertex {
                    position: Vector3::new(0.5, -0.5, 0.5),
                    tex_coord: Vector2::new(0.45057154, 0.21778576),
                },
                Vertex {
                    position: Vector3::new(0.5, -0.5, -0.5),
                    tex_coord: Vector2::new(0.005, 0.22778577),
                },
                Vertex {
                    position: Vector3::new(0.5, -0.5, -0.5),
                    tex_coord: Vector2::new(0.45057154, 0.005),
                },
                Vertex {
                    position: Vector3::new(-0.5, 0.5, 0.5),
                    tex_coord: Vector2::new(0.21778576, 0.6633573),
                },
                Vertex {
                    position: Vector3::new(-0.5, 0.5, -0.5),
                    tex_coord: Vector2::new(0.005, 0.6633573),
                },
                Vertex {
                    position: Vector3::new(-0.5, 0.5, -0.5),
                    tex_coord: Vector2::new(0.22778577, 0.44057155),
                },
                Vertex {
                    position: Vector3::new(-0.5, -0.5, 0.5),
                    tex_coord: Vector2::new(0.21778576, 0.45057154),
                },
                Vertex {
                    position: Vector3::new(-0.5, -0.5, 0.5),
                    tex_coord: Vector2::new(0.44057155, 0.22778577),
                },
                Vertex {
                    position: Vector3::new(-0.5, -0.5, -0.5),
                    tex_coord: Vector2::new(0.22778577, 0.22778577),
                },
                Vertex {
                    position: Vector3::new(-0.5, 0.5, 0.5),
                    tex_coord: Vector2::new(0.8861431, 0.005),
                },
                Vertex {
                    position: Vector3::new(0.5, 0.5, 0.5),
                    tex_coord: Vector2::new(0.8861431, 0.21778576),
                },
                Vertex {
                    position: Vector3::new(0.5, 0.5, 0.5),
                    tex_coord: Vector2::new(0.21778576, 0.8861431),
                },
                Vertex {
                    position: Vector3::new(-0.5, 0.5, -0.5),
                    tex_coord: Vector2::new(0.6733573, 0.005),
                },
                Vertex {
                    position: Vector3::new(-0.5, 0.5, -0.5),
                    tex_coord: Vector2::new(0.005, 0.6733573),
                },
                Vertex {
                    position: Vector3::new(0.5, 0.5, -0.5),
                    tex_coord: Vector2::new(0.005, 0.8861431),
                },
                Vertex {
                    position: Vector3::new(-0.5, -0.5, 0.5),
                    tex_coord: Vector2::new(0.45057154, 0.44057155),
                },
                Vertex {
                    position: Vector3::new(0.5, -0.5, 0.5),
                    tex_coord: Vector2::new(0.6633573, 0.44057155),
                },
                Vertex {
                    position: Vector3::new(0.5, -0.5, 0.5),
                    tex_coord: Vector2::new(0.44057155, 0.6633573),
                },
                Vertex {
                    position: Vector3::new(-0.5, -0.5, -0.5),
                    tex_coord: Vector2::new(0.45057154, 0.22778577),
                },
                Vertex {
                    position: Vector3::new(-0.5, -0.5, -0.5),
                    tex_coord: Vector2::new(0.22778577, 0.45057154),
                },
                Vertex {
                    position: Vector3::new(0.5, -0.5, -0.5),
                    tex_coord: Vector2::new(0.44057155, 0.45057154),
                },
            ]
        );
    }
}
