use super::*;

impl Game<'_> {
    /// A player (`player`, their UUID) took `taken` out of slot `slot` (the menu's index) of
    /// the mod menu type `menu`, which holds `left` after it: `Slot.onTake`, on the server,
    /// after the take. For block-backed menus `pos` and `entity` are `Some` (detached out of the
    /// world during the call); for blockless owned menus both are `None`. The caller undoes the
    /// take when a listener fails.
    pub(in crate::game) fn native_menu_slot_take(
        &mut self,
        call: mod_api::__private::MenuSlotTakeCall<'_>,
    ) {
        self.fire_menu_slot_take(call);
    }
}
