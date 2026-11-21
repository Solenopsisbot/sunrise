//! The `menu_slot_changed` hook: declarations the mod API generator reads (see `hooks/mod.rs`).

pub trait Hooks {
    /// One or more non-result container slots of an open mod menu of type `menu` changed on the
    /// server (`Container.setItem` / `AbstractContainerMenu.slotsChanged`). Fires for every
    /// authoritative input change:
    ///
    /// - Clicks (default clicks on blockless owned or block-backed mod menus, `menu_quick_move`
    ///   handlers, and `menu_clicked` overrides returning `Flow::Cancel`), running inside the
    ///   click savepoint before commit.
    /// - Committed host writes (`ItemPlace::MenuSlot`, `ItemPlace::BlockEntity`,
    ///   `BlockEntityRef::set_item_slot`, transactional `BlockEntityRef::merge_data` /
    ///   `replace_data`), delivered once per outermost committed transaction and never on abort.
    /// - External container writes to a block-backed mod menu (hoppers, `/item replace block`,
    ///   non-transactional `merge_data`), delivered immediately with all open viewers synced.
    ///
    /// Multiple non-result slot changes within one click step (such as a multi-slot drag) or one
    /// committed transaction are batched into a single `menu_slot_changed` call with `changed` in
    /// ascending menu slot order. Each [`MenuSlotChange`] gives the container, backing index,
    /// stack before and stack after; player inventory slots, linked containers, hidden slots and
    /// result-slot changes are never included. `context` carries a viewing player, the backing
    /// block position (`None` for a blockless owned menu even if its opener passed a screen
    /// position hint), and a read-only snapshot of the menu's own containers after the change.
    ///
    /// Push [`MenuSlotWrite`] entries to `outputs` to set declared result slots (or any declared
    /// non-hidden owned container slot on a blockless owned menu) atomically before viewers sync
    /// and before any click continuation runs. Writes applied from `outputs` never re-fire
    /// `menu_slot_changed`, and later entries for the same slot win. An invalid write (a
    /// non-`Main` container, an out-of-range, hidden, linked, or non-result slot on a block-backed
    /// menu, an invalid stack, or a count past the slot's and item's limit), or a listener panic,
    /// rolls back the entire click (cursor, inventory, transient slots or block entity, and
    /// effects) and resends the menu during a click, or rolls back the listener's own writes
    /// without corrupting the already-committed container change outside a click. Nothing is asked
    /// when no part listens for `menu`.
    #[hook(
        side = "server",
        contract = "listen",
        scope = "menu",
        key = "menu",
        neoforge = "none (vanilla Container.setItem and AbstractContainerMenu.slotsChanged; NeoForge adds no event)"
    )]
    fn menu_slot_changed(
        menu: &str,
        changed: &[MenuSlotChange],
        context: &MenuSlotContext,
        outputs: &mut Vec<MenuSlotWrite>,
    );
}
