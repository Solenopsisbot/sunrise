//! `GuiDraw::PlayerFace` as [`paint_checked`] draws it: `PlayerFaceExtractor.extractRenderState`
//! from a synthetic skin, a 64 x 64 slot at a power-of-two place in a 1024 x 1024 atlas, whose
//! face (8, 8) and hat (40, 8) areas are told apart by their exact UVs. The face comes first,
//! then the hat when asked for, each `size` square under the op's pose and tint; it is upside down
//! only when `flip` is set. Any size and any UUID paints.
use super::{GuiDraw, GuiFaces, GuiItems, PoseOp, abgr, paint_checked};
use crate::hud::{HudAtlas, HudBatch, Rect};
use std::borrow::Cow;

/// The synthetic skin: a live texture of the atlas, at texel (512, 256) of 1024 x 1024.
const SKIN: &str = "test/player_face";
const PLAYER: u128 = 0x853c_80ef_3c37_49fd_aa49_938b_674a_dae6;

/// The atlas UVs of the skin's columns and rows: `512 + u` / 1024 and `256 + v` / 1024.
const U8: f32 = 0.507_812_5;
const U16: f32 = 0.515_625;
const U40: f32 = 0.539_062_5;
const U48: f32 = 0.546_875;
const V8: f32 = 0.257_812_5;
const V16: f32 = 0.265_625;

/// A 64 x 64 skin slot at texel (`x`, `y`) of a 1024 x 1024 atlas.
fn slot(x: f32, y: f32) -> Rect {
    Rect {
        u0: x / 1024.0,
        v0: y / 1024.0,
        u1: (x + 64.0) / 1024.0,
        v1: (y + 64.0) / 1024.0,
        w: 64,
        h: 64,
    }
}

fn atlas() -> HudAtlas {
    let mut atlas = HudAtlas::build(&crate::resource_source::ResourceSource::directory(
        "/nonexistent/sunrise-test-assets".into(),
    ))
    .unwrap();
    atlas.live.insert(SKIN.to_owned(), slot(512.0, 256.0));
    atlas
}

/// Gives [`SKIN`] for every UUID, recording which were asked for.
#[derive(Default)]
struct Faces {
    asked: Vec<u128>,
}
impl GuiFaces for Faces {
    fn skin(&mut self, uuid: u128) -> Cow<'_, str> {
        self.asked.push(uuid);
        Cow::Borrowed(SKIN)
    }
}

struct NoItems;
impl GuiItems for NoItems {
    fn item(&mut self, _: &mut HudBatch, _: u32, _: i32, _: i32, _: i32) {
        unreachable!("no item in these buffers")
    }
    fn decorations(&mut self, _: &mut HudBatch, _: u32, _: i32, _: i32) {
        unreachable!("no item in these buffers")
    }
}

fn face_of(uuid: u128, x: i32, y: i32, size: i32, hat: bool, flip: bool, color: u32) -> GuiDraw {
    GuiDraw::PlayerFace {
        uuid,
        x,
        y,
        size,
        hat,
        flip,
        color,
    }
}

fn face(x: i32, y: i32, size: i32, hat: bool, flip: bool, color: u32) -> GuiDraw {
    face_of(PLAYER, x, y, size, hat, flip, color)
}

/// One drawn quad: its position bounds, the UV at its top-left and bottom-right corners, and
/// its vertex colour.
#[derive(Debug)]
struct Quad {
    pos: [f32; 4],
    top_left: [f32; 2],
    bottom_right: [f32; 2],
    color: u32,
}

fn quads(batch: &HudBatch) -> Vec<Quad> {
    batch
        .vertices
        .chunks(6)
        .map(|q| {
            let min = |i: usize| q.iter().map(|v| v.pos[i]).fold(f32::INFINITY, f32::min);
            let max = |i: usize| q.iter().map(|v| v.pos[i]).fold(f32::NEG_INFINITY, f32::max);
            let pos = [min(0), min(1), max(0), max(1)];
            let at = |x: f32, y: f32| q.iter().find(|v| v.pos == [x, y]).unwrap().uv;
            assert!(q.iter().all(|v| v.color == q[0].color));
            Quad {
                pos,
                top_left: at(pos[0], pos[1]),
                bottom_right: at(pos[2], pos[3]),
                color: q[0].color,
            }
        })
        .collect()
}

/// The vertices `draws` paints, and the UUIDs whose skins were asked for.
fn painted_vertices(atlas: &HudAtlas, draws: &[GuiDraw]) -> (HudBatch, Vec<u128>) {
    let mut batch = HudBatch::scratch([854, 480]);
    let mut faces = Faces::default();
    assert_eq!(
        paint_checked(&mut batch, atlas, draws, &mut NoItems, &mut faces),
        Ok(())
    );
    (batch, faces.asked)
}

fn painted(atlas: &HudAtlas, draws: &[GuiDraw]) -> (Vec<Quad>, Vec<u128>) {
    let (batch, asked) = painted_vertices(atlas, draws);
    (quads(&batch), asked)
}

#[test]
fn the_skin_slot_is_at_its_power_of_two_place() {
    // The literal UVs below are exact only on such a slot: every step is a power of two.
    let r = slot(512.0, 256.0);
    assert_eq!([r.u0, r.v0, r.u1, r.v1], [0.5, 0.25, 0.5625, 0.3125]);
    assert_eq!(
        [U8, U16, U40, U48],
        [520.0, 528.0, 552.0, 560.0].map(|t| t / 1024.0)
    );
    assert_eq!([V8, V16], [264.0, 272.0].map(|t| t / 1024.0));
}

#[test]
fn a_face_draws_the_face_then_the_hat_at_its_size_tint_and_pose() {
    let atlas = atlas();
    let color = 0x80FF_8040;
    let draws = [
        GuiDraw::Push(vec![PoseOp::Translate(100.0, 10.0)]),
        face(2, 3, 16, true, false, color),
        GuiDraw::Pop,
    ];
    let (quads, asked) = painted(&atlas, &draws);
    assert_eq!(asked, [PLAYER]);
    assert_eq!(quads.len(), 2, "{quads:?}");
    for (quad, [u0, u1]) in quads.iter().zip([[U8, U16], [U40, U48]]) {
        assert_eq!(quad.pos, [102.0, 13.0, 118.0, 29.0]);
        assert_eq!(quad.color, abgr(color));
        assert_eq!(quad.top_left, [u0, V8], "{quad:?}");
        assert_eq!(quad.bottom_right, [u1, V16], "{quad:?}");
    }
}

#[test]
fn without_the_hat_only_the_face_is_drawn_and_flip_turns_it_upside_down() {
    let atlas = atlas();
    let (quads, _) = painted(&atlas, &[face(0, 0, 8, false, false, 0xFFFF_FFFF)]);
    assert_eq!(quads.len(), 1);
    assert_eq!(quads[0].pos, [0.0, 0.0, 8.0, 8.0]);
    assert_eq!(quads[0].top_left, [U8, V8]);
    assert_eq!(quads[0].bottom_right, [U16, V16]);

    // Flipped, the face's bottom row is drawn at the top: the same pixels, rows reversed.
    let (quads, _) = painted(&atlas, &[face(0, 0, 8, true, true, 0xFFFF_FFFF)]);
    assert_eq!(quads.len(), 2);
    for (quad, [u0, u1]) in quads.iter().zip([[U8, U16], [U40, U48]]) {
        assert_eq!(quad.pos, [0.0, 0.0, 8.0, 8.0]);
        assert_eq!(quad.top_left, [u0, V16], "{quad:?}");
        assert_eq!(quad.bottom_right, [u1, V8], "{quad:?}");
    }
}

#[test]
fn any_size_and_any_uuid_paints_its_quads() {
    let atlas = atlas();
    for uuid in [0, u128::MAX, PLAYER] {
        for size in [0, -16, i32::MAX] {
            for (hat, quads) in [(false, 1), (true, 2)] {
                let draws = [face_of(uuid, 4, 4, size, hat, false, 0xFFFF_FFFF)];
                let (batch, asked) = painted_vertices(&atlas, &draws);
                assert_eq!(asked, [uuid]);
                assert_eq!(
                    batch.vertices.len(),
                    6 * quads,
                    "uuid {uuid:032x}, size {size}, hat {hat}"
                );
                let far = 4.0 + size as f32;
                assert!(batch.vertices.iter().all(|v| {
                    (v.pos[0] == 4.0 || v.pos[0] == far) && (v.pos[1] == 4.0 || v.pos[1] == far)
                }));
            }
        }
    }
}
