//! Times a redstone wire line through the real block-behaviour path.
//!
//! Builds `LINES` parallel wires of `LENGTH` cells on stone, toggles each
//! line's lever `TOGGLES` times, and runs whole level ticks until quiet. This
//! is the `Sim` path the game uses, over a map-backed level, so it measures
//! the redstone work itself rather than chunk storage.
//!
//! `SUNRISE_RESOURCES` selects the prepared resources (default:
//! `.local/sunrise-client/<version>`), the same as the parity probes.

use std::{collections::HashMap, path::PathBuf, time::Instant};
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
const LINES: i32 = 4;
const TOGGLES: i32 = 40;

struct Cells {
    cells: HashMap<BlockPos, BlockStateId>,
    air: BlockStateId,
    random: LegacyRandom,
    time: i64,
}

impl BlockEntityNeighbors for Cells {}
impl LevelAccess for Cells {
    fn block_state(&self, pos: BlockPos) -> BlockStateId {
        self.cells.get(&pos).copied().unwrap_or(self.air)
    }
    fn set_raw(&mut self, pos: BlockPos, state: BlockStateId) -> Option<BlockStateId> {
        let old = if state == self.air {
            self.cells.remove(&pos)
        } else {
            self.cells.insert(pos, state)
        };
        Some(old.unwrap_or(self.air))
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

    let mut level = Cells {
        cells: HashMap::new(),
        air,
        random: LegacyRandom::new(0),
        time: 0,
    };
    let mut ticks = Ticks::new(0);
    let mut levers = Vec::new();
    for line in 0..LINES {
        let z = line * 2;
        let lever_pos = BlockPos::new(-1, 64, z);
        level.cells.insert(lever_pos, lever);
        levers.push(lever_pos);
        for x in 0..LENGTH {
            level.cells.insert(BlockPos::new(x, 63, z), stone);
            level.cells.insert(BlockPos::new(x, 64, z), wire);
        }
    }

    // Warm up: the first toggle also gives every wire its connection shape.
    for &at in &levers {
        flip(&behaviours, &mut level, &mut ticks, at);
    }
    for _ in &levers {
        run(&behaviours, &mut level, &mut ticks);
    }
    let warmup = level.time;

    let started = Instant::now();
    let mut ticks_run = 0u32;
    for _ in 0..TOGGLES {
        for &at in &levers {
            flip(&behaviours, &mut level, &mut ticks, at);
            ticks_run += run(&behaviours, &mut level, &mut ticks);
        }
    }
    let elapsed = started.elapsed();
    let toggles = TOGGLES * levers.len() as i32;
    println!(
        "{LINES} lines x {LENGTH} wire: {toggles} toggles in {elapsed:.3?} ({:.1} us/toggle), \
         {ticks_run} follow-up ticks (wire cascades run inline; warmup {warmup})",
        elapsed.as_secs_f64() * 1e6 / f64::from(toggles),
    );
    Ok(())
}
