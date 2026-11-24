//! Sounds and particles in a level for native server mods, through a
//! `LevelRef` (docs/modding/src/how-to/native-mods.md, "Entity and level handles", #7387).
//! Every player in range hears or sees them, as `ServerLevel.playSound` and
//! `sendParticles` send them. They return false on a client, for a handle of
//! an earlier open, or for arguments the network can't carry.
use super::journal_level_write::PendingWrite;
use super::native_hooks::{mod_particles_ok, mod_sound_ok};
use super::{Game, creatures::NativeSound, lan_particles::QueuedParticle};
use mod_api::{LevelRef, PlayerRef, PlayerWorldRefusal, SoundSource};
use sunrise_client::particles::ParticleOptions;
use sunrise_world::registry::BlockStateId;

/// Validated inputs for queueing or deferring a level or entity-bound native sound.
pub(super) struct NativeSoundRequest<'a> {
    pub(super) level: LevelRef,
    pub(super) sound: &'a str,
    pub(super) at: [f64; 3],
    pub(super) source: SoundSource,
    pub(super) volume: f32,
    pub(super) pitch: f32,
    pub(super) except: Option<u32>,
    pub(super) entity: Option<i32>,
    /// The caller's seed (`ServerLevel.playSeededSound`), or `None` to draw the next one from
    /// the level's sound-seed stream when the sound is published (`Level.playSound`).
    pub(super) seed: Option<i64>,
}

impl Game<'_> {
    /// The level's dimension id, for a handle of this open on the server.
    fn native_level_dimension(&self, level: LevelRef) -> Option<&'static str> {
        self.native_level_world(level)?;
        Some(level.dimension_id())
    }
    /// Plays sound event `sound` at `at` for the players in this level
    /// within range (16 blocks, times the volume above 1), as
    /// `ServerLevel.playSound(except, ...)` does, under `source`'s volume
    /// slider, leaving out `except`'s client (a fake player has none, so it
    /// leaves out no one; a stale one queues nothing). A `minecraft:` event
    /// must be registered; a mod's goes by name and is heard where its
    /// resources define it. True means queued, not heard.
    pub(super) fn level_play_sound(
        &mut self,
        level: LevelRef,
        sound: &str,
        at: [f64; 3],
        source: SoundSource,
        volume: f32,
        pitch: f32,
        except: Option<PlayerRef>,
    ) -> bool {
        self.native_level_sound(level, sound, at, source, volume, pitch, except, None)
    }
    /// [`Self::level_play_sound`] with the caller's seed (`ServerLevel.playSeededSound`): the
    /// level's sound-seed stream does not move.
    pub(super) fn level_play_seeded_sound(
        &mut self,
        level: LevelRef,
        sound: &str,
        at: [f64; 3],
        source: SoundSource,
        volume: f32,
        pitch: f32,
        except: Option<PlayerRef>,
        seed: i64,
    ) -> bool {
        self.native_level_sound(level, sound, at, source, volume, pitch, except, Some(seed))
    }
    /// The next seed of the stream the level's sounds draw from
    /// (`Level.soundSeedGenerator.nextLong()`), drawn now without playing a sound: the same
    /// draw a published [`Self::level_play_sound`] makes. None where that call would refuse
    /// the level or send nothing.
    pub(super) fn level_next_sound_seed(&mut self, level: LevelRef) -> Option<i64> {
        self.native_level_dimension(level)?;
        Some(self.lan.as_ref()?.hub.next_sound_seed())
    }
    /// The connection a level sound leaves out for `except` (`Level.playSound(except, ...)`):
    /// `Some(None)` for no one, and for a fake player, who has no client to leave out; `None`
    /// refuses the sound for a stale or client-side player.
    pub(super) fn native_sound_except(&self, except: Option<PlayerRef>) -> Option<Option<u32>> {
        let Some(player) = except else {
            return Some(None);
        };
        match self.player_connection(player) {
            Ok(conn) => Some(Some(conn)),
            Err(PlayerWorldRefusal::NoConnection) => Some(None),
            Err(_) => None,
        }
    }
    /// A positional level sound leaving out `except`, with `seed` as [`NativeSoundRequest`] says.
    fn native_level_sound(
        &mut self,
        level: LevelRef,
        sound: &str,
        at: [f64; 3],
        source: SoundSource,
        volume: f32,
        pitch: f32,
        except: Option<PlayerRef>,
        seed: Option<i64>,
    ) -> bool {
        let Some(except) = self.native_sound_except(except) else {
            return false;
        };
        self.native_queue_sound(NativeSoundRequest {
            level,
            sound,
            at,
            source,
            volume,
            pitch,
            except,
            entity: None,
            seed,
        })
    }
    /// Queue or defer a validated level or entity-bound sound (`Level.playSound`), leaving out
    /// `except` and optionally binding to `entity`. Inside an open transaction the sound waits in
    /// `PendingWrite::Sound` until root commit (drawing its seed on publish unless the request
    /// gave one) and discards on abort.
    pub(super) fn native_queue_sound(&mut self, request: NativeSoundRequest<'_>) -> bool {
        let NativeSoundRequest {
            level,
            sound,
            at,
            source,
            volume,
            pitch,
            except,
            entity,
            seed,
        } = request;
        let Some(dimension) = self.native_level_dimension(level) else {
            return false;
        };
        if self.lan.is_none() || !mod_sound_ok(sound, at, source.index(), volume, pitch) {
            return false;
        }
        self.emit_level_effect(PendingWrite::Sound {
            sound: NativeSound {
                dimension,
                sound: sound.to_owned(),
                at,
                source: source.index() as i32,
                volume,
                pitch,
                seed: 0,
                except,
                entity,
                skip_hooks: false,
            },
            seed,
        });
        true
    }
    /// Sends `count` particles around `at` to the players in this level
    /// within 32 blocks, as `ServerLevel.sendParticles` does: each offset is
    /// a Gaussian times `spread`, and `speed` scales their motion (a zero
    /// count sends one particle moving along `spread`). Block particle types
    /// need `state`; item particles need a validated stack; trail and vibration aren't supported.
    /// True means queued, not drawn.
    pub(super) fn level_send_particles(
        &mut self,
        level: LevelRef,
        options: &ParticleOptions,
        state: Option<BlockStateId>,
        at: [f64; 3],
        spread: [f64; 3],
        speed: f64,
        count: i32,
    ) -> bool {
        let Some(dimension) = self.native_level_dimension(level) else {
            return false;
        };
        let registry = self.pack.assets.registry;
        let state_ok = |id| registry.state(id).is_some();
        if !mod_particles_ok(options, state, state_ok, at, spread, speed) {
            return false;
        }
        self.emit_level_effect(PendingWrite::Particles(QueuedParticle {
            dimension,
            options: options.clone(),
            state,
            at,
            spread,
            speed: [speed; 3],
            count,
            randomization: 0,
        }));
        true
    }
}
