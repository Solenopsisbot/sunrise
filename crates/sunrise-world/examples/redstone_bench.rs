//! Times a redstone wire line through the real block-behaviour path.
//!
//! Builds `LINES` parallel wires of `LENGTH` cells on stone, toggles each
//! line's lever `TOGGLES` times, and runs whole level ticks until quiet. This
//! is the `Sim` path the game uses, over a map-backed level, so it measures
//! the redstone work itself rather than chunk storage.
//!
//! `SUNRISE_RESOURCES` selects the prepared resources (default:
//! `.local/sunrise-client/<version>`), the same as the parity probes.

use std::{cell::Cell, path::PathBuf, time::Instant};
use sunrise_core::{
    geometry::{BlockHit, Direction, Vec3},
    position::BlockPos,
    random::LegacyRandom,
};
use sunrise_world::{
    level::{
        Behaviours, BlockEntityNeighbors, LevelAccess, PlayerContext, Ticks, data::load_registry,
    },
    registry::BlockStateId,
};

const LENGTH: i32 = 128;
const PLATE: i32 = 16;
const LINES: i32 = 4;
const TOGGLES: i32 = 40;
/// Rounds of `TOGGLES`; the run reports each and the best, because this
/// machine's timings drift by tens of percent between runs.
const ROUNDS: usize = 5;

/// A dense box of block states: the benchmark's world is a 130-cell line, so
/// an array read is what the game's chunk read costs. A `HashMap` here would
/// make the run measure hash overhead instead of redstone.
struct Cells {
    states: Vec<BlockStateId>,
    origin: (i32, i32, i32),
    size: (i32, i32, i32),
    air: BlockStateId,
    random: LegacyRandom,
    time: i64,
    /// Level traffic, so the run can say what the cascade costs in reads and
    /// writes rather than only in time.
    reads: Cell<u64>,
    writes: Cell<u64>,
}

/// Room for the lines, their stone, and the probes redstone makes around them.
const BOX: (i32, i32, i32) = (160, 24, 32);

impl Cells {
    fn new(air: BlockStateId) -> Self {
        let (sx, sy, sz) = BOX;
        Self {
            states: vec![air; (sx * sy * sz) as usize],
            origin: (-16, 54, -8),
            size: BOX,
            air,
            random: LegacyRandom::new(0),
            time: 0,
            reads: Cell::new(0),
            writes: Cell::new(0),
        }
    }

    fn slot(&self, pos: BlockPos) -> Option<usize> {
        let (ox, oy, oz) = self.origin;
        let (sx, sy, sz) = self.size;
        let (x, y, z) = (pos.x - ox, pos.y - oy, pos.z - oz);
        ((0..sx).contains(&x) && (0..sy).contains(&y) && (0..sz).contains(&z))
            .then(|| ((y * sz + z) * sx + x) as usize)
    }

    fn set(&mut self, pos: BlockPos, state: BlockStateId) {
        let slot = self
            .slot(pos)
            .unwrap_or_else(|| panic!("write outside the benchmark box at {pos:?}"));
        self.states[slot] = state;
    }
}

impl BlockEntityNeighbors for Cells {}
impl LevelAccess for Cells {
    fn block_state(&self, pos: BlockPos) -> BlockStateId {
        self.reads.set(self.reads.get() + 1);
        self.slot(pos).map_or(self.air, |slot| self.states[slot])
    }
    fn set_raw(&mut self, pos: BlockPos, state: BlockStateId) -> Option<BlockStateId> {
        self.writes.set(self.writes.get() + 1);
        let slot = self.slot(pos)?;
        let old = self.states[slot];
        self.states[slot] = state;
        Some(old)
    }
    fn is_loaded(&self, _pos: BlockPos) -> bool {
        true
    }
    fn random(&mut self) -> &mut LegacyRandom {
        &mut self.random
    }
    fn game_time(&self) -> i64 {
        self.time
    }
    fn block_entity_analog_signal(
        &mut self,
        _pos: BlockPos,
        _state: BlockStateId,
        _direction: Direction,
    ) -> i32 {
        0
    }
}

fn flip(behaviours: &Behaviours, level: &mut Cells, ticks: &mut Ticks, at: BlockPos) {
    let state = level.block_state(at);
    let _ = behaviours.use_without_item(
        level,
        ticks,
        state,
        BlockHit {
            block: at,
            direction: Direction::Up,
            location: Vec3::new(at.x as f64 + 0.5, at.y as f64 + 0.5, at.z as f64 + 0.5),
            inside: false,
        },
        PlayerContext {
            yaw: 0.0,
            pitch: 0.0,
            may_build: true,
            can_eat: false,
            game_master: false,
            respawn_point: None,
        },
    );
}

/// Whole level ticks until quiet, capped.
fn run(behaviours: &Behaviours, level: &mut Cells, ticks: &mut Ticks) -> u32 {
    let mut t = 0;
    while t < 64
        && (ticks.blocks.count() != 0
            || ticks.fluids.count() != 0
            || !ticks.block_events.is_empty()
            || !ticks.moving_pistons.is_empty())
    {
        t += 1;
        level.time += 1;
        behaviours.tick_scheduled(level, ticks);
        behaviours.run_block_events(level, ticks);
        behaviours.tick_block_entities(level, ticks);
    }
    t
}

/// Toggle every lever `TOGGLES` times, round by round, and report the best
/// round plus the level traffic per toggle.
fn measure(
    name: &str,
    behaviours: &Behaviours,
    level: &mut Cells,
    ticks: &mut Ticks,
    levers: &[BlockPos],
) {
    let mut rounds = Vec::new();
    let mut reads = 0u64;
    let mut writes = 0u64;
    let per_round = TOGGLES as usize * levers.len();
    for _ in 0..ROUNDS {
        level.reads.set(0);
        level.writes.set(0);
        let started = Instant::now();
        for _ in 0..TOGGLES {
            for &at in levers {
                flip(behaviours, level, ticks, at);
                run(behaviours, level, ticks);
            }
        }
        rounds.push(started.elapsed());
        reads += level.reads.get();
        writes += level.writes.get();
    }
    let best = rounds.iter().min().copied().unwrap_or_default();
    let per_toggle = best.as_secs_f64() * 1e6 / per_round as f64;
    println!(
        "{name}: best round {best:.3?} ({per_toggle:.1} us/toggle), {:.0} reads, \
         {:.1} writes per toggle",
        reads as f64 / (per_round * ROUNDS) as f64,
        writes as f64 / (per_round * ROUNDS) as f64,
    );
    println!("  rounds: {rounds:.3?}");
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let resources = std::env::var_os("SUNRISE_RESOURCES")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../../.local/sunrise-client")
                .join(sunrise_core::VANILLA_VERSION)
        });
    let registry = load_registry(&resources)?;
    let behaviours = Behaviours::load(&registry, &resources)?;
    let state = |name: &str| {
        registry
            .default_state(name)
            .unwrap_or_else(|| panic!("registry has no {name}"))
    };
    let (air, stone, wire, lever) = (
        state("minecraft:air"),
        state("minecraft:stone"),
        state("minecraft:redstone_wire"),
        state("minecraft:lever"),
    );

    // A line: two sources per wire, the shape `getBestNeighborSignal` and
    // `getIncomingSignal` handle most cheaply.
    let mut level = Cells::new(air);
    let mut ticks = Ticks::new(0);
    let mut levers = Vec::new();
    for line in 0..LINES {
        let z = line * 2;
        let lever_pos = BlockPos::new(-1, 64, z);
        level.set(lever_pos, lever);
        levers.push(lever_pos);
        for x in 0..LENGTH {
            level.set(BlockPos::new(x, 63, z), stone);
            level.set(BlockPos::new(x, 64, z), wire);
        }
    }
    // Warm up: the first toggle also gives every wire its connection shape.
    for &at in &levers {
        flip(&behaviours, &mut level, &mut ticks, at);
        run(&behaviours, &mut level, &mut ticks);
    }
    measure(
        &format!("{LINES} lines x {LENGTH} wire"),
        &behaviours,
        &mut level,
        &mut ticks,
        &levers,
    );

    // A field: up to eight sources per wire, and every one of them probed from
    // every direction. This is the shape a redstone computer's memory is, and
    // the one the per-wire walks are quadratic in.
    let mut level = Cells::new(air);
    let mut ticks = Ticks::new(0);
    let lever_pos = BlockPos::new(-1, 64, 0);
    level.set(lever_pos, lever);
    for x in 0..PLATE {
        for z in 0..PLATE {
            level.set(BlockPos::new(x, 63, z), stone);
            level.set(BlockPos::new(x, 64, z), wire);
        }
    }
    let levers = [lever_pos];
    for &at in &levers {
        flip(&behaviours, &mut level, &mut ticks, at);
        run(&behaviours, &mut level, &mut ticks);
    }
    measure(
        &format!("{PLATE} x {PLATE} wire field"),
        &behaviours,
        &mut level,
        &mut ticks,
        &levers,
    );
    Ok(())
}
