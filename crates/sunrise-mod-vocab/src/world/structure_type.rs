//! Structure types a trait mod registers: vanilla's `StructureType` with its
//! own `Structure.findGenerationPoint` and `StructurePiece`s, as
//! [`Registrar::register_structure_type`](crate::Registrar::register_structure_type)
//! hands it over once at startup (NeoForge's registries `STRUCTURE_TYPE` and
//! `STRUCTURE_PIECE`). A data pack structure with `"type": "modid:tower"`
//! then generates by the mod's code.
//!
//! There is no `Host` here: starts build on the worldgen worker threads and
//! pieces place per chunk during generation, so the mod sees only the small
//! contexts below, `Send + Sync` and deterministic — the same seed gives the
//! same pieces, and a type keeps no mutable state because two workers can
//! build neighbouring chunks at once. Everything around the type stays the
//! game's own: the set's spacing, the structure's biomes and its
//! `terrain_adaptation`, and the pieces ride the one piece mechanism the game
//! has, next to a fortress piece's.
use super::pool_element::{
    ElementContext, ElementPlacement, PoolElementRef, PoolPiece, TemplatePool,
};
use crate::{BlockKey, BlockMetadata, BlockState, RandomSource};
use std::sync::Arc;
pub use sunrise_core::position::ChunkPos;
use sunrise_core::{
    bounding_box::BoundingBox,
    nbt::{Compound, Tag},
};

/// A piece's placement failed; the generation of that piece stops there, as a
/// piece's own `Result` does in the engine.
#[derive(Debug)]
// exhaustive: mods build it themselves as a piece's error answer
#[derive(sunrise_mod_api_macros::Wire)]
pub struct StructureError(pub String);
impl std::fmt::Display for StructureError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}
impl std::error::Error for StructureError {}
impl From<(crate::Identifier, String)> for StructureError {
    fn from((source, reason): (crate::Identifier, String)) -> Self {
        Self(format!("{source}: {reason}"))
    }
}

/// What [`StructureType::find_generation_point`](StructureType::find_generation_point)
/// found: the start's position (vanilla's `Structure.findGenerationPoint`
/// answer) and its pieces (`Structure.generatePieces`). The start holds the
/// pieces [`GenerationContext::assemble_jigsaw`] laid out first, then
/// `pool_pieces`, then `pieces`; it needs at least one piece in all.
#[derive(Default)]
// exhaustive: mods build it themselves as find_generation_point's answer
pub struct FoundStructure {
    /// Where the start sits, in world blocks.
    pub position: [i32; 3],
    /// Pool element pieces the type laid out itself, from the pools
    /// [`GenerationContext::template_pool`] reads. The game keeps, saves and
    /// places them as it does `assemble_jigsaw`'s pieces.
    pub pool_pieces: Vec<PoolPiece>,
    /// The type's own pieces, shared with the game, which keeps them in its start.
    pub pieces: Vec<Arc<dyn StructurePiece>>,
}

/// `JigsawPlacement.addPieces`' settings, as a type decodes them from its own
/// config for [`GenerationContext::assemble_jigsaw`]. The bounds
/// `JigsawStructure`'s codec puts on `size` and `max_distance_from_center`
/// belong to the `minecraft:jigsaw` type and do not apply here: a type uses
/// its own.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct JigsawSettings {
    /// The template pool the start piece is drawn from (`start_pool`).
    pub start_pool: crate::Identifier,
    /// The start piece's jigsaw that sits on the start position
    /// (`start_jigsaw_name`); with none, the piece's origin does.
    pub start_jigsaw_name: Option<crate::Identifier>,
    /// How many rounds of children are laid out (`size`). At 0 or below no
    /// piece is placed, as vanilla adds pieces only when `maxDepth > 0`.
    pub max_depth: i32,
    /// `use_expansion_hack`: the village roads' extra room for their children.
    pub use_expansion_hack: bool,
    /// `project_start_to_heightmap`: the start's height is the position's Y
    /// plus this heightmap's height under the start piece's centre.
    pub project_start_to_heightmap: Option<HeightmapType>,
    /// `max_distance_from_center`: how far children may reach from the start's
    /// centre, across and up or down.
    pub max_distance_from_center: JigsawReach,
    /// `pool_aliases`: pool names replaced for this start, drawn from the world
    /// seed at the start position.
    pub pool_aliases: Vec<PoolAliasBinding>,
    /// `dimension_padding`: rows at the level's bottom and top no piece enters.
    pub dimension_padding: DimensionPadding,
    /// `liquid_settings`: whether pieces take on the water they are placed in.
    pub liquid_settings: LiquidSettings,
}
impl JigsawSettings {
    /// Settings with the defaults `JigsawStructure`'s own constructors use for
    /// every field but these two: no start jigsaw, no expansion hack, no
    /// projection, a reach of 80 across and `DimensionType.Y_SIZE` up and
    /// down (`MaxDistance(80)`), no aliases, no padding, and waterlogging
    /// applied.
    pub fn new(start_pool: crate::Identifier, max_depth: i32) -> Self {
        Self {
            start_pool,
            start_jigsaw_name: None,
            max_depth,
            use_expansion_hack: false,
            project_start_to_heightmap: None,
            max_distance_from_center: JigsawReach {
                horizontal: 80,
                // DimensionType.Y_SIZE.
                vertical: 4064,
            },
            pool_aliases: Vec::new(),
            dimension_padding: DimensionPadding { bottom: 0, top: 0 },
            liquid_settings: LiquidSettings::ApplyWaterlogging,
        }
    }
}

/// `JigsawStructure.MaxDistance`: the reach of a jigsaw's children from its
/// start's centre.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
// exhaustive: vanilla's MaxDistance has these two fields.
pub struct JigsawReach {
    /// Blocks along X and Z.
    pub horizontal: i32,
    /// Blocks up and down.
    pub vertical: i32,
}

/// `DimensionPadding`: rows at the level's bottom and top a jigsaw keeps clear.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
// exhaustive: vanilla's DimensionPadding has these two fields.
pub struct DimensionPadding {
    /// Rows above the level's minimum Y.
    pub bottom: i32,
    /// Rows below the level's maximum Y.
    pub top: i32,
}

/// `LiquidSettings`: whether a placed piece's blocks take on the water there.
#[derive(Clone, Copy, Debug, PartialEq, Eq, sunrise_mod_api_macros::Wire)]
// exhaustive: vanilla's LiquidSettings has these two values.
pub enum LiquidSettings {
    /// `apply_waterlogging`: waterloggable blocks placed in water are waterlogged.
    ApplyWaterlogging,
    /// `ignore_waterlogging`: blocks keep their template state.
    IgnoreWaterlogging,
}

/// One of `pool_aliases`: vanilla's `PoolAliasBinding` kinds. Weights follow
/// vanilla's weighted lists: none negative, and a total above 0 and at most
/// `i32::MAX`.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub enum PoolAliasBinding {
    /// `minecraft:direct`: `alias` always names `target`.
    Direct {
        /// The pool name jigsaws use.
        alias: crate::Identifier,
        /// The pool it stands for.
        target: crate::Identifier,
    },
    /// `minecraft:random`: `alias` names one of `targets`, drawn by weight.
    Random {
        /// The pool name jigsaws use.
        alias: crate::Identifier,
        /// The pools it may stand for, with their weights.
        targets: Vec<(crate::Identifier, i32)>,
    },
    /// `minecraft:random_group`: one group, drawn by weight, binds all its bindings.
    RandomGroup {
        /// The groups, with their weights.
        groups: Vec<(Vec<PoolAliasBinding>, i32)>,
    },
}

/// What [`GenerationContext::assemble_jigsaw`] laid out: vanilla's
/// `GenerationStub` of `JigsawPlacement.addPieces`.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct JigsawLayout {
    /// The start's position: the start piece's centre across, at its
    /// projected height plus the start jigsaw's offset.
    pub position: [i32; 3],
    /// Every piece's box, the start piece first, in the order they were placed.
    pub pieces: Vec<BoundingBox>,
}

/// A mod's structure type: vanilla's `StructureType`, whose code answers for a
/// data pack structure with `"type": "modid:tower"`. Implemented once, kept by
/// the game; deterministic and stateless.
pub trait StructureType: Send + Sync + 'static {
    /// `Structure.findGenerationPoint` with `generatePieces`: the start's
    /// position and its pieces for the chunk `context` names, or `None` for
    /// "no start in this chunk". The height and biome queries the context
    /// answers are the ones vanilla's own types ask of a
    /// `GenerationContext`; `context.config` lends its NBT config, converted once at load.
    fn find_generation_point(&self, context: &mut dyn GenerationContext) -> Option<FoundStructure>;
    /// The `StructurePieceType` loader: rebuild a saved piece from its tag
    /// (`StructurePieceType.load`), exactly as it was saved. Required: every
    /// piece type has its loader, and a saved piece is never regenerated. Resolve saved block
    /// states once here, against the same retained metadata the worker contexts borrow.
    /// Invalid saved blocks, properties or piece data return a typed error, never a panic.
    fn load_piece(
        &self,
        metadata: &dyn BlockMetadata,
        tag: &Compound,
    ) -> Result<Arc<dyn StructurePiece>, StructureError>;
    /// `Structure.afterPlace`, run once per chunk after all pieces were
    /// placed, with every piece's current box. Optional: most types place
    /// nothing here.
    fn after_place(
        &self,
        _context: &mut dyn PieceContext,
        _pieces: &[BoundingBox],
    ) -> Result<(), StructureError> {
        Ok(())
    }
}

/// The terrain-shaping rule around a structure piece.
#[derive(Clone, Copy, Debug, PartialEq, Eq, sunrise_mod_api_macros::Wire)]
// exhaustive: these are the five terrain rules supported by the density sampler.
pub enum TerrainAdjustment {
    /// Leave the surrounding terrain unchanged.
    None,
    /// Blend terrain around a buried piece.
    Bury,
    /// Shape terrain around the piece's ground plane.
    BeardThin,
    /// Shape terrain around the full piece box.
    BeardBox,
    /// Blend terrain around the box to enclose the piece.
    Encapsulate,
}

/// A piece's terrain shape, independent of its placement box and structure's setting.
#[derive(Clone, Copy, Debug, sunrise_mod_api_macros::Wire)]
#[non_exhaustive]
pub struct PieceBeardifier {
    /// Terrain-shaping box, independent of the piece's placement bounds.
    pub bounds: BoundingBox,
    /// Terrain adjustment used for this piece.
    pub adjustment: TerrainAdjustment,
    /// Ground-plane offset above the terrain box's minimum Y.
    pub ground_level_delta: i32,
}
impl PieceBeardifier {
    /// Construct the cached terrain metadata for one piece.
    pub fn new(
        bounds: BoundingBox,
        adjustment: TerrainAdjustment,
        ground_level_delta: i32,
    ) -> Self {
        Self {
            bounds,
            adjustment,
            ground_level_delta,
        }
    }
}

/// One piece of a mod's structure: vanilla's `StructurePiece`. Its `save` is
/// the engine's (`StructurePiece.createTag`: the type id, `BB`, `O`, `GD`)
/// around [`add_additional_save_data`](StructurePiece::add_additional_save_data).
pub trait StructurePiece: std::fmt::Debug + Send + Sync {
    /// `StructurePiece.getBoundingBox`: the piece's box, in world blocks.
    fn bounding_box(&self) -> BoundingBox;
    /// Override terrain shaping, even when the structure's adjustment is `None`.
    /// Queried once at generation or load, never per chunk. An answer whose adjustment
    /// is `None` omits this piece; no answer preserves the structure's default.
    fn beardifier(&self) -> Option<PieceBeardifier> {
        None
    }
    /// `StructurePiece.postProcess`: place the piece's blocks and entities
    /// that fall in the chunk `context` clips to. Writes outside it are
    /// dropped silently, as vanilla's `placeBlock` skips them.
    fn post_process(&self, context: &mut dyn PieceContext) -> Result<(), StructureError>;
    /// `StructurePiece.addAdditionalSaveData`: the piece's own fields, saved
    /// beside `BB`/`O`/`GD` and read back by the type's `load_piece`.
    fn add_additional_save_data(&self, tag: &mut Compound);
}

/// A noise-column block, classified by the questions structures ask.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
#[derive(sunrise_mod_api_macros::Wire)]
pub enum NoiseBlock {
    /// An air block.
    Air,
    /// Water, including water supplied by an aquifer.
    Water,
    /// Lava.
    Lava,
    /// A noise block whose state blocks motion.
    Solid,
    /// Any other noise-column state.
    Other,
}
impl NoiseBlock {
    /// Whether this class is solid, not merely non-air.
    pub fn is_solid(self) -> bool {
        self == Self::Solid
    }
}

/// A noise column from its minimum Y upward; reads outside it are air.
#[derive(Clone, Debug, Default)]
#[non_exhaustive]
#[derive(sunrise_mod_api_macros::Wire)]
pub struct NoiseColumn {
    /// The minimum Y represented by the first block.
    pub min: i32,
    /// Classified blocks in increasing Y order.
    pub blocks: Vec<NoiseBlock>,
}
impl NoiseColumn {
    /// Construct a column from its minimum Y and classified blocks.
    pub fn new(min: i32, blocks: Vec<NoiseBlock>) -> Self {
        Self { min, blocks }
    }
    /// Read a class, returning air outside the represented range.
    pub fn get(&self, y: i32) -> NoiseBlock {
        usize::try_from(i64::from(y) - i64::from(self.min))
            .ok()
            .and_then(|index| self.blocks.get(index).copied())
            .unwrap_or(NoiseBlock::Air)
    }
}

/// The vanilla heightmap opacity predicates used by structure queries.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
#[derive(sunrise_mod_api_macros::Wire)]
pub enum HeightmapType {
    /// Highest non-air block during world generation.
    WorldSurfaceWg,
    /// Highest non-air block.
    WorldSurface,
    /// Highest motion-blocking block during world generation.
    OceanFloorWg,
    /// Highest motion-blocking block.
    OceanFloor,
    /// Highest motion-blocking block or fluid.
    MotionBlocking,
    /// Motion-blocking block or fluid, ignoring leaves.
    MotionBlockingNoLeaves,
}

/// An opaque, process-local biome identity. Do not persist it or perform arithmetic on it.
#[derive(
    sunrise_mod_api_macros::Wire, Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash,
)]
#[non_exhaustive]
pub struct BiomeId(pub(crate) u32);

/// Construct the game's biome identity at the hidden integration boundary.
// hidden: only the game's adapter builds a biome identity; mods receive one from the context.
#[doc(hidden)]
pub fn biome_id(index: u32) -> BiomeId {
    BiomeId(index)
}

/// Construct the layout the game's adapter answers `assemble_jigsaw` with.
// hidden: only the game's adapter builds a layout; mods receive one from the context.
#[doc(hidden)]
pub fn jigsaw_layout(position: [i32; 3], pieces: Vec<BoundingBox>) -> JigsawLayout {
    JigsawLayout { position, pieces }
}

/// The start context a type's
/// [`find_generation_point`](StructureType::find_generation_point) sees:
/// vanilla's `GenerationContext` over one chunk, on the worker thread;
/// deterministic for the seed.
pub trait GenerationContext {
    /// Immutable, load-owned block metadata, safe to borrow on this worker.
    fn metadata(&self) -> &dyn BlockMetadata;
    /// The chunk the start would be in.
    fn chunk(&self) -> ChunkPos;
    /// The world's seed.
    fn seed(&self) -> i64;
    /// The level's sea level.
    fn sea_level(&self) -> i32;
    /// The level's minimum block Y.
    fn min_y(&self) -> i32;
    /// The level's maximum block Y.
    fn max_y(&self) -> i32;
    /// The random vanilla's types draw from, seeded for this chunk
    /// (`WorldgenRandom.setLargeFeatureSeed` of the seed, chunk X and Z).
    fn random(&mut self) -> &mut dyn RandomSource;
    /// `ChunkGenerator.getBaseHeight`: the first free height at (x, z) by the
    /// heightmap kind.
    fn base_height(&mut self, x: i32, z: i32, kind: HeightmapType) -> i32;
    /// `ChunkGenerator.getBaseColumn` as heightmap opacity, from `min_y` up,
    /// for several kinds from one noise-column pass.
    fn base_columns(&mut self, x: i32, z: i32, kinds: &[HeightmapType]) -> Vec<Vec<bool>>;
    /// `ChunkGenerator.getBaseColumn`'s blocks: the noise column at (x, z).
    fn noise_column(&mut self, x: i32, z: i32) -> NoiseColumn;
    /// Noise biome at a quart position (the resolver's, no zoom).
    fn noise_biome(&mut self, quart: [i32; 3]) -> BiomeId;
    /// `GenerationContext.validBiome` over inclusive block Y bounds of the
    /// column at (x, z): whether any biome the structure names occurs there.
    fn column_has_valid_biome(&mut self, x: i32, z: i32, min_y: i32, max_y: i32) -> bool;
    /// The structure's NBT config, converted once when its definition loads.
    fn config(&self) -> Arc<Tag>;
    /// `JigsawPlacement.addPieces` from the data pack's template pools, at
    /// `position` with `settings`, drawing from [`random`](Self::random), as a
    /// NeoForge structure type calls it from its own `findGenerationPoint`.
    /// The aliases are drawn here (`PoolAliasLookup.create` at `position` with
    /// the world seed). Every placement check of the layout is vanilla's: the
    /// projected start's biome, the padding, each child's free space and the
    /// reach. `None` when no start fits (an empty start pool, a missing start
    /// jigsaw, a start outside the valid biomes or the padding) or when an
    /// alias's weights are invalid.
    ///
    /// The pieces are jigsaw pieces the game keeps, saves and places itself:
    /// they join this start if the type then answers `Some`, ahead of its own.
    /// With `max_depth` 0 or below the layout holds no piece.
    fn assemble_jigsaw(
        &mut self,
        settings: &JigsawSettings,
        position: [i32; 3],
    ) -> Option<JigsawLayout>;
    /// The data pack's template pool `name`, as `Registry.get` of
    /// `TEMPLATE_POOL` finds it: `None` for a pool the data pack lacks or one
    /// left out because an element failed to decode. Aliases are not applied:
    /// a type that uses `pool_aliases` looks the aliased name up itself.
    fn template_pool(&mut self, name: &crate::Identifier) -> Option<TemplatePool>;
    /// Element reads for laying pieces out by hand: any element's jigsaws and
    /// box.
    fn elements(&self) -> &dyn ElementContext;
}

/// The placement context a piece's
/// [`post_process`](StructurePiece::post_process) and a type's
/// [`after_place`](StructureType::after_place) see: the blocks and entities of
/// one chunk, the piece's random and what the blocks around it are made of.
pub trait PieceContext {
    /// Immutable block metadata for this worker's resource snapshot.
    fn metadata(&self) -> &dyn BlockMetadata;
    /// The chunk being placed.
    fn chunk(&self) -> ChunkPos;
    /// The writable box: a `set_block` outside it is dropped silently.
    fn chunk_box(&self) -> BoundingBox;
    /// The piece's random (`RandomSource` of `postProcess`).
    fn random(&mut self) -> &mut dyn RandomSource;
    /// The block's default state; resolve it once when constructing or loading a piece.
    fn state(&self, block: BlockKey) -> BlockState;
    /// A block state with partial properties; missing properties keep defaults.
    /// Resolve once at piece construction or load, never once per placed block.
    fn state_with(&self, block: BlockKey, properties: &[(&str, &str)]) -> BlockState;
    /// `BlockState.isAir`.
    fn is_air(&self, state: BlockState) -> bool;
    /// The block at `pos`, or `None` outside the region.
    fn block(&mut self, pos: [i32; 3]) -> Option<BlockState>;
    /// `StructurePiece.placeBlock`: write `state` at `pos`, answering whether
    /// it was written. A position outside `chunk_box` is skipped silently, as
    /// vanilla's `placeBlock` skips it.
    fn set_block(&mut self, pos: [i32; 3], state: BlockState) -> bool;
    /// `WorldGenLevel.getHeight` of the heightmap kind at (x, z).
    fn height(&mut self, kind: HeightmapType, x: i32, z: i32) -> Option<i32>;
    /// `BlockState.isSolidRender` of a state: whether the block draws as a
    /// solid block, the way `StructurePiece.reorient` asks of its neighbours.
    fn solid_render(&self, state: BlockState) -> bool;
    /// `WorldGenLevel.addFreshEntity`: an entity from its saved NBT — its
    /// `id`, `Pos` and fields — which the generating chunk holds and the game
    /// finishes as it does a structure mob's when the chunk joins the world
    /// (`Mob.finalizeSpawn` with `STRUCTURE`; keep the
    /// `"sunrise:finalize"` byte a structure template's entities carry to ask
    /// for it).
    fn entity(&mut self, nbt: Compound);
    /// `WorldGenLevel.setBlockEntity` at `pos`: the block entity's saved NBT
    /// (its `id` and fields).
    fn block_entity(&mut self, pos: [i32; 3], nbt: Compound);
    /// `ConfiguredFeature.place` of the data pack's configured feature
    /// `feature` at `pos`, drawing from [`random`](Self::random), with no
    /// placement modifiers (as `/place feature` places one). Whether it placed anything;
    /// `Ok(false)` also for a feature the data pack lacks. The feature writes
    /// through the level, so its writes are not clipped to `chunk_box`.
    fn place_feature(&mut self, feature: &str, pos: [i32; 3]) -> Result<bool, StructureError>;
    /// `PlacedFeature.place` of the data pack's placed feature
    /// `placed_feature` at `pos`: its placement modifiers, then its feature,
    /// drawing from [`random`](Self::random), as a `FeaturePoolElement`
    /// places its feature. Whether it placed anything;
    /// `Ok(false)` also for a placed feature the data pack lacks.
    fn place_placed_feature(
        &mut self,
        placed_feature: &str,
        pos: [i32; 3],
    ) -> Result<bool, StructureError>;
    /// `StructurePoolElement.place` of any element, vanilla's or a mod's, in
    /// this chunk, drawing from [`random`](Self::random). This is how an
    /// element that wraps another places it. Whether it placed anything.
    fn place_element(
        &mut self,
        element: &PoolElementRef,
        placement: &ElementPlacement,
    ) -> Result<bool, StructureError>;
}

/// A registered API behaviour; the game builds its engine adapter at startup.
pub(crate) struct StructureTypeInfo {
    pub(crate) id: Box<str>,
    pub(crate) ty: Arc<dyn StructureType>,
}

/// Registered structure behaviours, for the hidden game integration boundary.
// hidden: the game's startup adapter reads the registered behaviours; mods declare them through the registrar.
#[doc(hidden)]
pub fn structure_type_declarations() -> Vec<(String, Arc<dyn StructureType>)> {
    crate::parts::installed_structure_type_list()
        .iter()
        .map(|info| (info.id.to_string(), info.ty.clone()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn noise_column_clips_outside_y_without_overflow() {
        let column = NoiseColumn::new(-2, vec![NoiseBlock::Water, NoiseBlock::Solid]);
        assert_eq!(column.get(-3), NoiseBlock::Air);
        assert_eq!(column.get(-2), NoiseBlock::Water);
        assert_eq!(column.get(-1), NoiseBlock::Solid);
        assert_eq!(column.get(0), NoiseBlock::Air);
        let extreme = NoiseColumn::new(i32::MAX, vec![NoiseBlock::Lava]);
        assert_eq!(extreme.get(i32::MAX), NoiseBlock::Lava);
        assert_eq!(extreme.get(i32::MIN), NoiseBlock::Air);
    }
}
