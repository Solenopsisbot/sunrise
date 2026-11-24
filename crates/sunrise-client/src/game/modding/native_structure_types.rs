//! Native worker adapters for API-owned structure behaviours.
use super::native_pool_elements::{
    Elements, element_ref, engine_element, engine_placement, engine_projection,
};
use super::native_structures::engine_rotation;
use super::native_worldgen::WorkerResources;
use mod_api::{
    BiomeId, ChunkPos, GenerationContext, HeightmapType, NoiseBlock, NoiseColumn, PieceContext,
    RandomSource, StructureError, StructurePiece, StructureType,
};
use mod_api::{BlockKey, BlockMetadata, BlockState};
use std::sync::Arc;
use sunrise_core::{
    bounding_box::BoundingBox,
    nbt::{Compound, Tag},
    random::{LegacyRandom, WorldgenRandom},
};
use sunrise_world::worldgen::{
    placement::PlacementQueryError,
    structure::{
        jigsaw::FeatureKey,
        legacy::{PieceDraws, PieceRandom},
        mod_types::ModStructureType,
    },
};

fn engine_heightmap(kind: HeightmapType) -> sunrise_world::heightmap::HeightmapType {
    use sunrise_world::heightmap::HeightmapType as Engine;
    match kind {
        HeightmapType::WorldSurfaceWg => Engine::WorldSurfaceWg,
        HeightmapType::WorldSurface => Engine::WorldSurface,
        HeightmapType::OceanFloorWg => Engine::OceanFloorWg,
        HeightmapType::OceanFloor => Engine::OceanFloor,
        HeightmapType::MotionBlocking => Engine::MotionBlocking,
        HeightmapType::MotionBlockingNoLeaves => Engine::MotionBlockingNoLeaves,
        _ => Engine::WorldSurfaceWg,
    }
}

struct Column(NoiseColumn);
impl sunrise_world::worldgen::structure::start::NoiseColumnTarget for Column {
    fn resize(&mut self, min: i32, height: usize) {
        self.0.min = min;
        self.0.blocks.clear();
        self.0.blocks.resize(height, NoiseBlock::Air);
    }
    fn set(&mut self, index: usize, block: sunrise_world::worldgen::structure::start::NoiseBlock) {
        use sunrise_world::worldgen::structure::start::NoiseBlock as Engine;
        self.0.blocks[index] = match block {
            Engine::Air => NoiseBlock::Air,
            Engine::Water => NoiseBlock::Water,
            Engine::Lava => NoiseBlock::Lava,
            Engine::Solid => NoiseBlock::Solid,
            Engine::Other => NoiseBlock::Other,
        };
    }
}

/// The engine's piece random, as a [`RandomSource`].
pub(super) struct Draws<'a, R: PieceDraws + ?Sized>(pub(super) &'a mut R);
impl<R: PieceDraws + ?Sized> RandomSource for Draws<'_, R> {
    fn next_int_bound(&mut self, bound: i32) -> i32 {
        self.0.int(bound)
    }
    fn next_long(&mut self) -> i64 {
        self.0.long()
    }
    fn next_float(&mut self) -> f32 {
        self.0.float()
    }
    fn next_double(&mut self) -> f64 {
        self.0.double()
    }
    fn next_boolean(&mut self) -> bool {
        self.0.boolean()
    }
    fn next_int(&mut self) -> i32 {
        self.0.int_unbounded()
    }
    fn next_gaussian(&mut self) -> f64 {
        self.0.gaussian()
    }
}

/// The engine's start, as a [`GenerationContext`].
struct Start<'a> {
    inner: &'a mut dyn sunrise_world::worldgen::structure::start::StartContext,
    random: Draws<'a, WorldgenRandom<LegacyRandom>>,
    chunk: ChunkPos,
    config: &'a Arc<Tag>,
    worker: &'a WorkerResources,
    inputs: &'a sunrise_world::worldgen::structure::mod_types::StructureInputs<'a>,
    /// The jigsaw pieces [`GenerationContext::assemble_jigsaw`] laid out, ahead of the type's own.
    assembled: Vec<sunrise_world::worldgen::structure::start::Piece>,
    elements: Elements<'a>,
}

/// A mod's alias bindings as the engine's; `None` when a weighted list is not one vanilla's
/// codec accepts.
fn engine_aliases(
    bindings: &[mod_api::PoolAliasBinding],
) -> Option<Vec<sunrise_world::worldgen::structure::start::PoolAliasBinding>> {
    use mod_api::PoolAliasBinding as Api;
    use sunrise_world::worldgen::{
        structure::start::PoolAliasBinding as Engine, weighted::WeightedList,
    };
    bindings
        .iter()
        .map(|binding| {
            Some(match binding {
                Api::Direct { alias, target } => Engine::Direct {
                    alias: alias.to_string(),
                    target: target.to_string(),
                },
                Api::Random { alias, targets } => Engine::Random {
                    alias: alias.to_string(),
                    targets: WeightedList::new(
                        targets
                            .iter()
                            .map(|(target, weight)| (target.to_string(), *weight))
                            .collect(),
                    )
                    .ok()?,
                },
                Api::RandomGroup { groups } => Engine::RandomGroup {
                    groups: WeightedList::new(
                        groups
                            .iter()
                            .map(|(group, weight)| Some((engine_aliases(group)?, *weight)))
                            .collect::<Option<_>>()?,
                    )
                    .ok()?,
                },
                _ => return None,
            })
        })
        .collect()
}

impl GenerationContext for Start<'_> {
    fn metadata(&self) -> &dyn BlockMetadata {
        self.worker.metadata
    }
    fn chunk(&self) -> ChunkPos {
        self.chunk
    }
    fn seed(&self) -> i64 {
        self.inner.seed()
    }
    fn sea_level(&self) -> i32 {
        self.inner.sea_level()
    }
    fn min_y(&self) -> i32 {
        self.inner.min_y()
    }
    fn max_y(&self) -> i32 {
        self.inner.max_y()
    }
    fn random(&mut self) -> &mut dyn RandomSource {
        &mut self.random
    }
    fn base_height(&mut self, x: i32, z: i32, kind: HeightmapType) -> i32 {
        self.inner.base_height(x, z, engine_heightmap(kind))
    }
    fn base_columns(&mut self, x: i32, z: i32, kinds: &[HeightmapType]) -> Vec<Vec<bool>> {
        self.inner
            .base_columns_by(x, z, kinds.len(), &|index| engine_heightmap(kinds[index]))
    }
    fn noise_column(&mut self, x: i32, z: i32) -> NoiseColumn {
        let mut column = Column(NoiseColumn::default());
        self.inner.noise_column_into(x, z, &mut column);
        column.0
    }
    fn noise_biome(&mut self, quart: [i32; 3]) -> BiomeId {
        mod_api::__private::biome_id(self.inner.noise_biome(quart).0)
    }
    fn column_has_valid_biome(&mut self, x: i32, z: i32, min_y: i32, max_y: i32) -> bool {
        self.inner.column_has_valid_biome(x, z, min_y, max_y)
    }
    fn config(&self) -> Arc<Tag> {
        self.config.clone()
    }
    fn assemble_jigsaw(
        &mut self,
        settings: &mod_api::JigsawSettings,
        position: [i32; 3],
    ) -> Option<mod_api::JigsawLayout> {
        let bindings = engine_aliases(&settings.pool_aliases)?;
        let aliases = sunrise_world::worldgen::structure::start::resolve_aliases(
            &bindings,
            position,
            self.inner.seed(),
        );
        let start_pool = settings.start_pool.to_string();
        let start_jigsaw = settings.start_jigsaw_name.as_ref().map(ToString::to_string);
        let config = sunrise_world::worldgen::structure::jigsaw::JigsawConfig {
            start_pool: &start_pool,
            start_jigsaw: start_jigsaw.as_deref(),
            max_depth: settings.max_depth,
            use_expansion_hack: settings.use_expansion_hack,
            project_to: settings.project_start_to_heightmap.map(engine_heightmap),
            max_distance: [
                settings.max_distance_from_center.horizontal,
                settings.max_distance_from_center.vertical,
            ],
            aliases: &aliases,
            padding: [
                settings.dimension_padding.bottom,
                settings.dimension_padding.top,
            ],
            waterlogging: settings.liquid_settings == mod_api::LiquidSettings::ApplyWaterlogging,
        };
        let (position, pieces) = self.inputs.assemble_jigsaw(
            &config,
            &mut *self.inner,
            &mut *self.random.0,
            position,
        )?;
        let layout = mod_api::__private::jigsaw_layout(
            position,
            pieces.iter().map(|piece| piece.bounding_box()).collect(),
        );
        self.assembled.extend(pieces);
        Some(layout)
    }
    fn template_pool(&mut self, name: &mod_api::Identifier) -> Option<mod_api::TemplatePool> {
        let pool = self.inputs.pool(&name.to_string())?;
        let fallback = mod_api::Identifier::parse(pool.fallback()).ok()?;
        Some(mod_api::__private::template_pool(
            pool.templates().iter().cloned().map(element_ref).collect(),
            fallback,
            pool.max_size(self.inputs.resources),
        ))
    }
    fn elements(&self) -> &dyn mod_api::ElementContext {
        &self.elements
    }
}

/// A mod's pool piece as the engine's; `None` for an element or rotation
/// this build doesn't know.
fn engine_pool_piece(
    piece: mod_api::PoolPiece,
) -> Option<sunrise_world::worldgen::structure::start::Piece> {
    use sunrise_world::worldgen::structure::jigsaw::{Junction, PoolPiece};
    Some(sunrise_world::worldgen::structure::start::Piece::Pool(
        PoolPiece {
            element: engine_element(&piece.element)?,
            position: piece.position,
            ground_level_delta: piece.ground_level_delta,
            rotation: engine_rotation(piece.rotation)?,
            bounding_box: piece.bounding_box,
            junctions: piece
                .junctions
                .iter()
                .map(|junction| Junction {
                    source_x: junction.source_x,
                    source_ground_y: junction.source_ground_y,
                    source_z: junction.source_z,
                    delta_y: junction.delta_y,
                    dest_projection: engine_projection(junction.dest_projection),
                })
                .collect(),
            waterlogging: piece.liquid == mod_api::LiquidSettings::ApplyWaterlogging,
        },
    ))
}

/// The engine's piece context, as a [`PieceContext`].
pub(super) struct Placing<'a> {
    world: &'a mut dyn sunrise_world::worldgen::structure::legacy::PieceWorld,
    worker: &'a WorkerResources,
    resources: &'a sunrise_world::worldgen::structure::StructureResources,
    blocks: &'a dyn sunrise_world::worldgen::structure::legacy::SolidRender,
    random: Draws<'a, dyn PieceRandom + 'a>,
    chunk_box: BoundingBox,
    chunk: ChunkPos,
}

impl<'a> Placing<'a> {
    pub(super) fn new<'b>(
        inner: &'a mut sunrise_world::worldgen::structure::legacy::PieceContext<'b>,
    ) -> Result<Self, PlacementQueryError> {
        let worker = WorkerResources::get(inner.resources.registry)?;
        Ok(Self {
            world: inner.world,
            worker,
            resources: inner.resources,
            blocks: inner.blocks,
            random: Draws(inner.random),
            chunk_box: inner.chunk_box,
            chunk: inner.chunk,
        })
    }
}

impl PieceContext for Placing<'_> {
    fn metadata(&self) -> &dyn BlockMetadata {
        self.worker.metadata
    }
    fn chunk(&self) -> ChunkPos {
        self.chunk
    }
    fn chunk_box(&self) -> BoundingBox {
        self.chunk_box
    }
    fn random(&mut self) -> &mut dyn RandomSource {
        &mut self.random
    }
    fn state(&self, block: BlockKey) -> BlockState {
        self.worker.state(block)
    }
    fn state_with(&self, block: BlockKey, properties: &[(&str, &str)]) -> BlockState {
        self.worker
            .state_with(self.resources.registry, block, properties)
    }
    fn is_air(&self, state: BlockState) -> bool {
        self.worker.is_air(state)
    }
    fn block(&mut self, pos: [i32; 3]) -> Option<BlockState> {
        self.world
            .block(pos)
            .ok()
            .and_then(|state| self.worker.index.to_api(state).ok())
    }
    #[cfg_attr(not(debug_assertions), allow(unused_variables))]
    fn set_block(&mut self, pos: [i32; 3], state: BlockState) -> bool {
        if !self.chunk_box.is_inside(pos) {
            #[cfg(debug_assertions)]
            log::debug!(
                "mod structure piece write at {:?} outside the chunk box",
                pos
            );
            return false;
        }
        let Ok(state) = self.worker.index.to_engine(state) else {
            return false;
        };
        sunrise_world::worldgen::structure::legacy::write_block(
            self.world,
            self.resources,
            pos,
            state,
        )
        .unwrap_or(false)
    }
    fn height(&mut self, kind: HeightmapType, x: i32, z: i32) -> Option<i32> {
        self.world.height(engine_heightmap(kind), x, z).ok()
    }
    fn solid_render(&self, state: BlockState) -> bool {
        self.worker
            .index
            .to_engine(state)
            .is_ok_and(|state| self.blocks.solid_render(state))
    }
    fn entity(&mut self, nbt: Compound) {
        if self.world.entity(nbt).is_err() {
            log::error!("Sunrise: a mod structure piece's entity was refused");
        }
    }
    fn block_entity(&mut self, pos: [i32; 3], nbt: Compound) {
        if self.world.block_entity(pos, nbt).is_err() {
            log::error!("Sunrise: a mod structure piece's block entity was refused");
        }
    }
    fn place_feature(&mut self, feature: &str, pos: [i32; 3]) -> Result<bool, StructureError> {
        self.world
            .place_feature(&mut *self.random.0, FeatureKey::Configured(feature), pos)
            .map_err(|e| StructureError(e.to_string()))
    }
    fn place_placed_feature(
        &mut self,
        placed_feature: &str,
        pos: [i32; 3],
    ) -> Result<bool, StructureError> {
        self.world
            .place_feature(&mut *self.random.0, FeatureKey::Placed(placed_feature), pos)
            .map_err(|e| StructureError(e.to_string()))
    }
    fn place_element(
        &mut self,
        element: &mod_api::PoolElementRef,
        placement: &mod_api::ElementPlacement,
    ) -> Result<bool, StructureError> {
        let (Some(element), Some(at)) = (engine_element(element), engine_placement(placement))
        else {
            return Err(StructureError(
                "an element or rotation this build doesn't know".into(),
            ));
        };
        self.world
            .place_element(&mut *self.random.0, &element, at)
            .map_err(|e| StructureError(e.to_string()))
    }
}

/// The adapter: the game's `ModStructureType` around a mod's `StructureType`,
/// and its `LegacyPiece` around the mod's `StructurePiece`s. One adapter, no
/// second mechanism: the pieces a type returns become the engine's legacy
/// pieces, so the start cache, the saving, the cross-chunk placement and the
/// beardifier treat them like a fortress piece's.
pub(crate) struct Adapter {
    inner: Arc<dyn StructureType>,
    /// The type's namespaced id (`modid:tower`), leaked once at registration
    /// for the piece tags' `"id"`.
    id: &'static str,
}

impl ModStructureType for Adapter {
    fn prepare_config(&self, config: &serde_json::Value) -> Result<Arc<Tag>, String> {
        crate::json_nbt::json_to_tag(config)
            .map(Arc::new)
            .map_err(|error| format!("{}: {error}", self.id))
    }
    fn find_generation_point(
        &self,
        context: &mut dyn sunrise_world::worldgen::structure::start::StartContext,
        inputs: &sunrise_world::worldgen::structure::mod_types::StructureInputs<'_>,
        chunk: ChunkPos,
        config: &Arc<Tag>,
        random: &mut WorldgenRandom<LegacyRandom>,
    ) -> Option<(
        [i32; 3],
        Vec<sunrise_world::worldgen::structure::start::Piece>,
    )> {
        let worker = WorkerResources::get(inputs.resources.registry).ok()?;
        let mut context = Start {
            worker,
            inner: context,
            random: Draws(random),
            chunk,
            config,
            inputs,
            assembled: Vec::new(),
            elements: Elements {
                resources: inputs.resources,
                worker,
            },
        };
        let found = self.inner.find_generation_point(&mut context)?;
        let ty = self.inner.clone();
        let id = self.id;
        let mut pieces = context.assembled;
        pieces.extend(found.pool_pieces.into_iter().filter_map(engine_pool_piece));
        pieces.extend(found.pieces.into_iter().map(|piece| {
            sunrise_world::worldgen::structure::start::Piece::Legacy(Box::new(ModPiece {
                beardifier: piece.beardifier().and_then(engine_beardifier),
                piece,
                ty: ty.clone(),
                id,
            }))
        }));
        Some((found.position, pieces))
    }
    fn load_piece(
        &self,
        registry: &sunrise_world::registry::BlockRegistry,
        tag: &Compound,
    ) -> Option<Box<dyn sunrise_world::worldgen::structure::legacy::LegacyPiece>> {
        let metadata = WorkerResources::get(registry).ok()?.metadata;
        // Preserve the engine's existing bad-piece refusal. The API loader's diagnostic is typed.
        let piece = self.inner.load_piece(metadata, tag).ok()?;
        Some(Box::new(ModPiece {
            beardifier: piece.beardifier().and_then(engine_beardifier),
            piece,
            ty: self.inner.clone(),
            id: self.id,
        }))
    }
}

/// The engine's rigid piece for a mod piece's answer; bounds whose minimum is above their
/// maximum are mod-controlled input, refused with a warning (the piece adds no terrain
/// adjustment) instead of trusting the decoder to have ordered them.
fn engine_beardifier(piece: mod_api::PieceBeardifier) -> Option<sunrise_core::density::RigidPiece> {
    use mod_api::TerrainAdjustment as Api;
    use sunrise_core::density::{StructureBounds, TerrainAdjustment as Engine};
    let adjustment = match piece.adjustment {
        Api::None => Engine::None,
        Api::Bury => Engine::Bury,
        Api::BeardThin => Engine::BeardThin,
        Api::BeardBox => Engine::BeardBox,
        Api::Encapsulate => Engine::Encapsulate,
    };
    let Some(bounds) = StructureBounds::new(piece.bounds.min, piece.bounds.max) else {
        log::warn!(
            "a mod structure piece's beardifier bounds {:?}..{:?} are not ordered: it adds no terrain adjustment",
            piece.bounds.min,
            piece.bounds.max
        );
        return None;
    };
    Some(sunrise_core::density::RigidPiece {
        bounds,
        adjustment,
        ground_level_delta: piece.ground_level_delta,
    })
}

/// The engine's legacy piece around a mod's piece: `createTag` of the type id
/// and the piece's box around the piece's own save data.
struct ModPiece {
    piece: Arc<dyn StructurePiece>,
    beardifier: Option<sunrise_core::density::RigidPiece>,
    /// The type the piece belongs to, whose `after_place` runs for the start.
    ty: Arc<dyn StructureType>,
    id: &'static str,
}
impl sunrise_world::worldgen::structure::legacy::LegacyPiece for ModPiece {
    fn beardifier(&self) -> Option<sunrise_core::density::RigidPiece> {
        self.beardifier
    }
    fn bounding_box(&self) -> BoundingBox {
        self.piece.bounding_box()
    }
    fn type_name(&self) -> &'static str {
        self.id
    }
    fn save(&self) -> Option<Compound> {
        let mut tag = sunrise_world::worldgen::structure::legacy::piece_tag(
            self.id,
            self.bounding_box(),
            None,
            0,
        );
        self.piece.add_additional_save_data(&mut tag);
        Some(tag)
    }
    fn post_process(
        &self,
        context: &mut sunrise_world::worldgen::structure::legacy::PieceContext<'_>,
    ) -> std::result::Result<(), PlacementQueryError> {
        let mut context = Placing::new(context)?;
        self.piece
            .post_process(&mut context)
            .map_err(|e| PlacementQueryError::Failed(e.0))
    }
    /// The engine runs the first piece's `afterPlace`; it is the type's
    /// [`StructureType::after_place`] the call reaches, with every piece's
    /// box — the piece itself has no `after_place`.
    fn after_place(
        &self,
        context: &mut sunrise_world::worldgen::structure::legacy::PieceContext<'_>,
        pieces: &[BoundingBox],
    ) -> std::result::Result<(), PlacementQueryError> {
        let mut context = Placing::new(context)?;
        self.ty
            .after_place(&mut context, pieces)
            .map_err(|e| PlacementQueryError::Failed(e.0))
    }
}
impl std::fmt::Debug for ModPiece {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ModPiece")
            .field("id", &self.id)
            .field("piece", &self.piece)
            .finish()
    }
}

impl Adapter {
    /// Wrap `ty` as the engine's type, under the namespaced `id`.
    pub(crate) fn new(id: &str, ty: Arc<dyn StructureType>) -> Self {
        Self {
            inner: ty,
            id: Box::leak(id.to_owned().into_boxed_str()),
        }
    }
    /// The engine's type behind an `Arc`, for the registration table.
    pub(crate) fn into_engine_type(self) -> Arc<dyn ModStructureType> {
        Arc::new(self)
    }
}

#[cfg(test)]
#[path = "../../../../../docs/modding/examples/src/worldgen.rs"]
#[allow(dead_code)] // The book example is compiled for its signatures only; TODO(#2273)
pub(super) mod book_example;
#[cfg(test)]
#[path = "native_structure_types/tests.rs"]
pub(in crate::game) mod tests;

/// Registered API declarations become engine adapters once, not once per chunk.
pub(in crate::game) fn installed_structure_types()
-> sunrise_world::worldgen::structure::mod_types::ModStructureTypes {
    mod_api::__private::structure_type_declarations()
        .into_iter()
        .map(|(id, ty)| {
            let adapter = Adapter::new(&id, ty).into_engine_type();
            (id, adapter)
        })
        .collect()
}
