use super::*;
use crate::game::native_structure_types::tests::{
    RecordingWorld, StubContext, link_inputs, test_resources,
};
use mod_api::{
    FoundStructure, GenerationContext, PieceContext as _, PoolPiece, StructurePiece, StructureType,
    Tag,
};
use sunrise_core::{
    position::ChunkPos,
    random::{LegacyRandom, WorldgenRandom},
};
use sunrise_world::worldgen::structure::{
    legacy::{PieceContext as EngineContext, SolidRender},
    mod_types::ModStructureType,
    start::Piece,
};

struct NoSolid;
impl SolidRender for NoSolid {
    fn solid_render(&self, _: sunrise_world::registry::BlockStateId) -> bool {
        false
    }
}

fn link_element() -> serde_json::Value {
    serde_json::json!({"element_type": "minecraft:single_pool_element", "location": "test:link",
        "projection": "rigid", "processors": {"processors": []}})
}

/// An element that wraps another: the wrapped element's jigsaws and box,
/// and a stone block under it before it places the wrapped element.
#[derive(Debug)]
struct Stilted {
    inner: PoolElementRef,
    inner_config: Tag,
    /// The projection each `place` was handed.
    placed_as: std::sync::Mutex<Vec<mod_api::Projection>>,
}
impl mod_api::PoolElement for Stilted {
    fn shuffled_jigsaws(
        &self,
        context: &dyn ElementContext,
        position: [i32; 3],
        rotation: StructureRotation,
        random: &mut dyn RandomSource,
    ) -> Vec<JigsawBlock> {
        context.jigsaws(&self.inner, position, rotation, random)
    }
    fn bounding_box(
        &self,
        context: &dyn ElementContext,
        position: [i32; 3],
        rotation: StructureRotation,
    ) -> BoundingBox {
        context.bounding_box(&self.inner, position, rotation)
    }
    fn place(
        &self,
        context: &mut dyn mod_api::PieceContext,
        placement: &ElementPlacement,
    ) -> Result<bool, StructureError> {
        let stone = context.state(mod_api::__private::block_key("minecraft:stone").unwrap());
        let [x, y, z] = placement.position;
        self.placed_as.lock().unwrap().push(placement.projection);
        context.set_block([x, y - 1, z], stone);
        context.place_element(&self.inner, placement)
    }
    fn projection(&self) -> mod_api::Projection {
        self.inner.projection()
    }
    fn save(&self) -> Compound {
        Compound::from_iter([("inner".into(), self.inner_config.clone())])
    }
}
struct StiltedType;
impl PoolElementType for StiltedType {
    fn decode(
        &self,
        context: &dyn ElementDecodeContext,
        config: &Tag,
    ) -> Result<Arc<dyn mod_api::PoolElement>, StructureError> {
        let inner_config = config
            .get("inner")
            .ok_or_else(|| StructureError("no inner element".into()))?
            .clone();
        Ok(Arc::new(Stilted {
            inner: context.element(&inner_config)?,
            inner_config,
            placed_as: Default::default(),
        }))
    }
}

/// A mod element decoded through the adapter answers the assembly with the
/// element it wraps: the same jigsaws in the same order for the same random,
/// and the same box. Placed in a chunk, it writes its own block and places
/// the wrapped element at the same spot, told the projection in effect (a
/// list's, here); saved, it writes its fields beside its `element_type`.
#[test]
fn a_mod_element_wraps_another_through_the_adapter() {
    let inputs = link_inputs();
    let resources = inputs.resources;
    let bindings = FeatureBlockBindings::new(resources.registry, Default::default()).unwrap();
    let config = serde_json::json!({"element_type": "test:stilted", "inner": link_element()});
    let decoded = ElementType {
        inner: Arc::new(StiltedType),
    }
    .decode(&config, resources, &bindings)
    .unwrap();
    let element = PoolElement::Mod {
        id: "test:stilted".into(),
        element: decoded,
        projection: Projection::TerrainMatching,
    };
    let inner = PoolElement::decode_element(&link_element(), resources, &bindings).unwrap();
    let jigsaws = |element: &PoolElement| {
        element
            .shuffled_jigsaws(
                resources,
                [5, 64, 5],
                Rotation::Clockwise90,
                &mut LegacyRandom::new(1),
            )
            .into_iter()
            .map(|j| {
                (
                    j.pos,
                    j.front,
                    j.top,
                    j.pool.into_owned(),
                    j.target.into_owned(),
                )
            })
            .collect::<Vec<_>>()
    };
    assert_eq!(jigsaws(&element).len(), 2);
    assert_eq!(jigsaws(&element), jigsaws(&inner));
    assert_eq!(
        element.bounding_box(resources, [5, 64, 5], Rotation::None),
        BoundingBox::new([5, 64, 5], [5, 66, 5])
    );

    let handle = element_ref(Arc::new(element.clone()));
    assert!(handle.downcast_ref::<Stilted>().is_some());
    assert!(!handle.is_empty());
    assert!(element_ref(Arc::new(PoolElement::Empty)).is_empty());

    let air = resources.registry.default_state("minecraft:air").unwrap();
    let stone = resources.registry.default_state("minecraft:stone").unwrap();
    let mut world = RecordingWorld {
        air,
        writes: Vec::new(),
        states: Vec::new(),
        placed: Vec::new(),
    };
    let mut random = LegacyRandom::new(3);
    let PoolElement::Mod {
        element: mod_element,
        ..
    } = &element
    else {
        unreachable!()
    };
    let at = ElementAt {
        position: [5, 64, 5],
        reference: [8, 64, 8],
        rotation: Rotation::None,
        waterlogging: true,
        keep_jigsaws: false,
    };
    let placed = mod_element
        .place(
            &mut EngineContext {
                world: &mut world,
                resources,
                blocks: &NoSolid,
                random: &mut random,
                chunk_box: BoundingBox::new([0, -64, 0], [15, 320, 15]),
                chunk: ChunkPos::new(0, 0),
                reference: [8, 64, 8],
            },
            at,
            Projection::TerrainMatching,
        )
        .unwrap();
    assert!(placed);
    assert_eq!(
        *handle
            .downcast_ref::<Stilted>()
            .unwrap()
            .placed_as
            .lock()
            .unwrap(),
        [mod_api::Projection::TerrainMatching]
    );
    assert_eq!(world.writes, [[5, 63, 5]]);
    assert_eq!(world.states, [stone]);
    assert_eq!(world.placed, [format!("test:link at {at:?}")]);

    let saved = element.save();
    assert_eq!(
        saved.get("element_type"),
        Some(&Tag::String("test:stilted".into()))
    );
    assert_eq!(
        saved.get("inner").and_then(|inner| inner.get("location")),
        Some(&Tag::String("test:link".into()))
    );
}

/// A structure type lays a jigsaw chain out by its own rules from the data
/// pack's pool: the pool's elements, their jigsaws and boxes, and
/// `can_attach` to put a second link on the first's head. Its pieces are the
/// ones `assemble_jigsaw` lays out from the same pool and start. The game
/// keeps the assembled pieces and then the two pool pieces it hands back, as
/// jigsaw pieces, in that order.
#[test]
fn a_mod_type_lays_a_chain_out_by_hand_from_a_pool() {
    struct Chain;
    impl StructureType for Chain {
        fn find_generation_point(
            &self,
            context: &mut dyn GenerationContext,
        ) -> Option<FoundStructure> {
            let pool = context.template_pool(&Identifier::parse("test:links").ok()?)?;
            assert_eq!(
                (pool.size(), pool.fallback().to_string(), pool.max_size()),
                (1, "minecraft:empty".to_owned(), 3)
            );
            assert!(
                context
                    .template_pool(&Identifier::parse("test:missing").ok()?)
                    .is_none()
            );
            let start = pool.random_template(context.random())?.clone();
            let child = pool.shuffled_templates(context.random())[0].clone();
            // As `JigsawPlacement.addPieces` puts it: the start's ground
            // level (its bottom plus `ground_level_delta`) at the start height.
            let height = 64;
            let origin = [0, height - start.ground_level_delta(), 0];
            let rotation = StructureRotation::None;
            let mut random = LegacyRandom::new(9);
            let mut draws = Draws(&mut random);
            let elements = context.elements();
            let head = elements
                .jigsaws(&start, origin, rotation, &mut draws)
                .into_iter()
                .find(|j| j.front == mod_api::Direction::Up)?;
            let foot = elements
                .jigsaws(&child, [0; 3], rotation, &mut draws)
                .into_iter()
                .find(|j| head.can_attach(j))?;
            let at = std::array::from_fn(|i| head.pos[i] + [0, 1, 0][i] - foot.pos[i]);
            let piece = |element: PoolElementRef, position: [i32; 3]| {
                let bounding_box = elements.bounding_box(&element, position, rotation);
                PoolPiece::new(
                    element,
                    position,
                    1,
                    rotation,
                    bounding_box,
                    mod_api::LiquidSettings::ApplyWaterlogging,
                )
            };
            let pool_pieces = vec![piece(start, origin), piece(child, at)];
            let settings = mod_api::JigsawSettings::new(Identifier::parse("test:links").ok()?, 1);
            let assembled = context.assemble_jigsaw(&settings, [0, height, 0])?;
            assert_eq!(
                assembled.pieces,
                pool_pieces
                    .iter()
                    .map(|p| p.bounding_box)
                    .collect::<Vec<_>>()
            );
            Some(FoundStructure {
                position: origin,
                pool_pieces,
                pieces: Vec::new(),
            })
        }
        fn load_piece(
            &self,
            _: &dyn BlockMetadata,
            _: &Compound,
        ) -> Result<Arc<dyn StructurePiece>, StructureError> {
            Err(StructureError("the type saves no piece of its own".into()))
        }
    }
    let inputs = link_inputs();
    let mut random = WorldgenRandom::new(LegacyRandom::new(0));
    random.set_large_feature_seed(0, 0, 0);
    let (_, pieces) =
        super::super::native_structure_types::Adapter::new("testmod:chain", Arc::new(Chain))
            .find_generation_point(
                &mut StubContext,
                &inputs,
                ChunkPos::new(0, 0),
                &Arc::new(Tag::Compound(Compound::new())),
                &mut random,
            )
            .expect("a start from the pool");
    let boxes: Vec<BoundingBox> = pieces
        .iter()
        .map(|piece| match piece {
            Piece::Pool(piece) => piece.bounding_box,
            Piece::Legacy(_) => panic!("only pool pieces"),
        })
        .collect();
    let chain = [
        BoundingBox::new([0, 63, 0], [0, 65, 0]),
        BoundingBox::new([0, 66, 0], [0, 68, 0]),
    ];
    assert_eq!(boxes, [chain, chain].concat());
}

/// A piece places a configured feature and a placed feature through the
/// level the engine hands it, at the positions it names, each by its kind.
#[test]
fn a_piece_places_configured_and_placed_features() {
    let resources = test_resources();
    let air = resources.registry.default_state("minecraft:air").unwrap();
    let mut world = RecordingWorld {
        air,
        writes: Vec::new(),
        states: Vec::new(),
        placed: Vec::new(),
    };
    let mut random = LegacyRandom::new(7);
    {
        let mut engine = EngineContext {
            world: &mut world,
            resources,
            blocks: &NoSolid,
            random: &mut random,
            chunk_box: BoundingBox::new([0, -64, 0], [15, 320, 15]),
            chunk: ChunkPos::new(0, 0),
            reference: [8, 64, 8],
        };
        let mut context = Placing::new(&mut engine).unwrap();
        assert!(context.place_feature("test:tree", [1, 64, 1]).unwrap());
        assert!(
            context
                .place_placed_feature("test:patch", [2, 65, 2])
                .unwrap()
        );
    }
    assert_eq!(
        world.placed,
        [
            r#"Configured("test:tree") at [1, 64, 1]"#,
            r#"Placed("test:patch") at [2, 65, 2]"#,
        ]
    );
}
