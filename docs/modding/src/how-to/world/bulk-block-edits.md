# Typed bulk block edits

Use complete `BlockState` values in `BlockEdit.palette`. Construct them with
`BlockState::of(&host.block_states(), block, constraints)` (or change an existing state
with `with`). `filters` contains owned `StatePredicate` values. `StatePredicate::block`
validates property names and values strictly; `StatePredicate::tag` retains arbitrary
property requirements, including for an empty tag, and rejects duplicate properties.

A predicate requires block identity or tag membership and every supplied property.
Omitted properties are wildcards, never default-state requirements. An unknown or
missing property on a tag member never matches. Sparse, non-Cartesian state tables
are supported: a partial predicate does not need to form a complete state with defaults.

All palette entries, predicates and run indices are prepared before any mutation.
NBT is not representable. At most 65,536 filters are allowed. Unloaded chunks and
out-of-height portions of runs are skipped. Edits preserve strict block-entity behavior,
drop no contents from replaced block entities, notify no neighbors and send whole changed chunks.
Inside a transaction in the level in place, the new states and entities are visible immediately.
Abort quietly restores both, along with the connection group each replaced block had. Only the
outermost commit in that level publishes the changed chunks, with their resulting connection groups.
When a bulk edit and a `LevelRef::set_block` write touch the same position, they replay in
call order. Only the ordinary write can spill contents or notify neighbors.

## Read column palettes

`ColumnBlocks.palette` also contains complete `BlockState` values. States of the
same block with different properties have distinct entries. Only distinct numeric
states are converted to API values, not each block in the returned columns.

`ColumnBlocks.water_colors` carries one `0xRRGGBB` server-side biome JSON water
colour (`BiomeSpecialEffects.waterColor`) per column at the column's top (`0` where
unloaded). Colormap-derived grass and foliage tints are client-side only. See
[Read blended biome tints on the client](read-biome-data.md#read-blended-biome-tints-on-the-client)
for `ClientHost::biome_tint(pos, resolver)` and the equivalent read inside a block tint callback.

## Query terrain before loading chunks

`ServerHost::terrain_surface(dimension, columns)` reads the level's generator, not resident
chunks. It returns one optional `TerrainColumn` per input pair `(x, z)`, in input order.
At most `MAX_TERRAIN_COLUMNS` (4,096) columns are accepted per call. Use the hook's live server host.

- `height` is one above the highest solid block, skipping fluids, like OceanFloor. The scan
  starts at the level top, so the Nether returns its bedrock roof, not a floor.
- `block` is that surface's `BlockKey`; `water` is one above the highest fluid over it,
  or absent on dry land. `biome` names the biome in the world's table, including data pack biomes.
- Void columns and worlds without their own generator return `None`. Custom generators that
  do not provide a noise surface also have no answer.
- No chunk is created, loaded or held. Features (trees, ores, lakes and decorations) and slope
  rules are not included: even an undecorated chunk may have a different top block. A teleport
  mod can sample candidate columns, then hold only its chosen chunk and verify the decorated blocks.
- A client, a dimension other than the level being ticked, or an over-limit request returns
  error text before reading anything. Results depend on generator inputs, not loaded chunks or
  query order. The game makes one noise sample per column on the server tick, and logs the
  column count for calls exceeding the configured slow-call threshold.

Mods only receive `TerrainColumn`; it is non-exhaustive and constructed by the game.

#### Upgrading one saved compound

When a mod imports a schematic, quest book or other saved data, pass the compound and the
`DataVersion` of its source to `host.upgrade_nbt(kind, data_version, nbt)`. This runs the game's
own data fixers through the current data version. The method is available on both sides and
needs no world. It does not stamp a `DataVersion` or run fixes for an enclosing chunk or file.

Choose an `NbtDataType` for what the compound represents: `ItemStack`, `BlockState`,
`BlockEntity`, `Entity` or `EntityTree`. For example, an importer can upgrade a saved stack
with `host.upgrade_nbt(NbtDataType::ItemStack, source_version, stack_nbt)?` before decoding it
with `ItemStack::from_nbt`. For entities, choose `EntityTree` if the root's riders or
passengers should also be upgraded; `Entity` does not fix the root's riding tree. Vanilla's
standalone entity and block-entity fixers can retain old root keys alongside the upgraded fields.

Unknown mod IDs are preserved, though generic format changes like the item-stack count and
components still apply. A well-formed compound from the current game version is unchanged.
Data older than the first schema is read with that schema, as in vanilla. Pass `-1` for
unversioned data.

`NbtUpgradeError` distinguishes a future source version, a malformed compound (for example,
one missing its `id`, called `Name` on block states before DataVersion 5006), and legacy
tables that a fix needed but could not read. `TooDeep` and `UpgradedTooDeep` distinguish
input and output past vanilla's NBT read depth. This counts the root at level zero and includes
wrapper compounds when a mixed list is encoded. The upgrade has no size or node cap.
