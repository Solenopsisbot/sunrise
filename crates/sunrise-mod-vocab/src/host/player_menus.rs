//! The `player_menus` area: the menu a player has open. Host method declarations; access defaults and game forwarders are generated.
#![allow(unused_variables)]
use crate::*;
/// The handles' reads of the `player_menus` area, implemented by the game and reached through [`Backend`].
// hidden: a handle's backend, reached through `Backend`; mods call the handle, never this trait
#[doc(hidden)]
#[sunrise_mod_api_macros::host_area]
pub trait PlayerMenusBackend {
    /// Side: both.
    /// A player's current supported menu, or the inventory menu when none is open.
    /// On a client, only the local player's menu is available: its slots and carried stack
    /// are the client's view (server updates plus local prediction), not server authority.
    /// On a server, refuses crafting tables, merchants, workstations, lecterns, mounts,
    /// stale players and players held out by another active turn. Client screen visibility
    /// does not change the menu's backing indices.
    /// Reads for the acting player use their current inventory, equipment, cursor and ender
    /// contents. On a client, refuses stale players and unsupported menu layouts.
    fn player_container_menu(&self, player: PlayerRef) -> Option<MenuRef>;
    /// Side: server.
    /// The player's container counter (`ServerPlayer.containerCounter`): the window id the
    /// last menu opened for them took, 0 until one opens. Every menu the server opens for
    /// them advances it first, as vanilla's `nextContainerCounter` does, to `counter % 100 + 1`
    /// and takes that id: a block's or an entity's container, a mount's inventory, a merchant,
    /// [`PlayerRef::open_mod_menu`](crate::PlayerRef::open_mod_menu), and
    /// [`PlayerRef::next_container_counter`](crate::PlayerRef::next_container_counter). So the
    /// next open takes `container_counter % 100 + 1`, the id a Java `MenuProvider.createMenu`
    /// is handed. None for a player not online here (a stale handle) and on a client.
    fn player_container_counter(&self, player: PlayerRef) -> Option<i32>;
}
/// The handles' writes of the `player_menus` area, implemented by the game and reached through [`BackendMut`].
// hidden: a handle's backend, reached through `Backend`; mods call the handle, never this trait
#[doc(hidden)]
#[sunrise_mod_api_macros::host_area]
pub trait PlayerMenusBackendMut {
    /// Side: server.
    /// Opens a block entity's menu for a connected player (`ServerPlayer.openMenu`).
    /// Returns Stale for an invalidated entity; Ok(false) for a wrong-level player,
    /// non-provider, cancelled open, active menu click, an out-of-range slot exclusion, or an
    /// unconnected player such as a fake player.
    fn player_open_menu(
        &mut self,
        player: PlayerRef,
        entity: BlockEntityRef,
    ) -> Result<bool, BlockEntityError>;
    /// Side: server.
    /// Opens content mod menu type `menu` for a connected player from anywhere, with no
    /// block (`ServerPlayer.openMenu(provider, extraDataWriter)`): an item's use, an entity,
    /// a command. The menu replaces the one they had open. Its container slots show empty
    /// and their clicks reach `menu_clicked`. `extra` (at most 1,048,555 bytes, fitting within
    /// vanilla's 1 MiB clientbound custom payload limit) is available only during construction in
    /// [`crate::MenuScreenOpen::extra`]. `pos`, when given, is offered to the factory and
    /// available later through `screen_pos`. `data` has exactly the menu type's `data_slots`
    /// signed `i32` values. The negotiated blockless payload sends indices `0..=u16::MAX` after
    /// opening; read the received values through [`crate::MenuScreenFrame::data`]. The menu's
    /// owning mod can change the stored values with [`MenuRef::set_data_slot`]. Pass
    /// `opening_hand` for an item opener regardless of slot backing, or `None` for another opener.
    /// The server remembers that hand's stack for `menu_still_valid`. A type declaring
    /// `owned_slots` uses ordinary clicks and accepts writes through `ItemPlace::MenuSlot`; it may
    /// declare take-only `results` but not a `crafting` grid. The old menu closes on the server as
    /// a close does (its
    /// `removed`, `player_container_close`, the cursor stack and crafting grids back to the
    /// inventory) before the new one is built; `replace` says whether the client is first sent
    /// a close for it, as for `open_menu`. `MenuReplace::InPlace` sends none: the client keeps
    /// its screen and mouse until the new screen replaces it. The window id, or Ok(None) when
    /// a mod cancelled the open or a registered exclusion is out of range. Errors: `NotModMenu`,
    /// `OwnedSlotsWithCrafting`, `ClickInProgress` (a speculative click cannot open a new screen),
    /// `PlayerUnavailable` (including a client that does not support the blockless layout, a
    /// disconnect in progress or a turn unavailable for the veto), `TooLarge`,
    /// `DataCountMismatch`, `Title` or `TitleNotEncodable`. A cancelled or refused open
    /// does not close the old menu.
    #[serialize_text]
    fn player_open_mod_menu(
        &mut self,
        player: PlayerRef,
        request: ModMenuOpen,
    ) -> Result<Option<i32>, OpenMenuError>;
    /// Side: server.
    /// Backs the container slots of the mod menu `window` that `player_open_mod_menu` opened
    /// for the player with storage that is not a block entity (#3385; NeoForge's
    /// `StackCopySlot` and `ResourceHandlerSlot`): `places[n]` is what slot `n` shows. From then
    /// on a click or shift-click on those slots, and an `ItemPlace::MenuSlot` write, go through
    /// that storage's own handler, journalled; the client sees ordinary synced slots, mirrored
    /// after each click and each write through an `ItemPlace` of that storage, never each tick.
    /// Right after the open is the place to call it, the player's own item use included: backed
    /// inside a hook of that player's own action, the slots are mirrored once the action is over.
    /// A slot past `places` stays the mods' own (`menu_clicked`). The menu closes once a
    /// place stops resolving (the backpack left the hand, the entity is gone) or a held stack is
    /// no longer the one bound when the slots were backed, checked before each click and at
    /// each change notice. A `MenuSlotPlace::HeldItem` locks the inventory slot holding that
    /// stack while the menu is open, so it can't go into itself. Backing again replaces the
    /// places; a slot no longer backed (an empty list, a shorter one) is emptied, never left
    /// holding the copy it showed. A [`MenuSlotBackRefusal`], and nothing changes, for a player
    /// not online or a window that is not their open menu, a menu that is not a blockless mod
    /// menu, a menu that declares `owned_slots`, more places than its container slots, or a
    /// place that doesn't resolve now.

    fn player_back_mod_menu_slots(
        &mut self,
        player: PlayerRef,
        window: i32,
        places: Vec<MenuSlotPlace>,
    ) -> Result<(), MenuSlotBackRefusal>;
    /// Side: server.
    /// Advances the player's container counter as an open does (`ServerPlayer.nextContainerCounter`:
    /// `counter % 100 + 1`) and returns the new value, opening and closing nothing: the open
    /// menu keeps its own window id. For a mod that models `ServerPlayer.openMenu` when no
    /// window follows the advance, as when `MenuProvider.createMenu` answers `null`: vanilla
    /// advances the counter before it asks, and the next menu opened takes the id after this
    /// one. An aborted transaction does not undo the advance, as it does not undo a menu's open,
    /// except a merchant's window replaced in that same transaction, whose abort puts back the
    /// window and counter it replaced. None for a player not online here and on a client.
    fn player_next_container_counter(&mut self, player: PlayerRef) -> Option<i32>;
    /// Side: server.
    /// Opens the player's own ender chest for them with no block, titled `title`: a three-row
    /// `ChestMenu` over their `PlayerEnderChestContainer`, as `EnderChestBlock.useWithoutItem`
    /// opens it (`ChestMenu.threeRows`) but with no active chest, so it stays valid wherever
    /// they go. An item's use, a command or a tick can open it. Its slots are the player's
    /// ender chest itself: clicks, shift-clicks, drags and the close move stacks as an ender
    /// chest block's menu does, and the game writes and saves them. The menu replaces the one
    /// they had open, as `ServerPlayer.openMenu` does, and the mods may cancel it with
    /// `container_opened` (`player_container_open` is the notice after it opened, and cannot
    /// cancel). Inside a hook of the player's own action (`use_item`, `right_click_item`,
    /// `right_click_block`, their command), the menu opens when that action ends, as a block's
    /// menu the same action opens does. Ok(true) inside an action means queued: the menu is
    /// still subject to cancellation when the queue runs; outside one it opened. Ok(false) is an
    /// immediate cancellation by a mod. Errors:
    /// `PlayerUnavailable` (stale, dead, fake, or on a client), or `Title` for a title that
    /// could not cross; nothing opens then.
    #[serialize_text]
    fn player_open_ender_chest(
        &mut self,
        player: PlayerRef,
        title: Text,
    ) -> Result<bool, OpenMenuError>;
}
