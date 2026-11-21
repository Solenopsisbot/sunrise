//! The `menu_closed` hook: declarations the mod API generator reads (see `hooks/mod.rs`).
// catalog-order: 0035

pub trait Hooks {
    /// A menu a mod opened closed: the player closed it, the server or a mod
    /// opened another, or the player left. `opening_hand` is the hand the opener supplied,
    /// if any. For a generic `owned_slots` menu (without declared `results`), `owned_slots`
    /// contains the transient container's final values in slot order and those stacks are not
    /// returned or dropped automatically by the game. For a processing owned menu (declaring
    /// both `owned_slots` and `results`), the game automatically returns non-result inputs to
    /// the player's inventory (dropping overflow, or dropping on disconnect or death), discards
    /// declared `results` slots, and passes an empty `owned_slots` slice here so items cannot
    /// be duplicated. The window is already detached; a listener can use the Host to return
    /// generic owned slots or update the opening item if the player is still available.
    #[hook(
        side = "server",
        contract = "listen",
        version = 3,
        neoforge = "PlayerContainerEvent.Close"
    )]
    fn menu_closed();
}
