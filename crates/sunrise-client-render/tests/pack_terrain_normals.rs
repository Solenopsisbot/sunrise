//! A `shaders/` pack's terrain normal and tangent (`gl_Normal`, `at_tangent`, spec §5.1)
//! through the real `Renderer`, with flat section draws on and off (`set_flat_draws`): a pack
//! that writes one of them as its colour, inside a box of six quads, one facing the camera from
//! each direction, solid (`gbuffers_terrain`) and translucent (`gbuffers_water`). Each face must
//! read its own normal and tangent in both draw paths (#432). Asset-free; run with throwaway SUNRISE_HOME,
//! SUNRISE_LAUNCHER_HOME, XDG_DATA_HOME and XDG_CONFIG_HOME (AGENTS.md):
//! `cargo test -p sunrise-client-render --test pack_terrain_normals -- --ignored`
//! (under `python3 tools/ci/with_xvfb.py` without a display) gives each its own. On a Wayland
//! desktop, unset `WAYLAND_DISPLAY` as `with_xvfb.py` does: the child's own `XDG_RUNTIME_DIR`
//! has no Wayland socket.
use client::{
    gfx::{Frame, Renderer, ScreenshotRequest},
    graphics::GraphicsApi,
    hud::HudBatch,
    mesher::{SectionMesh, TerrainVertex},
};
use std::{path::Path, sync::Arc};
use sunrise_client_render as client;
use winit::{
    application::ApplicationHandler,
    event::WindowEvent,
    event_loop::{ActiveEventLoop, EventLoop},
    window::{Window, WindowId},
};

#[test]
#[ignore = "daily"]
fn pack_terrain_normals() {
    run_daily("pack_terrain_normals", &[], fixture);
}

#[test]
#[ignore = "daily"]
fn pack_terrain_normals_without_flat_draws() {
    run_daily(
        "pack_terrain_normals_without_flat_draws",
        &[("SUNRISE_FLAT_DRAWS", "0")],
        fixture_without_flat,
    );
}

/// The section the box is in: keys count sections from the bottom of the world, y -64, so 10
/// is y 96 to 112.
const SECTION: (i32, i32, i32) = (0, 10, 0);
/// The camera, at the middle of the section.
const EYE: [f64; 3] = [8.0, 104.0, 8.0];

/// The pack's colour for a direction's component: -1, 0 and 1 as 51, 153 and 255 of 255 (0.2,
/// 0.6 and 1.0, each a whole number of 255ths).
fn channel(n: i32) -> u8 {
    match n {
        -1 => 51,
        0 => 153,
        1 => 255,
        _ => unreachable!(),
    }
}

/// Writes a pack whose solid and translucent terrain write `input` (a direction), as
/// `n * 0.4 + 0.6`, into `colortex0`, and loads it.
fn pack(renderer: &mut Renderer, folder: &Path, input: &str) {
    let shaders = folder.join("shaders");
    std::fs::create_dir_all(&shaders).unwrap();
    for name in ["gbuffers_terrain", "gbuffers_water"] {
        std::fs::write(
            shaders.join(format!("{name}.vsh")),
            format!(
                "#version 120\nattribute vec4 at_tangent;\nvarying vec3 n;\n\
                 void main() {{ gl_Position = ftransform(); n = {input}; }}\n"
            ),
        )
        .unwrap();
        std::fs::write(
            shaders.join(format!("{name}.fsh")),
            "#version 120\nvarying vec3 n;\n\
             void main() { gl_FragData[0] = vec4(n * 0.4 + 0.6, 1.0); }\n",
        )
        .unwrap();
    }
    std::fs::write(shaders.join("shaders.properties"), "shadow.enabled=false\n").unwrap();
    let pack = sunrise_shaders::Pack::open(folder).unwrap();
    let mut macros = sunrise_shaders::Macros::standard();
    renderer.define_pack_macros(&mut macros);
    let (properties, _) = sunrise_shaders::read_properties(&pack, &mut macros);
    let post = sunrise_shaders::load_post(&pack, "", &properties, &macros, &renderer.pack_inputs());
    assert!(post.terrain.is_some(), "gbuffers_terrain loads");
    assert!(post.water.is_some(), "gbuffers_water loads");
    renderer
        .set_shader_pack(0, Some(post), &[])
        .unwrap()
        .expect("the pack runs");
}

/// The quad facing `normal` (an axis, `axis`, and its sign) from the side of the box opposite
/// to it, 4 blocks from the camera and 12 across: its corners in the order that makes
/// `cross(p1 - p0, p2 - p0)` point along `normal`.
fn face(axis: usize, sign: i32) -> [[f32; 3]; 4] {
    let (u, v) = ((axis + 1) % 3, (axis + 2) % 3);
    let corner = |du: f32, dv: f32| {
        let mut p = [0.0; 3];
        p[axis] = 8.0 - 4.0 * sign as f32;
        p[u] = du;
        p[v] = dv;
        p
    };
    // Along u then v: the axis after them, as `e_u x e_v = e_axis` for a cyclic order.
    let up = [
        corner(2.0, 2.0),
        corner(14.0, 2.0),
        corner(14.0, 14.0),
        corner(2.0, 14.0),
    ];
    if sign > 0 {
        up
    } else {
        [up[3], up[2], up[1], up[0]]
    }
}

/// The box's six quads, all in the solid list or all in the translucent one.
fn mesh(translucent: bool) -> SectionMesh {
    let mut mesh = SectionMesh::default();
    let uvs = [[0., 1.], [1., 1.], [1., 0.], [0., 0.]];
    for axis in 0..3 {
        for sign in [1, -1] {
            let quad = face(axis, sign)
                .into_iter()
                .zip(uvs)
                .map(|(p, uv)| TerrainVertex::new(p, uv, 0, u32::MAX, 0xf0f0));
            if translucent {
                mesh.translucent.extend(quad);
            } else {
                mesh.solid.extend(quad);
            }
        }
    }
    mesh.sort_solid();
    mesh
}

/// Each view: its yaw and pitch, and the normal and tangent of the face it looks at. The face
/// across the box faces back toward the camera; its tangent is where its U grows (`mesh`'s
/// UVs): along the axis after its normal's (`face`'s `u`), whichever way the quad faces. Yaw 0
/// looks south (+z) and 90 west (-x); pitch 90 looks down.
const VIEWS: [(&str, f32, f32, [i32; 3], [i32; 3]); 6] = [
    ("south", 0.0, 0.0, [0, 0, -1], [1, 0, 0]),
    ("west", 90.0, 0.0, [1, 0, 0], [0, 1, 0]),
    ("north", 180.0, 0.0, [0, 0, 1], [1, 0, 0]),
    ("east", 270.0, 0.0, [-1, 0, 0], [0, 1, 0]),
    ("down", 0.0, 90.0, [0, 1, 0], [0, 0, 1]),
    ("up", 0.0, -90.0, [0, -1, 0], [0, 0, 1]),
];

fn shoot(renderer: &mut Renderer, hud: &HudBatch, yaw: f32, pitch: f32, path: &Path) -> [u8; 4] {
    let mut frame = fixture_frame(hud);
    frame.yaw = yaw;
    frame.pitch = pitch;
    frame.eye_rotation = [yaw, pitch];
    renderer.screenshot = Some(ScreenshotRequest {
        path: path.to_path_buf(),
        size: None,
        scaled: false,
        background: false,
    });
    renderer.render(&frame).unwrap();
    renderer
        .screenshot_result
        .take()
        .expect("readback")
        .unwrap()
        .save()
        .unwrap();
    let decoder = png::Decoder::new(std::io::BufReader::new(std::fs::File::open(path).unwrap()));
    let mut reader = decoder.read_info().unwrap();
    let mut bytes = vec![0; reader.output_buffer_size().unwrap()];
    let info = reader.next_frame(&mut bytes).unwrap();
    assert_eq!(info.color_type, png::ColorType::Rgba);
    let (width, height) = (info.width as usize, info.height as usize);
    let at = 4 * ((height / 2) * width + width / 2);
    bytes[at..at + 4].try_into().unwrap()
}

/// A renderer of 320 by 180 with Translucent Batching on (translucent quads get an arena of their
/// own unless the pack's draws read the section arena's pages); `pack` as `Renderer::new`
/// takes the picked pack (it draws flat, it is a `shaders/` pack).
fn renderer(
    window: Arc<Window>,
    pack: (bool, bool),
) -> Result<Renderer, Box<dyn std::error::Error>> {
    let white = [255; 16 * 16 * 4];
    let zero = [0; 4];
    let mut renderer = pollster::block_on(Renderer::new(
        window,
        true,
        &white,
        16,
        16,
        1,
        &[],
        &[],
        (&white, 16, 16),
        (&zero, 1, 1),
        (&zero, 1, 1),
        (&zero, 1),
        Arc::new(client::gfx::panorama::Faces::colour([0, 0, 0, 255])),
        None,
        None,
        GraphicsApi::Vulkan,
        true,
        pack,
        &client::graphics::Graphics::default(),
        Default::default(),
        Default::default(),
    ))?;
    renderer.resize(320, 180);
    Ok(renderer)
}

/// The box's six views with the pack writing `written` (`normal` or `tangent`), its quads in
/// the `translucent` list or the solid one: each view's pixel against `want` of the view's normal
/// and tangent. A first frame lays the sections out for the pack (which may drop them), then the
/// box is uploaded into that layout.
fn check_views(
    renderer: &mut Renderer,
    hud: &HudBatch,
    out: &Path,
    name: &str,
    translucent: bool,
    want: fn([i32; 3], [i32; 3]) -> [i32; 3],
    result: &mut serde_json::Map<String, serde_json::Value>,
) {
    let list = if translucent { "translucent" } else { "solid" };
    shoot(renderer, hud, 0.0, 0.0, &out.join("settle.png"));
    renderer.take_dropped_sections();
    renderer.upload(SECTION, &mesh(translucent));
    for (view, yaw, pitch, normal, tangent) in VIEWS {
        let name = format!("{name}-{list}-{view}");
        let pixel = shoot(renderer, hud, yaw, pitch, &out.join(format!("{name}.png")));
        let want = want(normal, tangent);
        let expected = [channel(want[0]), channel(want[1]), channel(want[2]), 255];
        assert_eq!(pixel, expected, "{name}: the face's {want:?}");
        result.insert(name, format!("{pixel:?}").into());
    }
}

fn normal(normal: [i32; 3], _: [i32; 3]) -> [i32; 3] {
    normal
}
fn tangent(_: [i32; 3], tangent: [i32; 3]) -> [i32; 3] {
    tangent
}
/// The constant normal per-section draws give where they don't read the pages.
fn constant(_: [i32; 3], _: [i32; 3]) -> [i32; 3] {
    [0, 1, 0]
}

/// Flat draws on and off on a device that has them, the pack loaded after each switch.
fn fixture(window: Arc<Window>, out: &Path) -> Result<(), Box<dyn std::error::Error>> {
    std::fs::create_dir_all(out)?;
    let mut renderer = renderer(window, (true, true))?;
    let hud = HudBatch::scratch([320, 180]);
    let mut result = serde_json::Map::new();
    for flat in [true, false] {
        renderer.set_flat_draws(flat)?;
        let draws = if flat { "flat" } else { "per-section" };
        for (written, input, want) in [
            ("normal", "gl_Normal", normal as fn(_, _) -> _),
            ("tangent", "at_tangent.xyz", tangent),
        ] {
            pack(
                &mut renderer,
                &out.join(format!("pack-{draws}-{written}")),
                input,
            );
            for translucent in [false, true] {
                let name = format!("{draws}-{written}");
                check_views(
                    &mut renderer,
                    &hud,
                    out,
                    &name,
                    translucent,
                    want,
                    &mut result,
                );
            }
        }
    }
    finish(out, result)
}

/// Without flat draws (`SUNRISE_FLAT_DRAWS=0`) and with no pack picked: the section arena's
/// pages start as vertex buffers alone. A pack loaded after the renderer makes them storage for
/// its per-section terrain programs (`follow_arena_storage`), which then read the real normals;
/// unloading it makes them vertex buffers again, as large as before. A pack whose buffer
/// objects leave no room in the vertex stage for the pages still loads its terrain programs, and
/// they get the constant normal, the pages staying vertex buffers.
fn fixture_without_flat(window: Arc<Window>, out: &Path) -> Result<(), Box<dyn std::error::Error>> {
    std::fs::create_dir_all(out)?;
    let mut renderer = renderer(window, (false, false))?;
    assert!(
        !renderer.flat_draws(),
        "SUNRISE_FLAT_DRAWS=0 leaves out flat draws"
    );
    let hud = HudBatch::scratch([320, 180]);
    let mut result = serde_json::Map::new();
    shoot(&mut renderer, &hud, 0.0, 0.0, &out.join("start.png"));
    let (storage, vertices) = renderer.section_arena_pages();
    assert!(!storage, "no pack: the pages are vertex buffers alone");
    pack(&mut renderer, &out.join("pack-normal"), "gl_Normal");
    for translucent in [false, true] {
        check_views(
            &mut renderer,
            &hud,
            out,
            "loaded",
            translucent,
            normal,
            &mut result,
        );
    }
    assert!(
        renderer.section_arena_pages().0,
        "the pack reads the pages: storage"
    );
    renderer.set_shader_pack(0, None, &[]).unwrap();
    shoot(&mut renderer, &hud, 0.0, 0.0, &out.join("unloaded.png"));
    assert_eq!(
        renderer.section_arena_pages(),
        (false, vertices),
        "unloaded: vertex buffers alone again, of the pages' first size"
    );
    // Past any device's storage buffers: the terrain programs read no pages.
    renderer.set_pack_terrain_bindings(client::gfx::pack_atlas::TerrainBindings {
        buffer_objects: u32::MAX,
        geometry: false,
    });
    pack(&mut renderer, &out.join("pack-crowded"), "gl_Normal");
    for translucent in [false, true] {
        check_views(
            &mut renderer,
            &hud,
            out,
            "crowded",
            translucent,
            constant,
            &mut result,
        );
    }
    assert_eq!(renderer.section_arena_pages(), (false, vertices));
    finish(out, result)
}

fn finish(
    out: &Path,
    result: serde_json::Map<String, serde_json::Value>,
) -> Result<(), Box<dyn std::error::Error>> {
    // Vulkan drew these frames. A software adapter (lavapipe) is fine: the comparisons are exact.
    let adapter = client::gfx::adapter().expect("the renderer's adapter");
    assert_eq!(adapter.backend, "Vulkan");
    eprintln!(
        "pack_terrain_normals adapter: {} ({}, {}), driver {}",
        adapter.name, adapter.backend, adapter.kind, adapter.driver
    );
    std::fs::write(out.join("result.json"), serde_json::to_vec_pretty(&result)?)?;
    Ok(())
}

type Fixture = fn(Arc<Window>, &Path) -> Result<(), Box<dyn std::error::Error>>;

struct App<'a>(&'a Path, Fixture);
impl ApplicationHandler for App<'_> {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        let window = Arc::new(
            event_loop
                .create_window(Window::default_attributes().with_visible(false))
                .unwrap(),
        );
        if let Err(error) = (self.1)(window, self.0) {
            eprintln!("fixture failed: {error}");
            std::process::exit(1);
        }
        event_loop.exit();
    }
    fn window_event(&mut self, _: &ActiveEventLoop, _: WindowId, _: WindowEvent) {}
}

fn run(out: &Path, fixture: Fixture) {
    for key in [
        "SUNRISE_HOME",
        "SUNRISE_LAUNCHER_HOME",
        "XDG_DATA_HOME",
        "XDG_CONFIG_HOME",
    ] {
        assert!(
            std::env::var_os(key).is_some(),
            "set throwaway {key} (AGENTS.md)"
        );
    }
    let mut builder = EventLoop::builder();
    #[cfg(target_os = "linux")]
    {
        use winit::platform::x11::EventLoopBuilderExtX11;
        builder.with_any_thread(true);
    }
    #[cfg(target_os = "windows")]
    {
        use winit::platform::windows::EventLoopBuilderExtWindows;
        builder.with_any_thread(true);
    }
    builder
        .build()
        .unwrap()
        .run_app(&mut App(out, fixture))
        .unwrap();
}

/// Runs `fixture` in a child process of this test binary that runs only `test`, with `env` set
/// and throwaway homes.
fn run_daily(test: &str, env: &[(&str, &str)], fixture: Fixture) {
    const OUTPUT: &str = "SUNRISE_TERRAIN_NORMALS_FIXTURE_OUTPUT";
    if let Some(output) = std::env::var_os(OUTPUT) {
        let mut descriptor = wgpu::InstanceDescriptor::new_without_display_handle();
        descriptor.backends = wgpu::Backends::VULKAN;
        let instance = wgpu::Instance::new(descriptor);
        pollster::block_on(instance.request_adapter(&Default::default()))
            .expect("pack_terrain_normals refused: no Vulkan adapter");
        run(Path::new(&output), fixture);
        return;
    }

    // Keep the event loop and every writable home in a fresh process.
    let scratch = tempfile::tempdir().unwrap();
    let mut command = std::process::Command::new(std::env::current_exe().unwrap());
    command
        .args(["--exact", test, "--ignored", "--nocapture"])
        .current_dir(scratch.path())
        .env(OUTPUT, scratch.path().join("output"))
        .env("SUNRISE_LAUNCHER_NO_SELF_UPDATE", "1")
        .env("SUNRISE_NO_AUDIO", "1")
        .env("SUNRISE_TELEMETRY_URL", "off")
        .envs(env.iter().copied());
    for name in [
        "SUNRISE_HOME",
        "SUNRISE_LAUNCHER_HOME",
        "XDG_DATA_HOME",
        "XDG_CONFIG_HOME",
        "XDG_RUNTIME_DIR",
        "SUNRISE_LAUNCHER_TRASH",
        "SUNRISE_LAUNCHER_CLIPBOARD",
        "SUNRISE_LAUNCHER_DESKTOP",
    ] {
        let home = scratch.path().join(name);
        std::fs::create_dir(&home).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&home, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        command.env(name, home);
    }
    assert!(
        command.status().unwrap().success(),
        "terrain normal fixture {test} failed"
    );
}

fn fixture_frame(hud: &HudBatch) -> Frame<'_> {
    let eye = glam::DVec3::from_array(EYE);
    Frame {
        weather: &[],
        weather_time: client::weather::Time::new(0, 0.0),
        camera: eye,
        yaw: 0.0,
        pitch: 0.0,
        fov_degrees: 85.0,
        cull_fov_degrees: 85.0,
        eye,
        eye_rotation: [0.0, 0.0],
        first_person: true,
        bob: glam::Mat4::IDENTITY,
        spin: glam::Mat4::IDENTITY,
        sky: client::environment::Sky::default(),
        shadow_scene_revision: (0, 0),
        shadow_cache: false,
        distance: 64.0,
        detail_distance: 64.0,
        shadow_distance: None,
        depth_far: 1000.0,
        underwater: false,
        water_plane: None,
        cave_culling: false,
        title: false,
        sky_blocked: false,
        skybox: false,
        end_sky: false,
        end_sky_sprite: None,
        boss_world_fog: false,
        environment_fog: None,
        water_fog: [0.0; 3],
        fog: None,
        dark_disc: false,
        outline: Vec::new(),
        outline_after_terrain: false,
        outline_high_contrast: false,
        pvp: Default::default(),
        debug_lines: &[],
        debug_lines_on_top: &[],
        debug_crosshair: &[],
        hud,
        glint_alpha: 0.75,
        glint_ticks: 0.0,
        nether_lighting: false,
        entities: &[],
        explicit_particles: None,
        static_block_entities: &[],
        visible_static_block_entity_sections: &[],
        static_block_entity_stats: Default::default(),
        entity_motion: &[],
        entity_motion_posed: &[],
        entity_spans: &[],
        skin: None,
        glowing: &[],
        dynamic_lights: &[],
        colored_lights: &[],
        emissive_layers: &[],
        clouds: None,
        lightning: Default::default(),
        additive: &[],
        see_through: &[],
        world_primitives: Default::default(),
        shadow_only: &[],
        celestial: &[],
        celestial_bodies: &[],
        stars: None,
        hand: &[],
        hand_items: &[],
        hand_fov_degrees: 70.0,
        screen: &[],
        pack: Default::default(),
        border: None,
        game_time: 0.0,
        shader_game_time: 0.0,
        end_portal_sprites: [[0.0; 4]; 2],
        crumbling_sprite: [0.0; 4],
        ticks: 0,
        darkness: (0.0, 0.0),
        night_vision: 0.0,
        block_flicker: 0.0,
        boss_darken: 0.0,
        chunk_fade: 0.0,
        post_effects: client::post_chain::Request::none(),
        menu_blur: None,
    }
}
