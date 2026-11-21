//! Server-side `menu_slot_changed`: vanilla `Container.setItem` / `AbstractContainerMenu.slotsChanged`
//! for mod menu types.
//!
//! Routes every authoritative input change on an open mod menu through one unified path:
//! - Clicks (default clicks on blockless owned or block-backed mod menus, `menu_quick_move`, and
//!   custom `menu_clicked` overrides returning `Flow::Cancel`), running inside the click savepoint
//!   so a listener panic or invalid `MenuSlotWrite` rolls back the entire click.
//! - Transactional host writes (`ItemPlace::MenuSlot`, `ItemPlace::BlockEntity`,
//!   `BlockEntityRef::set_item_slot`, transactional `BlockEntityRef::merge_data` / `replace_data`),
//!   deduplicated once per outermost committed transaction and never firing on abort.
//! - Non-transactional external container writes to a block-backed mod menu (hoppers,
//!   `/item replace block`, non-transactional `merge_data`), delivered immediately with all open
//!   viewers synced.
//!
//! Multiple non-result slot changes within one click step (such as a multi-slot drag) or one
//! transaction are batched into a single `menu_slot_changed` call in ascending menu slot order.
//! Result-slot changes and `MenuSlotWrite` outputs never re-fire `menu_slot_changed`.
use super::api_values::{TryIntoApi, TryIntoEngine};
use super::{Game, transactions::After};
use mod_api::{MenuContainer, MenuSlotChange, MenuSlotContext, MenuSlotWrite};
use sunrise_client::{
    block_entities::Pos,
    containers::{BlockEntity, ModMenuType},
    items::{ItemStack, Items},
    lan::menu::Slots,
    menu::Menu,
};

/// Maximum stack size for container slots without an explicit per-slot owned limit
/// (`Container.DEFAULT_MAX_STACK_SIZE`).
const CONTAINER_MAX: u32 = 99;

/// Open mod menu whose `menu_slot_changed` dispatch is currently on the call stack.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::game) enum SlotChangedTarget {
    Owned(u64),
    Block(Pos),
}

struct SlotChangedGuard<'a, 'g> {
    game: &'a mut Game<'g>,
}

impl Drop for SlotChangedGuard<'_, '_> {
    fn drop(&mut self) {
        self.game.slot_changed_active.pop();
    }
}

/// Collect non-result owned container slot changes between `before` and `after`, in ascending menu
/// slot order. Result slots, hidden slots and linked slots never trigger `menu_slot_changed`.
pub(super) fn collect_slot_changes(
    layout: &ModMenuType,
    hidden: &[usize],
    before: &[Option<ItemStack>],
    after: &[Option<ItemStack>],
) -> Vec<MenuSlotChange> {
    let mut changes = Vec::new();
    for slot in 0..layout.slots {
        if !layout.owns_slot(slot)
            || hidden.contains(&slot)
            || layout.results.contains(&slot)
            || layout.crafting.is_some_and(|c| c.result == slot)
        {
            continue;
        }
        let old = before.get(slot).and_then(Option::as_ref);
        let new = after.get(slot).and_then(Option::as_ref);
        if old == new {
            continue;
        }
        let (container, index) = match layout.crafting {
            Some(c) if c.grid_slots().contains(&slot) => (MenuContainer::Grid, slot - c.grid),
            _ => (MenuContainer::Main, slot),
        };
        let old_api = old.and_then(|stack| stack.try_into_api().ok());
        let new_api = new.and_then(|stack| stack.try_into_api().ok());
        if old.is_some() && old_api.is_none() || new.is_some() && new_api.is_none() {
            continue;
        }
        changes.push(MenuSlotChange::new(container, index, old_api, new_api));
    }
    changes
}

/// Build a read-only [`MenuSlotContext`] from a `Menu`'s own containers.
pub(super) fn context_from_menu(
    menu: &Menu,
    player: u128,
    pos: Option<[i32; 3]>,
) -> MenuSlotContext {
    let copy = |slots: &[Option<ItemStack>]| -> Vec<Option<mod_api::ItemStack>> {
        slots
            .iter()
            .map(|slot| slot.as_ref().and_then(|stack| stack.try_into_api().ok()))
            .collect()
    };
    let (main, grid, result) = menu.slot_containers();
    MenuSlotContext::new(
        Some(player),
        pos,
        vec![copy(main), copy(grid), copy(result)],
    )
}

/// Why [`validate_slot_writes`] refused a requested [`MenuSlotWrite`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SlotWriteRefusal {
    NonMainContainer,
    UnavailableSlot,
    CraftingSlot,
    OwnedSlotBounds,
    NonResultBlockSlot,
    InvalidStack,
    StackLimitExceeded,
}

impl SlotWriteRefusal {
    const fn reason(self) -> &'static str {
        match self {
            Self::NonMainContainer => "output write must target MenuContainer::Main",
            Self::UnavailableSlot => "output write targets an undeclared, linked or hidden slot",
            Self::CraftingSlot => "output write cannot target a crafting slot",
            Self::OwnedSlotBounds => "output write is outside owned_slots",
            Self::NonResultBlockSlot => {
                "block-backed output write must target a declared result slot"
            }
            Self::InvalidStack => "output write carries an invalid stack",
            Self::StackLimitExceeded => "output write exceeds the slot or item stack limit",
        }
    }
}

/// Validate every requested [`MenuSlotWrite`] before applying any of them.
///
/// On a blockless owned menu (`owned_limits.is_some()`), any declared non-hidden owned `Main` slot
/// may be written up to its declared slot limit and item limit. On a block-backed menu
/// (`owned_limits.is_none()`), only declared non-hidden `results` slots in `Main` may be written.
fn validate_slot_writes(
    items: &Items,
    max_stack: impl Fn(&ItemStack) -> u32,
    layout: &ModMenuType,
    hidden: &[usize],
    owned_limits: Option<&[u8]>,
    block_limit: Option<u32>,
    outputs: Vec<MenuSlotWrite>,
) -> Result<Vec<(usize, Option<ItemStack>)>, SlotWriteRefusal> {
    let mut validated = Vec::with_capacity(outputs.len());
    for write in outputs {
        if write.container != MenuContainer::Main {
            return Err(SlotWriteRefusal::NonMainContainer);
        }
        let slot = write.slot;
        if slot >= layout.slots || !layout.owns_slot(slot) || hidden.contains(&slot) {
            return Err(SlotWriteRefusal::UnavailableSlot);
        }
        if layout
            .crafting
            .is_some_and(|c| c.grid_slots().contains(&slot) || c.result == slot)
        {
            return Err(SlotWriteRefusal::CraftingSlot);
        }
        let slot_cap = match owned_limits {
            Some(limits) => {
                let Some(&limit) = limits.get(slot) else {
                    return Err(SlotWriteRefusal::OwnedSlotBounds);
                };
                u32::from(limit)
            }
            None => {
                if !layout.results.contains(&slot) {
                    return Err(SlotWriteRefusal::NonResultBlockSlot);
                }
                block_limit.unwrap_or(CONTAINER_MAX)
            }
        };
        let stack = match write.stack {
            None => None,
            Some(api_stack) => {
                let engine_stack = api_stack
                    .try_into_engine(items)
                    .map_err(|_| SlotWriteRefusal::InvalidStack)?;
                let limit = slot_cap.min(max_stack(&engine_stack));
                if engine_stack.count == 0 || engine_stack.count > limit {
                    return Err(SlotWriteRefusal::StackLimitExceeded);
                }
                Some(engine_stack)
            }
        };
        validated.push((slot, stack));
    }
    Ok(validated)
}

fn resized_items(items: &[Option<ItemStack>], len: usize) -> Vec<Option<ItemStack>> {
    let mut out = items.to_vec();
    out.resize(len, None);
    out
}

impl Game<'_> {
    /// Build a read-only [`MenuSlotContext`] through [`context_from_menu`] on every path: uses the
    /// live lifted `Menu` during a click, or lifts a read-only `Menu::new(Kind::Mod(layout))`
    /// loaded with the live container items (including linked slots and recomputed crafting
    /// result) so clicks, `menu_clicked` overrides, `menu_quick_move`, `ItemPlace` writes,
    /// `BlockEntityRef` writes, hoppers, `/item replace`, and `merge_data` all present identical
    /// container snapshots to listeners.
    fn slot_changed_context(
        &self,
        conn: u32,
        uuid: u128,
        layout: &'static ModMenuType,
        pos: Option<[i32; 3]>,
        items: &[Option<ItemStack>],
        live_menu: Option<&Menu>,
    ) -> MenuSlotContext {
        if let Some(menu) = live_menu {
            return context_from_menu(menu, uuid, pos);
        }
        let mut full_items = resized_items(items, layout.slots);
        if let Some(l) = layout.links
            && let Some(links) = self
                .lan
                .as_ref()
                .and_then(|host| host.hub.guest(conn))
                .and_then(|g| g.menu.as_ref())
                .map(|m| m.links.as_slice())
        {
            for (k, stack) in l.slots().zip(
                sunrise_client::lan::menu::read_links(&self.world, links, l.count)
                    .into_iter()
                    .chain(std::iter::repeat(None)),
            ) {
                full_items[k] = stack;
            }
        }
        let mut menu = Menu::new(sunrise_client::menu::Kind::Mod(layout));
        let mut env = self.menu_env();
        env.loop_context = &self.loop_context;
        if let Some(host) = self.lan.as_ref()
            && let Some(guest) = host.hub.guest(conn)
        {
            env.player = Some(guest.uuid);
            env.player_entity_id = Some(guest.entity_id);
            env.limited_crafting = host.hub.limited_crafting;
            env.known_recipes = Some(&guest.recipe_book);
        }
        menu.load_container(&env, &full_items);
        context_from_menu(&menu, uuid, pos)
    }

    fn with_slot_changed_guard<R>(
        &mut self,
        target: SlotChangedTarget,
        f: impl FnOnce(&mut Self) -> R,
    ) -> Option<R> {
        if self.slot_changed_active.contains(&target) {
            return None;
        }
        self.slot_changed_active.push(target);
        let guard = SlotChangedGuard { game: self };
        Some(f(guard.game))
    }

    /// Invoke `menu_slot_changed` inside `catch_unwind` and return validated writes, or `None` on
    /// listener panic or invalid write.
    fn fire_validated_slot_changed(
        &mut self,
        layout: &ModMenuType,
        hidden: &[usize],
        owned_limits: Option<&[u8]>,
        block_limit: Option<u32>,
        changed: &[MenuSlotChange],
        context: &MenuSlotContext,
    ) -> Option<Vec<(usize, Option<ItemStack>)>> {
        let menu_id = layout.id;
        let mut outputs = Vec::new();
        let heard = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            self.native_menu_slot_changed(mod_api::__private::MenuSlotChangedCall {
                menu: menu_id,
                changed,
                context,
                outputs: &mut outputs,
            });
        }));
        if heard.is_err() {
            log::error!("menu_slot_changed: a listener failed for {menu_id}; the change is undone");
            return None;
        }
        match validate_slot_writes(
            self.items(),
            |stack| self.containers().max_stack(stack),
            layout,
            hidden,
            owned_limits,
            block_limit,
            outputs,
        ) {
            Ok(writes) => Some(writes),
            Err(refusal) => {
                log::warn!(
                    "menu_slot_changed: refused output write for {menu_id}: {}",
                    refusal.reason()
                );
                None
            }
        }
    }

    /// Fire `menu_slot_changed` under `conn`'s guest turn when not already inside a turn for `conn`.
    #[allow(clippy::too_many_arguments)]
    fn fire_slot_changed_for_guest(
        &mut self,
        conn: u32,
        layout: &ModMenuType,
        hidden: &[usize],
        owned_limits: Option<&[u8]>,
        block_limit: Option<u32>,
        changed: &[MenuSlotChange],
        context: &MenuSlotContext,
    ) -> Option<Vec<(usize, Option<ItemStack>)>> {
        if self.lan_acting == Some(conn) || self.lan_guest_swaps.contains(&conn) {
            self.fire_validated_slot_changed(
                layout,
                hidden,
                owned_limits,
                block_limit,
                changed,
                context,
            )
        } else {
            self.as_guest(conn, |game| {
                game.fire_validated_slot_changed(
                    layout,
                    hidden,
                    owned_limits,
                    block_limit,
                    changed,
                    context,
                )
            })
            .flatten()
        }
    }

    /// Record the last notified snapshot for an open blockless owned menu, journaled when inside
    /// an open transaction so rollback restores the previous baseline.
    fn record_owned_slot_changed_seen(
        &mut self,
        conn: u32,
        generation: u64,
        items: Vec<Option<ItemStack>>,
    ) {
        if !self.transactions.is_empty() {
            self.journal_menu_click(conn);
        }
        if let Some(host) = self.lan.as_mut()
            && let Some(gi) = host.hub.guests.connection_index(conn)
            && host.hub.guests[gi].menu_generation == generation
        {
            host.hub.guests.update(gi, |g| {
                if let Some(open) = g.menu.as_mut() {
                    open.slot_changed_seen = Some(items);
                }
            });
        }
    }

    /// Record the last notified snapshot for every open viewer of a block-backed mod menu at `pos`
    /// in the active dimension, journaled when inside an open transaction.
    fn record_block_slot_changed_seen(&mut self, pos: Pos, items: &[Option<ItemStack>]) {
        let viewers: Vec<(u32, usize)> = self
            .lan
            .as_ref()
            .map(|host| {
                host.hub
                    .guests
                    .block_mod_menu_viewers(pos)
                    .iter()
                    .map(|&gi| &host.hub.guests[gi])
                    .filter(|g| g.placed && host.hub.here(g))
                    .filter_map(|g| {
                        let open = g.menu.as_ref()?;
                        let layout = open.mod_type?;
                        (open.slots == Slots::Block(pos)).then_some((g.conn, layout.slots))
                    })
                    .collect()
            })
            .unwrap_or_default();
        if !self.transactions.is_empty() {
            for &(conn, _) in &viewers {
                self.journal_menu_click(conn);
            }
        }
        let Some(host) = self.lan.as_mut() else {
            return;
        };
        for (conn, slots) in viewers {
            if let Some(gi) = host.hub.guests.connection_index(conn) {
                let seen = resized_items(items, slots);
                host.hub.guests.update(gi, |g| {
                    if let Some(open) = g.menu.as_mut()
                        && open.slots == Slots::Block(pos)
                    {
                        open.slot_changed_seen = Some(seen);
                    }
                });
            }
        }
    }

    /// Sync `conn`'s open container immediately, temporarily swapping the acting guest's live
    /// inventory/cursor back into its hub record if `conn` is currently active in `Game`.
    pub(super) fn sync_guest_container(&mut self, conn: u32) {
        let Some(gi) = self.guest_index(conn) else {
            return;
        };
        let own = self.lan_acting == Some(conn);
        if own && let Some(host) = self.lan.as_mut() {
            host.hub.guests.update(gi, |g| {
                std::mem::swap(&mut self.inventory, &mut g.inventory);
                std::mem::swap(&mut self.armor, &mut g.armor);
                std::mem::swap(&mut self.location_armor, &mut g.location_armor);
                std::mem::swap(&mut self.offhand, &mut g.offhand);
                std::mem::swap(&mut self.equipment, &mut g.equipment);
                std::mem::swap(&mut self.carried, &mut g.carried);
            });
        }
        if let Some(host) = self.lan.as_mut() {
            host.hub.sync_container(conn, &self.world);
        }
        if own && let Some(host) = self.lan.as_mut() {
            host.hub.guests.update(gi, |g| {
                std::mem::swap(&mut self.inventory, &mut g.inventory);
                std::mem::swap(&mut self.armor, &mut g.armor);
                std::mem::swap(&mut self.location_armor, &mut g.location_armor);
                std::mem::swap(&mut self.offhand, &mut g.offhand);
                std::mem::swap(&mut self.equipment, &mut g.equipment);
                std::mem::swap(&mut self.carried, &mut g.carried);
            });
        }
    }

    /// Fire `menu_slot_changed` for a blockless owned menu inside a click transaction if any
    /// non-result container slot changed between `before` and `after`, applying any validated
    /// output writes to `guest.native_menu` and `menu` (when present) without re-firing.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn mod_menu_slot_changed_owned(
        &mut self,
        conn: u32,
        uuid: u128,
        generation: u64,
        window: i32,
        layout: &'static ModMenuType,
        hidden: &[usize],
        menu: Option<&mut Menu>,
        before: &[Option<ItemStack>],
        after: &[Option<ItemStack>],
    ) -> bool {
        let changed = collect_slot_changes(layout, hidden, before, after);
        if changed.is_empty() {
            self.record_owned_slot_changed_seen(
                conn,
                generation,
                resized_items(after, layout.slots),
            );
            return true;
        }
        let context = self.slot_changed_context(conn, uuid, layout, None, after, menu.as_deref());
        let owned_limits = layout.owned_slots.as_deref();
        let Some(writes) =
            self.with_slot_changed_guard(SlotChangedTarget::Owned(generation), |game| {
                game.fire_slot_changed_for_guest(
                    conn,
                    layout,
                    hidden,
                    owned_limits,
                    None,
                    &changed,
                    &context,
                )
            })
        else {
            return true;
        };
        let Some(writes) = writes else {
            return false;
        };
        if let Some(player) = self.player(uuid) {
            self.journal_menus(player);
        }
        let Some(host) = self.lan.as_mut() else {
            return false;
        };
        let Some(gi) = host.hub.guests.connection_index(conn) else {
            return false;
        };
        let still_live = host.hub.guests[gi].menu_generation == generation
            && host.hub.guests[gi]
                .menu
                .as_ref()
                .is_some_and(|m| m.window == window);
        if !still_live {
            return true;
        }
        // Sync any Host writes the listener made to transient slots, then apply authoritative
        // output writes on top without re-firing `menu_slot_changed`.
        let mut live_items = resized_items(&host.hub.guests[gi].native_menu, layout.slots);
        for (slot, stack) in writes {
            live_items[slot] = stack;
        }
        host.hub.guests.update(gi, |g| {
            g.native_menu = live_items.clone();
        });
        if let Some(menu) = menu {
            for (slot, stack) in live_items.iter().enumerate() {
                menu.load_slot(slot, stack.clone());
            }
        }
        self.record_owned_slot_changed_seen(conn, generation, live_items);
        true
    }

    /// Fire `menu_slot_changed` for a block-backed mod menu inside a click transaction if any
    /// non-result owned container slot changed between `before` and `after`, applying any
    /// validated result-slot outputs to both the block entity and `menu` (when present) without
    /// re-firing.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn mod_menu_slot_changed_block(
        &mut self,
        conn: u32,
        uuid: u128,
        pos: Pos,
        generation: Option<u64>,
        window: i32,
        layout: &'static ModMenuType,
        hidden: &[usize],
        menu: Option<&mut Menu>,
        before: &[Option<ItemStack>],
        after: &[Option<ItemStack>],
    ) -> bool {
        let changed = collect_slot_changes(layout, hidden, before, after);
        if changed.is_empty() {
            if let Some(BlockEntity::Mod(entity)) = self.world.block_entities.get(&pos) {
                let items = entity.items.clone();
                self.record_block_slot_changed_seen(pos, &items);
            }
            return true;
        }
        let pos_arr = [pos.0, pos.1, pos.2];
        let context =
            self.slot_changed_context(conn, uuid, layout, Some(pos_arr), after, menu.as_deref());
        let block_limit = self.world.container_stack_limit(pos);
        let Some(writes) = self.with_slot_changed_guard(SlotChangedTarget::Block(pos), |game| {
            game.fire_slot_changed_for_guest(
                conn,
                layout,
                hidden,
                None,
                block_limit,
                &changed,
                &context,
            )
        }) else {
            return true;
        };
        let Some(writes) = writes else {
            return false;
        };
        if generation != self.world.block_entities.generation(&pos)
            || !self
                .lan
                .as_ref()
                .and_then(|host| host.hub.guest(conn))
                .and_then(|guest| guest.menu.as_ref())
                .is_some_and(|open| open.window == window)
        {
            return true;
        }
        if !writes.is_empty() {
            self.journal_block_entity(pos);
            let mut mutated = false;
            if let Some(BlockEntity::Mod(entity)) = self.world.block_entities.get_mut(&pos) {
                if entity.items.len() < layout.slots {
                    entity.items.resize(layout.slots, None);
                }
                for (slot, stack) in &writes {
                    if entity.items[*slot] != *stack {
                        entity.items[*slot] = stack.clone();
                        mutated = true;
                    }
                }
            }
            if mutated {
                self.after_commit(After::Changed(pos));
            }
        }
        if let Some(BlockEntity::Mod(entity)) = self.world.block_entities.get(&pos) {
            let items = entity.items.clone();
            if let Some(menu) = menu {
                for slot in 0..layout.slots {
                    if layout.owns_slot(slot) && layout.crafting.is_none_or(|c| c.result != slot) {
                        menu.load_slot(slot, items.get(slot).cloned().flatten());
                    }
                }
            }
            self.record_block_slot_changed_seen(pos, &items);
        }
        true
    }

    /// Reconcile non-result slot edits made by a `menu_clicked` override (`Flow::Cancel`) before
    /// the click savepoint commits. Returning `false` rolls back the cancelled click and resends.
    pub(super) fn reconcile_click_slot_changes(&mut self, conn: u32) -> bool {
        if !mod_api::__private::has_part_hook(mod_api::HookId::MenuSlotChanged) {
            return true;
        }
        enum Target {
            Owned {
                uuid: u128,
                generation: u64,
                window: i32,
                layout: &'static ModMenuType,
                hidden: Vec<usize>,
                before: Vec<Option<ItemStack>>,
                after: Vec<Option<ItemStack>>,
            },
            Block {
                uuid: u128,
                pos: Pos,
                generation: Option<u64>,
                window: i32,
                layout: &'static ModMenuType,
                hidden: Vec<usize>,
                before: Vec<Option<ItemStack>>,
                after: Vec<Option<ItemStack>>,
            },
        }
        let target = {
            let Some(host) = self.lan.as_ref() else {
                return true;
            };
            let Some(guest) = host.hub.guest(conn) else {
                return true;
            };
            let Some(open) = guest.menu.as_ref() else {
                return true;
            };
            let Some(layout) = open.mod_type else {
                return true;
            };
            if !mod_api::__private::menu_listens(mod_api::HookId::MenuSlotChanged, layout.id) {
                return true;
            }
            let seen_or_sent = || {
                open.slot_changed_seen.clone().or_else(|| {
                    (!open.sent.is_empty()).then(|| resized_items(&open.sent, layout.slots))
                })
            };
            match open.slots {
                Slots::Native if layout.owned_slots.is_some() => {
                    let Some(before) = seen_or_sent() else {
                        return true;
                    };
                    Some(Target::Owned {
                        uuid: guest.uuid,
                        generation: guest.menu_generation,
                        window: open.window,
                        layout,
                        hidden: open.hidden.clone(),
                        before,
                        after: resized_items(&guest.native_menu, layout.slots),
                    })
                }
                Slots::Block(pos) => {
                    let Some(BlockEntity::Mod(entity)) = self.world.block_entities.get(&pos) else {
                        return true;
                    };
                    let Some(before) = seen_or_sent() else {
                        return true;
                    };
                    Some(Target::Block {
                        uuid: guest.uuid,
                        pos,
                        generation: self.world.block_entities.generation(&pos),
                        window: open.window,
                        layout,
                        hidden: open.hidden.clone(),
                        before,
                        after: resized_items(&entity.items, layout.slots),
                    })
                }
                _ => None,
            }
        };
        match target {
            Some(Target::Owned {
                uuid,
                generation,
                window,
                layout,
                hidden,
                before,
                after,
            }) => {
                self.journal_guest_click(conn);
                self.journal_menu_click(conn);
                self.mod_menu_slot_changed_owned(
                    conn, uuid, generation, window, layout, &hidden, None, &before, &after,
                )
            }
            Some(Target::Block {
                uuid,
                pos,
                generation,
                window,
                layout,
                hidden,
                before,
                after,
            }) => self.mod_menu_slot_changed_block(
                conn, uuid, pos, generation, window, layout, &hidden, None, &before, &after,
            ),
            None => true,
        }
    }

    /// Notify `menu_slot_changed` for a blockless owned menu whose transient slots were modified
    /// outside a click (for example via `ItemPlace::MenuSlot`) once its transaction commits.
    /// Runs the listener in its own savepoint so invalid writes or panics roll back listener edits
    /// without corrupting committed storage, then syncs the open viewer immediately.
    pub(super) fn notify_owned_menu_slot_changed(
        &mut self,
        generation: u64,
        fallback_before: &[Option<ItemStack>],
        click_active: Option<u32>,
    ) {
        if self
            .slot_changed_active
            .contains(&SlotChangedTarget::Owned(generation))
            || !mod_api::__private::has_part_hook(mod_api::HookId::MenuSlotChanged)
        {
            return;
        }
        let info = {
            let Some(host) = self.lan.as_ref() else {
                return;
            };
            host.hub
                .guests
                .iter()
                .find(|g| g.placed && g.menu_generation == generation)
                .and_then(|g| {
                    let open = g.menu.as_ref()?;
                    let layout = open.mod_type.filter(|t| t.owned_slots.is_some())?;
                    (open.slots == Slots::Native
                        && mod_api::__private::menu_listens(
                            mod_api::HookId::MenuSlotChanged,
                            layout.id,
                        ))
                    .then(|| {
                        let before = open
                            .slot_changed_seen
                            .clone()
                            .unwrap_or_else(|| resized_items(fallback_before, layout.slots));
                        let after = resized_items(&g.native_menu, layout.slots);
                        (
                            g.conn,
                            g.uuid,
                            open.window,
                            layout,
                            open.hidden.clone(),
                            before,
                            after,
                        )
                    })
                })
        };
        let Some((conn, uuid, window, layout, hidden, before, after)) = info else {
            return;
        };
        let changed = collect_slot_changes(layout, &hidden, &before, &after);
        if changed.is_empty() {
            self.record_owned_slot_changed_seen(conn, generation, after);
            return;
        }
        let context = self.slot_changed_context(conn, uuid, layout, None, &after, None);
        let owned_limits = layout.owned_slots.as_deref();
        let _ = self.with_slot_changed_guard(SlotChangedTarget::Owned(generation), |game| {
            let _ = mod_api::run_transaction(game, |game, _| {
                let Some(writes) = game.fire_slot_changed_for_guest(
                    conn,
                    layout,
                    &hidden,
                    owned_limits,
                    None,
                    &changed,
                    &context,
                ) else {
                    return Err(());
                };
                if !writes.is_empty() {
                    if let Some(player) = game.player(uuid) {
                        game.journal_menus(player);
                    }
                    if let Some(host) = game.lan.as_mut()
                        && let Some(gi) = host.hub.guests.connection_index(conn)
                        && host.hub.guests[gi].menu_generation == generation
                        && host.hub.guests[gi]
                            .menu
                            .as_ref()
                            .is_some_and(|m| m.window == window)
                    {
                        let mut live =
                            resized_items(&host.hub.guests[gi].native_menu, layout.slots);
                        for (slot, stack) in writes {
                            live[slot] = stack;
                        }
                        host.hub.guests.update(gi, |g| g.native_menu = live);
                    }
                }
                Ok(())
            });
        });
        let final_items = self
            .lan
            .as_ref()
            .and_then(|host| host.hub.guest(conn))
            .filter(|g| g.menu_generation == generation)
            .map(|g| resized_items(&g.native_menu, layout.slots));
        if let Some(final_items) = final_items {
            self.record_owned_slot_changed_seen(conn, generation, final_items);
            if Some(conn) != click_active {
                self.sync_guest_container(conn);
            }
        }
    }
}

struct BlockMenuViewer {
    conn: u32,
    uuid: u128,
    layout: &'static ModMenuType,
    hidden: Vec<usize>,
    seen: Option<Vec<Option<ItemStack>>>,
    sent: Vec<Option<ItemStack>>,
}

/// O(1)-indexed lookup of placed guests passing `here` who currently view a block-backed mod menu
/// at `pos` whose menu type listens to `menu_slot_changed`. Returns immediately without scanning
/// the full guest list or allocating when no guest has a block-backed mod menu open at `pos`.
fn listening_block_menu_viewers(
    guests: &sunrise_client::lan::guest_list::GuestList,
    here: impl Fn(&sunrise_client::lan::guest::Guest) -> bool,
    pos: Pos,
) -> Vec<BlockMenuViewer> {
    let viewer_indices = guests.block_mod_menu_viewers(pos);
    if viewer_indices.is_empty() {
        return Vec::new();
    }
    let mut viewers = Vec::new();
    for &gi in viewer_indices {
        let g = &guests[gi];
        if !g.placed || !here(g) {
            continue;
        }
        let Some(open) = g.menu.as_ref() else {
            continue;
        };
        let Some(layout) = open.mod_type else {
            continue;
        };
        if open.slots != Slots::Block(pos)
            || !mod_api::__private::menu_listens(mod_api::HookId::MenuSlotChanged, layout.id)
        {
            continue;
        }
        viewers.push(BlockMenuViewer {
            conn: g.conn,
            uuid: g.uuid,
            layout,
            hidden: open.hidden.clone(),
            seen: open.slot_changed_seen.clone(),
            sent: open.sent.clone(),
        });
    }
    viewers
}

/// Gate external and transactional block-menu slot-change work on the O(1) open-menu index so
/// container cloning and context lifting run only when at least one active listening viewer has
/// the menu open at `pos`.
fn with_listening_block_viewers<R>(
    guests: &sunrise_client::lan::guest_list::GuestList,
    here: impl Fn(&sunrise_client::lan::guest::Guest) -> bool,
    pos: Pos,
    on_viewed: impl FnOnce(Vec<BlockMenuViewer>) -> R,
) -> Option<R> {
    let viewers = listening_block_menu_viewers(guests, here, pos);
    if viewers.is_empty() {
        return None;
    }
    Some(on_viewed(viewers))
}

impl Game<'_> {
    /// Notify `menu_slot_changed` for any open block-backed mod menu at `pos` whose backing block
    /// entity changed outside a click (committed transaction write, hopper, `/item replace block`,
    /// or `merge_data`), deduplicated across all open viewers and syncing every viewer immediately.
    ///
    /// Performs an O(1) open-menu index lookup first and returns immediately without touching
    /// `world.block_entities`, cloning container items, scanning guests, or lifting a `Menu`
    /// context when no active listening viewer has a menu open at `pos`.
    pub(super) fn notify_block_menu_slot_changed(
        &mut self,
        pos: Pos,
        fallback_before: Option<&[Option<ItemStack>]>,
        click_active: Option<u32>,
    ) {
        if self
            .slot_changed_active
            .contains(&SlotChangedTarget::Block(pos))
            || !mod_api::__private::has_part_hook(mod_api::HookId::MenuSlotChanged)
        {
            return;
        }
        let Some((viewers, generation, live_before_hook)) = self.lan.as_ref().and_then(|host| {
            with_listening_block_viewers(
                &host.hub.guests,
                |g| host.hub.here(g),
                pos,
                |viewers| {
                    let Some(BlockEntity::Mod(entity)) = self.world.block_entities.get(&pos) else {
                        return None;
                    };
                    Some((
                        viewers,
                        self.world.block_entities.generation(&pos),
                        entity.items.clone(),
                    ))
                },
            )
            .flatten()
        }) else {
            return;
        };
        let rep = viewers
            .iter()
            .find(|v| Some(v.conn) == self.lan_acting)
            .unwrap_or(&viewers[0]);
        let before = match (&rep.seen, fallback_before) {
            (Some(seen), _) => seen.clone(),
            (None, Some(fallback)) => resized_items(fallback, rep.layout.slots),
            (None, None) if !rep.sent.is_empty() => resized_items(&rep.sent, rep.layout.slots),
            (None, None) => {
                self.record_block_slot_changed_seen(pos, &live_before_hook);
                return;
            }
        };
        let after = resized_items(&live_before_hook, rep.layout.slots);
        let changed = collect_slot_changes(rep.layout, &rep.hidden, &before, &after);
        if !changed.is_empty() {
            let (conn, uuid, layout, hidden) = (rep.conn, rep.uuid, rep.layout, rep.hidden.clone());
            let context = self.slot_changed_context(
                conn,
                uuid,
                layout,
                Some([pos.0, pos.1, pos.2]),
                &after,
                None,
            );
            let block_limit = self.world.container_stack_limit(pos);
            let _ = self.with_slot_changed_guard(SlotChangedTarget::Block(pos), |game| {
                let _ = mod_api::run_transaction(game, |game, _| {
                    let Some(writes) = game.fire_slot_changed_for_guest(
                        conn,
                        layout,
                        &hidden,
                        None,
                        block_limit,
                        &changed,
                        &context,
                    ) else {
                        return Err(());
                    };
                    if !writes.is_empty()
                        && generation == game.world.block_entities.generation(&pos)
                    {
                        game.journal_block_entity(pos);
                        let mut mutated = false;
                        if let Some(BlockEntity::Mod(entity)) =
                            game.world.block_entities.get_mut(&pos)
                        {
                            if entity.items.len() < layout.slots {
                                entity.items.resize(layout.slots, None);
                            }
                            for (slot, stack) in &writes {
                                if entity.items[*slot] != *stack {
                                    entity.items[*slot] = stack.clone();
                                    mutated = true;
                                }
                            }
                        }
                        if mutated {
                            game.after_commit(After::Changed(pos));
                        }
                    }
                    Ok(())
                });
            });
        }
        if let Some(BlockEntity::Mod(entity)) = self.world.block_entities.get(&pos) {
            let final_items = entity.items.clone();
            self.record_block_slot_changed_seen(pos, &final_items);
        }
        for viewer in viewers {
            if Some(viewer.conn) != click_active {
                self.sync_guest_container(viewer.conn);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{SlotWriteRefusal, collect_slot_changes, context_from_menu, validate_slot_writes};
    use mod_api::{MenuContainer, MenuSlotWrite};
    use sunrise_client::{
        containers::{ModCrafting, ModLinks, ModMenuType},
        items::{ItemStack, Items},
        menu::{Kind, Menu},
    };

    fn with_test_item_keys(f: impl FnOnce(mod_api::ItemKey, mod_api::ItemKey, Items)) {
        let mods = mod_api::Mods::default();
        mod_platform::testing::with_mods(mods.clone(), || {
            super::super::api_values::install_parts(&mods).unwrap();
            mod_api::__private::resolve_items([
                "minecraft:iron_sword",
                "minecraft:stick",
                "minecraft:diamond_sword",
                "minecraft:netherite_chestplate",
                "minecraft:diamond_chestplate",
                "minecraft:potion",
            ])
            .unwrap();
            let stick = mod_api::__private::item_key("minecraft:stick").unwrap();
            let potion = mod_api::__private::item_key("minecraft:potion").unwrap();
            static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
            let root = std::env::temp_dir().join(format!(
                "sunrise-slot-changes-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            ));
            std::fs::create_dir_all(&root).unwrap();
            std::fs::write(
                root.join("items.json"),
                r#"{"minecraft:stick":{"minecraft:max_stack_size":64}}"#,
            )
            .unwrap();
            let defaults = sunrise_world::item_defaults::read(&root).unwrap();
            let items =
                Items::catalog(&root, &sunrise_world::loot::PackOverlay::NONE, &defaults).unwrap();
            std::fs::remove_dir_all(root).unwrap();
            f(stick, potion, items);
        });
    }

    #[test]
    fn collect_slot_changes_skips_results_hidden_and_linked_slots() {
        with_test_item_keys(|stick, _, _| {
            let layout = ModMenuType {
                id: "test:machine",
                owner: "test",
                slots: 5,
                player_inventory: true,
                data_slots: 0,
                owned_slots: None,
                results: vec![2],
                inputs: vec![0, 1],
                crafting: None,
                links: Some(ModLinks { from: 4, count: 1 }),
            };
            let before = vec![
                None,
                Some(ItemStack::new("minecraft:stick", 1)),
                None,
                None,
                None,
            ];
            let after = vec![
                Some(ItemStack::new("minecraft:stick", 2)),
                Some(ItemStack::new("minecraft:stick", 1)),
                Some(ItemStack::new("minecraft:potion", 1)),
                Some(ItemStack::new("minecraft:potion", 3)),
                Some(ItemStack::new("minecraft:potion", 4)),
            ];
            let changes = collect_slot_changes(&layout, &[3], &before, &after);
            assert_eq!(changes.len(), 1);
            assert_eq!(changes[0].container, MenuContainer::Main);
            assert_eq!(changes[0].slot, 0);
            assert_eq!(changes[0].before, None);
            assert_eq!(
                changes[0].after,
                Some(mod_api::ItemStack::new(stick, 2).unwrap())
            );
        });
    }

    #[test]
    fn collect_slot_changes_batches_multi_slot_edits_in_menu_order() {
        with_test_item_keys(|stick, potion, _| {
            let layout = ModMenuType {
                id: "test:processor",
                owner: "test",
                slots: 4,
                player_inventory: true,
                data_slots: 0,
                owned_slots: Some(vec![64, 64, 64, 16]),
                results: vec![3],
                inputs: vec![0, 1, 2],
                crafting: None,
                links: None,
            };
            let before = vec![
                Some(ItemStack::new("minecraft:stick", 1)),
                None,
                Some(ItemStack::new("minecraft:potion", 4)),
                None,
            ];
            let after = vec![
                Some(ItemStack::new("minecraft:stick", 3)),
                Some(ItemStack::new("minecraft:stick", 2)),
                None,
                Some(ItemStack::new("minecraft:potion", 1)),
            ];
            let changes = collect_slot_changes(&layout, &[], &before, &after);
            assert_eq!(changes.len(), 3);
            assert_eq!(
                changes.iter().map(|c| c.slot).collect::<Vec<_>>(),
                vec![0, 1, 2]
            );
            assert_eq!(
                changes[0].before,
                Some(mod_api::ItemStack::new(stick, 1).unwrap())
            );
            assert_eq!(
                changes[0].after,
                Some(mod_api::ItemStack::new(stick, 3).unwrap())
            );
            assert_eq!(changes[1].before, None);
            assert_eq!(
                changes[1].after,
                Some(mod_api::ItemStack::new(stick, 2).unwrap())
            );
            assert_eq!(
                changes[2].before,
                Some(mod_api::ItemStack::new(potion, 4).unwrap())
            );
            assert_eq!(changes[2].after, None);
        });
    }

    #[test]
    fn validate_slot_writes_enforces_scope_container_and_slot_caps() {
        with_test_item_keys(|stick, _, items| {
            let owned_layout = ModMenuType {
                id: "test:owned",
                owner: "test",
                slots: 3,
                player_inventory: true,
                data_slots: 0,
                owned_slots: Some(vec![16, 16, 4]),
                results: vec![2],
                inputs: vec![0, 1],
                crafting: None,
                links: None,
            };
            let block_layout = ModMenuType {
                id: "test:block",
                owner: "test",
                slots: 3,
                player_inventory: true,
                data_slots: 0,
                owned_slots: None,
                results: vec![2],
                inputs: vec![0, 1],
                crafting: None,
                links: None,
            };
            let four = mod_api::ItemStack::new(stick, 4).unwrap();
            let five = mod_api::ItemStack::new(stick, 5).unwrap();

            // Owned menu accepts declared slot within its per-slot cap and clearing.
            let ok = validate_slot_writes(
                &items,
                |_| 64,
                &owned_layout,
                &[],
                owned_layout.owned_slots.as_deref(),
                None,
                vec![
                    MenuSlotWrite::new(MenuContainer::Main, 2, Some(four.clone())),
                    MenuSlotWrite::new(MenuContainer::Main, 0, None),
                ],
            )
            .unwrap();
            assert_eq!(ok.len(), 2);

            // Over owned slot cap -> refused.
            assert_eq!(
                validate_slot_writes(
                    &items,
                    |_| 64,
                    &owned_layout,
                    &[],
                    owned_layout.owned_slots.as_deref(),
                    None,
                    vec![MenuSlotWrite::new(MenuContainer::Main, 2, Some(five))],
                ),
                Err(SlotWriteRefusal::StackLimitExceeded)
            );

            // Non-Main container -> refused.
            assert_eq!(
                validate_slot_writes(
                    &items,
                    |_| 64,
                    &owned_layout,
                    &[],
                    owned_layout.owned_slots.as_deref(),
                    None,
                    vec![MenuSlotWrite::new(
                        MenuContainer::Player,
                        0,
                        Some(four.clone())
                    )],
                ),
                Err(SlotWriteRefusal::NonMainContainer)
            );

            // Block-backed menu accepts result slot, refuses non-result input slot, refuses hidden.
            assert!(
                validate_slot_writes(
                    &items,
                    |_| 64,
                    &block_layout,
                    &[],
                    None,
                    Some(64),
                    vec![MenuSlotWrite::new(
                        MenuContainer::Main,
                        2,
                        Some(four.clone())
                    )],
                )
                .is_ok()
            );
            assert_eq!(
                validate_slot_writes(
                    &items,
                    |_| 64,
                    &block_layout,
                    &[],
                    None,
                    Some(64),
                    vec![MenuSlotWrite::new(
                        MenuContainer::Main,
                        0,
                        Some(four.clone())
                    )],
                ),
                Err(SlotWriteRefusal::NonResultBlockSlot)
            );
            assert_eq!(
                validate_slot_writes(
                    &items,
                    |_| 64,
                    &block_layout,
                    &[2],
                    None,
                    Some(64),
                    vec![MenuSlotWrite::new(MenuContainer::Main, 2, Some(four))],
                ),
                Err(SlotWriteRefusal::UnavailableSlot)
            );
        });
    }

    #[test]
    fn context_from_menu_includes_main_grid_and_result_containers() {
        with_test_item_keys(|stick, potion, _| {
            let layout: &'static ModMenuType = Box::leak(Box::new(ModMenuType {
                id: "test:crafter",
                owner: "test",
                slots: 6,
                player_inventory: true,
                data_slots: 0,
                owned_slots: None,
                results: Vec::new(),
                inputs: vec![0, 5],
                crafting: Some(ModCrafting {
                    grid: 0,
                    size: 2,
                    result: 4,
                }),
                links: None,
            }));
            let mut menu = Menu::new(Kind::Mod(layout));
            menu.load_slot(0, Some(ItemStack::new("minecraft:stick", 2)));
            menu.load_slot(4, Some(ItemStack::new("minecraft:potion", 1)));
            menu.load_slot(5, Some(ItemStack::new("minecraft:stick", 3)));

            let ctx = context_from_menu(&menu, 42, Some([1, 64, 1]));
            assert_eq!(ctx.player, Some(42));
            assert_eq!(ctx.pos, Some([1, 64, 1]));
            assert_eq!(
                ctx.slot(MenuContainer::Grid, 0),
                Some(&mod_api::ItemStack::new(stick, 2).unwrap())
            );
            assert_eq!(
                ctx.slot(MenuContainer::Result, 0),
                Some(&mod_api::ItemStack::new(potion, 1).unwrap())
            );
            assert_eq!(
                ctx.slot(MenuContainer::Main, 5),
                Some(&mod_api::ItemStack::new(stick, 3).unwrap())
            );
            assert_eq!(ctx.slot(MenuContainer::Main, 0), None);
            assert_eq!(ctx.slot(MenuContainer::Main, 4), None);
        });
    }

    #[test]
    fn external_block_slot_change_skips_clone_and_context_lift_without_listening_viewer() {
        use std::sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        };
        use sunrise_client::lan::{
            Joined, PlayerSender,
            guest::Guest,
            guest_list::GuestList,
            menu::{Menu as OpenMenu, Slots},
        };

        const LISTENING_MENU: &str = "test:listening_machine";
        const SILENT_MENU: &str = "test:silent_machine";

        struct ListeningPart;
        #[mod_api::__subscriptions]
        impl mod_api::MenuPart for ListeningPart {
            fn menu_slot_changed(
                &self,
                _: &mut dyn mod_api::ServerHost,
                _: &mut mod_api::MenuSlotChangedEvent<'_>,
            ) {
            }
        }
        struct TestMod;
        #[mod_api::__subscriptions]
        impl mod_api::Hooks for TestMod {
            fn register(&self, r: &mut dyn mod_api::Registrar, _: &mod_api::RegistrationContext) {
                r.menu_part(LISTENING_MENU, ListeningPart);
            }
        }

        let make_guest = |conn: u32, placed: bool, dim: &str| {
            let (tx, _) = std::sync::mpsc::channel();
            let mut g = Guest::new(
                Joined {
                    conn,
                    uuid: u128::from(conn) + 500,
                    name: format!("guest{conn}"),
                    properties: Vec::new(),
                    info: sunrise_protocol::packets::configuration::ClientInformation::new(8),
                    brand: "sunrise".into(),
                    sender: PlayerSender::new(tx, Arc::default()),
                    latency: Arc::default(),
                    address: "127.0.0.1:25565".parse().unwrap(),
                    expected: false,
                },
                conn as i32 + 20,
                [0.0, 64.0, 0.0],
                8,
            );
            g.placed = placed;
            g.dimension = dim.to_owned();
            g
        };

        let listening_layout: &'static ModMenuType = Box::leak(Box::new(ModMenuType {
            id: LISTENING_MENU,
            owner: "test",
            slots: 2,
            player_inventory: true,
            data_slots: 0,
            owned_slots: None,
            results: vec![1],
            inputs: vec![0],
            crafting: None,
            links: None,
        }));
        let silent_layout: &'static ModMenuType = Box::leak(Box::new(ModMenuType {
            id: SILENT_MENU,
            owner: "test",
            slots: 2,
            player_inventory: true,
            data_slots: 0,
            owned_slots: None,
            results: vec![1],
            inputs: vec![0],
            crafting: None,
            links: None,
        }));
        let open_at = |layout: &'static ModMenuType, pos: super::Pos| OpenMenu {
            window: 1,
            kind: 0,
            registry_id: layout.id.into(),
            pos: Some(pos),
            slots: Slots::Block(pos),
            size: layout.slots,
            state: 0,
            sent: Vec::new(),
            drag: None,
            sent_data: Vec::new(),
            blockless_data: Vec::new(),
            hidden: Vec::new(),
            mod_type: Some(layout),
            craft: None,
            links: Vec::new(),
            asks_still_valid: false,
            opening_item: None,
            slot_changed_seen: None,
        };

        let mods: mod_api::Mods = vec![mod_api::ModEntry::new("test", Box::new(TestMod))].into();
        mod_platform::testing::with_mods(mods.clone(), || {
            super::super::api_values::install_parts(&mods).unwrap();
            mod_api::__private::resolve_items(["minecraft:stick"]).unwrap();

            let target = (10, 64, 10);
            let other = (20, 64, 20);
            let backing = vec![Some(ItemStack::new("minecraft:stick", 4)), None];
            let clones = AtomicUsize::new(0);
            let lifts = AtomicUsize::new(0);
            let run_path = |guests: &GuestList| {
                super::with_listening_block_viewers(
                    guests,
                    |g| g.placed_in("minecraft:overworld"),
                    target,
                    |viewers| {
                        clones.fetch_add(1, Ordering::Relaxed);
                        let cloned = backing.clone();
                        lifts.fetch_add(1, Ordering::Relaxed);
                        let mut menu = Menu::new(Kind::Mod(viewers[0].layout));
                        for (slot, stack) in cloned.into_iter().enumerate() {
                            menu.load_slot(slot, stack);
                        }
                        context_from_menu(&menu, viewers[0].uuid, Some([10, 64, 10]))
                    },
                )
            };

            let mut guests = GuestList::default();
            // 1. No viewers at target, even with guests viewing another block, an unplaced guest,
            //    a guest in another dimension, and a viewer of a non-listening menu at target.
            guests.push(make_guest(1, true, "minecraft:overworld"));
            guests.push(make_guest(2, false, "minecraft:overworld"));
            guests.push(make_guest(3, true, "minecraft:the_nether"));
            guests.push(make_guest(4, true, "minecraft:overworld"));
            guests.update(0, |g| g.menu = Some(open_at(listening_layout, other)));
            guests.update(1, |g| g.menu = Some(open_at(listening_layout, target)));
            guests.update(2, |g| g.menu = Some(open_at(listening_layout, target)));
            guests.update(3, |g| g.menu = Some(open_at(silent_layout, target)));
            assert!(run_path(&guests).is_none());
            assert_eq!(clones.load(Ordering::Relaxed), 0);
            assert_eq!(lifts.load(Ordering::Relaxed), 0);

            // 2. Opening the listening menu at target for an active placed guest clones & lifts once.
            guests.update(0, |g| g.menu = Some(open_at(listening_layout, target)));
            let ctx = run_path(&guests).expect("listening viewer should build context");
            assert_eq!(ctx.player, Some(501));
            assert_eq!(ctx.pos, Some([10, 64, 10]));
            assert_eq!(clones.load(Ordering::Relaxed), 1);
            assert_eq!(lifts.load(Ordering::Relaxed), 1);

            // 3. Closing the menu returns to zero-clone / zero-lift fast path.
            guests.update(0, |g| g.menu = None);
            assert!(run_path(&guests).is_none());
            assert_eq!(clones.load(Ordering::Relaxed), 1);
            assert_eq!(lifts.load(Ordering::Relaxed), 1);

            // 4. Reopening and removing the guest (disconnect) also clears the index.
            guests.update(0, |g| g.menu = Some(open_at(listening_layout, target)));
            guests.remove(0);
            assert!(run_path(&guests).is_none());
            assert_eq!(clones.load(Ordering::Relaxed), 1);
            assert_eq!(lifts.load(Ordering::Relaxed), 1);
        });
    }
}
