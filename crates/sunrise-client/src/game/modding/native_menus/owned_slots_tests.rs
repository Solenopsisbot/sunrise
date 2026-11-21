use super::tests::{MOD_MENU, mod_menu_guest, sent};
use crate::game::{
    Game,
    api_values::{self, TryIntoApi},
    native_entity_refs::tests::with_game,
};
use mod_api::{
    Flow, Hand, Hooks, HostTransaction, ItemPlace, ItemResource, MenuClickedEvent, MenuClosedEvent,
    MenuContainer, MenuItemsRefusal, MenuKey, MenuPart, MenuRef, MenuReplace, MenuSlotBackRefusal,
    MenuSlotBacking, MenuSlotChangedEvent, MenuSlotPlace, MenuSlotWrite, MenuStillValidEvent,
    ModEntry, Mods, Registrar, ServerHost,
};
use std::sync::mpsc;
use sunrise_client::{items::ItemStack, lan::Out};
use sunrise_protocol::packets::play::{
    Clientbound as C, ContainerClick, ContainerClose, Serverbound as S, SetCursorItem,
};

thread_local! {
    static SCRIPTED_CLICK: std::cell::Cell<Option<Flow>> = const { std::cell::Cell::new(None) };
    static WINDOW_REQUEST: std::cell::Cell<Option<(MenuKey, bool)>> = const { std::cell::Cell::new(None) };
    static CLOSE_NOTICES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    static LOCK_FIRST_SLOT: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

struct Part;
#[mod_api::sunrise_part]
impl MenuPart for Part {
    fn screen(
        &self,
        _: &dyn mod_api::Host,
        _: &mut mod_api::ScreenEvent<'_>,
    ) -> Option<mod_api::Screen> {
        Some(mod_api::Screen {
            slots: vec![Some(mod_api::ScreenSlot::default()); 2],
            ..Default::default()
        })
    }

    fn menu_still_valid(&self, _: &dyn mod_api::LoopContext, event: &mut MenuStillValidEvent<'_>) {
        if let Some(item) = event.opening_item {
            assert_eq!(item.hand, Hand::Off);
            event.valid = item.stack_matches;
        }
    }
    fn menu_slot_rules(&self, _: &dyn mod_api::Host, event: &mut mod_api::MenuSlotRulesEvent<'_>) {
        if LOCK_FIRST_SLOT.get() && event.slot == 0 {
            assert_eq!(event.context.player, Some(123));
            assert!(
                event
                    .context
                    .slot(mod_api::MenuContainer::Main, 0)
                    .is_some()
            );
            event.may_pickup = false;
            event.may_place = false;
        }
    }

    fn menu_clicked(&self, host: &mut dyn ServerHost, event: &mut MenuClickedEvent<'_>) -> Flow {
        if let Some((key, by_handle)) = WINDOW_REQUEST.get() {
            let title = mod_api::Text::literal("Would replace the current menu");
            assert_eq!(
                host.open_menu(
                    event.uuid,
                    1,
                    title.clone(),
                    Vec::new(),
                    MenuReplace::InPlace
                ),
                Ok(None)
            );
            let player = host.player(event.uuid).unwrap();
            assert_eq!(
                player.open_mod_menu(host, mod_api::ModMenuOpen::new(key, title)),
                Err(mod_api::OpenMenuError::ClickInProgress)
            );
            if by_handle {
                let menu = player.container_menu(host).unwrap();
                assert!(menu.close_container(host));
            } else {
                host.close_menu(event.uuid);
            }
            assert!(
                host.menu(event.uuid, event.window).is_some(),
                "close is deferred"
            );
            return Flow::Cancel;
        }
        if let Some(flow) = SCRIPTED_CLICK.get() {
            return flow;
        }
        if event.slot != 1 {
            return Flow::Continue;
        }
        let Some(menu) = host.menu(event.uuid, event.window) else {
            return Flow::Continue;
        };
        let Some(cursor) = menu.carried(host).flatten() else {
            return Flow::Continue;
        };
        host.transaction(|host, tx| {
            ItemPlace::MenuSlot { menu, slot: 1 }.insert(host, tx, &ItemResource::of(&cursor), 1);
            Ok::<_, ()>(())
        })
        .unwrap();
        Flow::Cancel
    }
}

struct Listener {
    return_contents: bool,
}

#[mod_api::__subscriptions]
impl Hooks for Listener {
    fn register(&self, r: &mut dyn Registrar, _: &mod_api::RegistrationContext) {
        r.menu_part(MOD_MENU, Part);
    }

    fn menu_closed(&self, host: &mut dyn ServerHost, event: &mut MenuClosedEvent<'_>) {
        CLOSE_NOTICES.set(CLOSE_NOTICES.get() + 1);
        if !self.return_contents {
            return;
        }
        assert_eq!(event.opening_hand, Some(Hand::Off));
        assert!(host.menu(event.uuid, event.window).is_none());
        let player = host.player(event.uuid).unwrap();
        host.transaction(|host, _| {
            for stack in event.owned_slots.iter().flatten() {
                assert!(player.give_stack(host, stack.clone()).unwrap().is_none());
            }
            Ok::<_, ()>(())
        })
        .unwrap();
    }
}

fn with_owned_menu(
    return_contents: bool,
    test: impl FnOnce(&mut Game<'_>, mpsc::Receiver<Out>, MenuKey),
) {
    with_owned_layout(return_contents, false, test);
}

fn with_owned_layout(
    return_contents: bool,
    player_inventory: bool,
    test: impl FnOnce(&mut Game<'_>, mpsc::Receiver<Out>, MenuKey),
) {
    let mods: Mods = vec![ModEntry::new(
        "listen",
        Box::new(Listener { return_contents }),
    )]
    .into();
    mod_platform::testing::with_mods(mods.clone(), || {
        api_values::install_parts(&mods).unwrap();
        let _callbacks = sunrise_client::native_client_hooks::scoped_menu_validity_hooks(
            api_values::menu_still_valid,
            |menu| mod_api::__private::menu_listens(mod_api::HookId::MenuStillValid, menu),
        );
        sunrise_client::containers::with_mod_menu_types(
            [(
                MOD_MENU.into(),
                "listen".into(),
                2,
                0,
                Vec::new(),
                Vec::new(),
                None,
                None,
                player_inventory,
                Some(vec![1, 4]),
            )],
            || {
                with_game(|game| {
                    let (rx, key) = mod_menu_guest(game);
                    game.lan.as_mut().unwrap().hub.guests.update(0, |g| {
                        g.sender.menu_extra_rev = sunrise_protocol::menu_extra::LIVE_DATA_REVISION;
                        g.offhand = Some(ItemStack::new("minecraft:paper", 2));
                    });
                    test(game, rx, key);
                });
            },
        );
    });
}

fn open(game: &mut Game<'_>, key: MenuKey) -> (i32, MenuRef) {
    let player = game.player(123).unwrap();
    // Open from the acting player's item action, when the hand lives in the game,
    // not in the guest's record.
    let window = game
        .as_guest(1, |game| {
            player.open_mod_menu(game, {
                let mut request =
                    mod_api::ModMenuOpen::new(key, mod_api::Text::literal("Owned slots"));
                request.opening_hand = Some(Hand::Off);
                request
            })
        })
        .unwrap()
        .unwrap()
        .unwrap();
    (window, game.player_container_menu(player).unwrap())
}

fn click(game: &mut Game<'_>, container: i32, slot: i16) {
    game.lan_guest_packet(
        1,
        S::ContainerClick(ContainerClick {
            container,
            state: 0,
            slot,
            button: 0,
            input: 0,
            changed: Vec::new(),
            carried: None,
        }),
    );
    game.run_native_menu_events();
}

#[test]
#[ignore = "daily"]
fn an_item_opened_menu_has_two_empty_owned_slots_with_individual_caps() {
    with_owned_menu(false, |game, rx, key| {
        let (window, menu) = open(game, key);
        let packets = sent(&rx);
        assert!(packets.iter().any(|p| matches!(p,
            C::ContainerSetContent(content) if content.container == window
                && content.items == vec![None, None]
        )));
        assert_eq!(menu.slot_count(game), Some(2));
        let player = game.player(123).unwrap();
        assert_eq!(
            player.back_mod_menu_slots(
                game,
                window,
                vec![MenuSlotPlace::HeldItem {
                    hand: Hand::Off,
                    slot: 0
                }],
            ),
            Err(MenuSlotBackRefusal::UnsupportedMenu),
        );
        let stick =
            mod_api::ItemStack::of(mod_api::__private::item_key("minecraft:stick").unwrap());
        let resource = ItemResource::of(&stick);
        for (slot, cap) in [1, 4].into_iter().enumerate() {
            assert_eq!(menu.slot(game, slot), Some(None));
            assert_eq!(
                menu.slot_backing(game, slot),
                Some(MenuSlotBacking::Owned(slot))
            );
            assert_eq!(menu.slot_holds(game, slot, &stick), Some(cap));
            let host: &mut dyn ServerHost = game;
            host.transaction(|host, tx| {
                let place = ItemPlace::MenuSlot { menu, slot };
                assert_eq!(place.insert(host, tx, &resource, 8), cap);
                assert_eq!(place.stack(host).unwrap().count(), cap);
                Ok::<_, ()>(())
            })
            .unwrap();
        }
        let host: &mut dyn ServerHost = game;
        assert_eq!(
            host.menu_set_items(123, window, vec![]),
            Err(MenuItemsRefusal::OwnedSlots)
        );
        assert_eq!(menu.slot(host, 0), Some(Some(stick)));
    });
}

#[test]
#[ignore = "daily"]
fn opening_an_owned_menu_with_crafting_keeps_the_old_window() {
    with_owned_menu(false, |game, rx, key| {
        let (_, menu) = open(game, key);
        sent(&rx);
        // Bypass package validation to exercise the binding boundary itself: owned_slots + crafting
        // is still refused, while owned_slots + results opens cleanly.
        sunrise_client::containers::with_mod_menu_types(
            [(
                MOD_MENU.into(),
                "listen".into(),
                2,
                0,
                vec![],
                vec![],
                Some(sunrise_client::containers::ModCrafting {
                    grid: 0,
                    size: 1,
                    result: 1,
                }),
                None,
                false,
                Some(vec![1, 4]),
            )],
            || {
                let player = game.player(123).unwrap();
                assert_eq!(
                    player.open_mod_menu(
                        game,
                        mod_api::ModMenuOpen::new(key, mod_api::Text::literal("Invalid crafting")),
                    ),
                    Err(mod_api::OpenMenuError::OwnedSlotsWithCrafting)
                );
                assert_eq!(game.player_container_menu(player), Some(menu));
                assert!(sent(&rx).is_empty());
            },
        );
        sunrise_client::containers::with_mod_menu_types(
            [(
                MOD_MENU.into(),
                "listen".into(),
                2,
                0,
                vec![1],
                vec![0],
                None,
                None,
                true,
                Some(vec![16, 4]),
            )],
            || {
                let player = game.player(123).unwrap();
                let opened = player
                    .open_mod_menu(
                        game,
                        mod_api::ModMenuOpen::new(key, mod_api::Text::literal("Valid results")),
                    )
                    .unwrap();
                assert!(opened.is_some());
                assert_ne!(game.player_container_menu(player), Some(menu));
            },
        );
    });
}

#[test]
#[ignore = "daily"]
fn a_cursor_cleared_outside_a_click_is_sent_once_on_the_next_broadcast() {
    with_owned_menu(false, |game, rx, key| {
        game.lan.as_mut().unwrap().hub.guests.update(0, |guest| {
            guest.carried = Some(ItemStack::new("minecraft:stick", 2));
        });
        let (window, menu) = open(game, key);
        sent(&rx);
        let cursor = menu.cursor();
        let stick = ItemStack::new("minecraft:stick", 1).try_into_api().unwrap();
        let resource = ItemResource::of(&stick);
        game.transaction(|host, tx| {
            assert_eq!(cursor.insert(host, tx, &resource, 3), 3);
            Ok::<_, ()>(())
        })
        .unwrap();

        // Establish what the client sees through a full menu sync, not a click prediction.
        let hub = &mut game.lan.as_mut().unwrap().hub;
        hub.resend_menu(1, window, &game.world);
        assert!(sent(&rx).iter().any(|p| matches!(p,
            C::ContainerSetContent(content) if content.container == window
                && content.carried.as_ref().map(|item| item.count) == Some(3)
        )));
        hub.sync_containers(&game.world);
        assert!(
            sent(&rx).is_empty(),
            "the full sync already sent the cursor"
        );

        game.transaction(|host, tx| {
            assert_eq!(cursor.extract(host, tx, &resource, 3), 3);
            Ok::<_, ()>(())
        })
        .unwrap();
        assert!(sent(&rx).is_empty(), "nothing before the broadcast");

        let hub = &mut game.lan.as_mut().unwrap().hub;
        // A repair of window 0 does not tell the open window about the clear.
        hub.send_inventory(0);
        assert!(matches!(&sent(&rx)[..], [C::ContainerSetContent(content)]
            if content.container == 0 && content.carried.as_ref().map(|item| item.count) == Some(2)));
        hub.sync_containers(&game.world);
        let packets = sent(&rx);
        assert!(
            matches!(
                &packets[..],
                [C::SetCursorItem(SetCursorItem { item: None })]
            ),
            "{packets:?}"
        );
        hub.sync_containers(&game.world);
        assert!(sent(&rx).is_empty(), "the empty cursor was already sent");
    });
}

#[test]
#[ignore = "daily"]
fn owned_click_prediction_applies_caps_and_the_server_corrects_an_override() {
    use crate::game::execution_role::ExecutionRole;
    use sunrise_client::menu::Input;
    use sunrise_protocol::item::Hashed;
    with_owned_menu(false, |game, rx, key| {
        let (window, menu) = open(game, key);
        let opening = sent(&rx);
        let state = opening
            .iter()
            .find_map(|p| match p {
                C::ContainerSetContent(p) if p.container == window => Some(p.state),
                _ => None,
            })
            .unwrap();
        let stack = |count| Some(ItemStack::new("minecraft:stick", count));
        let hub = &mut game.lan.as_mut().unwrap().hub;
        let builtin = hub.sync_data().builtin.clone();
        let network = [1, 3, 4, 6, 7].map(|count| hub.stack(&stack(count)).unwrap());
        let [one, three, four, six, seven] = &network;
        let prediction = |slot, placed, left| {
            S::ContainerClick(ContainerClick {
                container: window,
                state,
                slot,
                button: 0,
                input: 0,
                changed: vec![(slot, Some(Hashed::of(placed).unwrap()))],
                carried: Some(Hashed::of(left).unwrap()),
            })
        };
        let set_server_cursor = |game: &mut Game<'_>| {
            game.lan
                .as_mut()
                .unwrap()
                .hub
                .guests
                .update(0, |g| g.carried = stack(7));
        };
        set_server_cursor(game);
        game.lan_guest_packet(1, prediction(0, one, six));
        assert_eq!(
            menu.slot(game, 0),
            Some(Some(stack(1).unwrap().try_into_api().unwrap()))
        );
        assert_eq!(
            menu.carried(game),
            Some(Some(stack(6).unwrap().try_into_api().unwrap()))
        );
        assert!(
            sent(&rx).is_empty(),
            "the server agreed with the ordinary click"
        );

        game.lan_guest_packet(
            1,
            S::ContainerClick(ContainerClick {
                container: window,
                state,
                slot: 0,
                button: 0,
                input: sunrise_client::lan::menu::input::QUICK_MOVE,
                changed: Vec::new(),
                carried: Some(Hashed::of(six).unwrap()),
            }),
        );
        assert_eq!(
            menu.slot(game, 0),
            Some(Some(stack(1).unwrap().try_into_api().unwrap())),
            "shift-click must not move an item to an omitted player inventory"
        );
        assert!(sent(&rx).is_empty());

        set_server_cursor(game);
        game.lan_guest_packet(1, prediction(1, four, three));
        assert_eq!(
            menu.slot(game, 1),
            Some(Some(stack(1).unwrap().try_into_api().unwrap()))
        );
        assert_eq!(
            menu.carried(game),
            Some(Some(stack(7).unwrap().try_into_api().unwrap()))
        );
        let corrections = sent(&rx);
        let corrected_state = corrections
            .iter()
            .find_map(|p| match p {
                C::ContainerSetContent(p) if p.container == window => {
                    assert_eq!(p.items, [Some(one.clone()), Some(one.clone())]);
                    assert_eq!(p.carried.as_ref(), Some(seven));
                    Some(p.state)
                }
                _ => None,
            })
            .unwrap();

        // The server's copy and cursor now differ only in components from what the
        // packet claims. Matching item IDs and counts is not enough to skip correction.
        let mut named = stack(7).unwrap();
        named.data_mut().custom_name = Some(sunrise_core::nbt::Tag::String("Filter".into()));
        let hub = &mut game.lan.as_mut().unwrap().hub;
        hub.guests.update(0, |g| {
            g.native_menu[1] = None;
            g.carried = Some(named.clone());
        });
        let named_one = ItemStack {
            count: 1,
            ..named.clone()
        };
        let named_one_net = hub.stack(&Some(named_one.clone()));
        let named_seven_net = hub.stack(&Some(named.clone()));
        game.lan_guest_packet(
            1,
            S::ContainerClick(ContainerClick {
                container: window,
                state: corrected_state,
                slot: 1,
                button: 0,
                input: 0,
                changed: vec![(1, Some(Hashed::of(one).unwrap()))],
                carried: Some(Hashed::of(seven).unwrap()),
            }),
        );
        let component_correction = sent(&rx);
        let state = component_correction
            .iter()
            .find_map(|p| match p {
                C::ContainerSetContent(p) if p.container == window => {
                    assert_eq!(p.items[1], named_one_net);
                    assert_eq!(p.carried, named_seven_net);
                    Some(p.state)
                }
                _ => None,
            })
            .unwrap();

        // No player slots means this quick move does nothing. The prediction has the correct
        // cursor, but claims the slot kept a named stick instead of an ordinary one.
        game.lan_guest_packet(
            1,
            S::ContainerClick(ContainerClick {
                container: window,
                state,
                slot: 0,
                button: 0,
                input: sunrise_client::lan::menu::input::QUICK_MOVE,
                changed: vec![(0, named_one_net.as_ref().map(|s| Hashed::of(s).unwrap()))],
                carried: named_seven_net.as_ref().map(|s| Hashed::of(s).unwrap()),
            }),
        );
        let slot_correction = sent(&rx);
        assert!(matches!(&slot_correction[..], [C::ContainerSetSlot(p)]
            if p.container == window && p.slot == 0 && p.item.as_ref() == Some(one)
        ));

        // Run the real client click path and apply those authoritative packets. Both sides
        // know the declared caps; only the server executes the part's override.
        game.lan = None;
        game.options.execution_role = ExecutionRole::Client;
        let _peer = crate::game::native_client_session::tests::logged_client(game);
        game.remote.as_mut().unwrap().builtin = std::sync::Arc::new(builtin);
        for packet in opening {
            game.handle_packet(packet).unwrap();
        }
        assert_eq!(game.remote_menu_type(), Some(MOD_MENU));
        assert_eq!(game.slots(), [300, 301]);
        let cursor = || {
            C::SetCursorItem(SetCursorItem {
                item: Some(seven.clone()),
            })
        };
        game.handle_packet(cursor()).unwrap();
        assert!(game.menu_ops(&[(Some(300), 0, Input::Pickup)]));
        assert_eq!(*game.slot_mut(300), stack(1));
        assert_eq!(game.carried, stack(6));
        assert!(game.menu_ops(&[(Some(300), 0, Input::QuickMove)]));
        assert_eq!(*game.slot_mut(300), stack(1));

        game.handle_packet(cursor()).unwrap();
        assert!(game.menu_ops(&[(Some(301), 0, Input::Pickup)]));
        assert_eq!(*game.slot_mut(301), stack(4));
        assert_eq!(game.carried, stack(3));
        for packet in corrections {
            game.handle_packet(packet).unwrap();
        }
        assert_eq!(*game.slot_mut(300), stack(1));
        assert_eq!(*game.slot_mut(301), stack(1));
        assert_eq!(game.carried, stack(7));
        for packet in component_correction {
            game.handle_packet(packet).unwrap();
        }
        assert_eq!(*game.slot_mut(301), Some(named_one));
        assert_eq!(game.carried, Some(named));
    });
}

#[test]
#[ignore = "daily"]
fn closing_discards_owned_slots_unless_the_listener_returns_their_contents() {
    for return_contents in [false, true] {
        with_owned_menu(return_contents, |game, _rx, key| {
            let (window, menu) = open(game, key);
            let stick =
                mod_api::ItemStack::of(mod_api::__private::item_key("minecraft:stick").unwrap());
            let host: &mut dyn ServerHost = game;
            host.transaction(|host, tx| {
                assert_eq!(
                    ItemPlace::MenuSlot { menu, slot: 1 }.insert(
                        host,
                        tx,
                        &ItemResource::of(&stick),
                        3
                    ),
                    3
                );
                Ok::<_, ()>(())
            })
            .unwrap();
            game.lan_guest_packet(1, S::ContainerClose(ContainerClose { container: window }));
            game.run_native_menu_events();
            assert_eq!(menu.slot(game, 1), None);
            let guest = &game.lan.as_ref().unwrap().hub.guests[0];
            assert_eq!(
                guest
                    .inventory
                    .iter()
                    .flatten()
                    .cloned()
                    .collect::<Vec<_>>(),
                if return_contents {
                    vec![ItemStack::new("minecraft:stick", 3)]
                } else {
                    vec![]
                }
            );
            assert!(game.dropped.is_empty());
            let (_, reopened) = open(game, key);
            assert_eq!(reopened.slot(game, 1), Some(None));
        });
    }
}

#[test]
#[ignore = "daily"]
fn abort_after_replacement_restores_only_committed_contents_to_the_old_close() {
    with_owned_menu(true, |game, _rx, key| {
        let (_, old) = open(game, key);
        let player = game.player(123).unwrap();
        let stick =
            mod_api::ItemStack::of(mod_api::__private::item_key("minecraft:stick").unwrap());
        let resource = ItemResource::of(&stick);
        let old_slot = ItemPlace::MenuSlot { menu: old, slot: 1 };
        let host: &mut dyn ServerHost = game;
        host.transaction(|host, tx| {
            assert_eq!(old_slot.insert(host, tx, &resource, 2), 2);
            Ok::<_, ()>(())
        })
        .unwrap();

        let aborted: Result<(), ()> = host.transaction(|host, tx| {
            assert_eq!(old_slot.insert(host, tx, &resource, 1), 1);
            player
                .open_mod_menu(host, {
                    let mut request =
                        mod_api::ModMenuOpen::new(key, mod_api::Text::literal("Replacement"));
                    request.opening_hand = Some(Hand::Off);
                    request.replace = MenuReplace::InPlace;
                    request
                })
                .unwrap()
                .unwrap();
            let new = player.container_menu(host).unwrap();
            let new_slot = ItemPlace::MenuSlot { menu: new, slot: 1 };
            assert_eq!(new_slot.insert(host, tx, &resource, 4), 4);
            Err(())
        });
        assert_eq!(aborted, Err(()));
        let new = game.player_container_menu(player).unwrap();
        assert_eq!(old.slot(game, 1), None);
        assert_eq!(new.slot(game, 1), Some(None), "no leakage between windows");
        game.run_native_menu_events();
        let guest = &game.lan.as_ref().unwrap().hub.guests[0];
        assert_eq!(
            guest
                .inventory
                .iter()
                .flatten()
                .cloned()
                .collect::<Vec<_>>(),
            [ItemStack::new("minecraft:stick", 2)],
            "the old close returned only the committed value"
        );
    });
}

#[test]
#[ignore = "daily"]
fn the_part_closes_the_menu_when_the_opening_stack_leaves_its_hand() {
    with_owned_menu(false, |game, rx, key| {
        let (window, menu) = open(game, key);
        let change_hand = |game: &mut Game<'_>, change: fn(&mut ItemStack)| {
            game.lan.as_mut().unwrap().hub.guests.update(0, |g| {
                change(g.offhand.as_mut().unwrap());
            });
        };
        change_hand(game, |s| s.count = 3);
        click(game, window, 1);
        assert_eq!(
            menu.container_id(game),
            Some(window),
            "count does not affect the comparison"
        );

        // Same item, different components: the opening stack is no longer held.
        change_hand(game, |s| {
            s.data_mut().custom_name = Some(sunrise_core::nbt::Tag::String("Different".into()));
        });
        click(game, window, 1);
        assert_eq!(menu.container_id(game), None);
        assert!(sent(&rx).iter().any(|p| matches!(p,
            C::ContainerClose(p) if p.container == window
        )));

        let (window, menu) = open(game, key);
        game.lan
            .as_mut()
            .unwrap()
            .hub
            .guests
            .update(0, |g| g.offhand = None);
        click(game, window, 1);
        assert_eq!(menu.container_id(game), None);
        assert!(sent(&rx).iter().any(|p| matches!(p,
            C::ContainerClose(p) if p.container == window
        )));
    });
}

#[test]
#[ignore = "daily"]
fn scoped_owned_clicks_replace_or_continue_each_input_mode() {
    use sunrise_client::lan::menu::input::*;
    with_owned_layout(false, true, |game, rx, key| {
        let stone = |count| Some(ItemStack::new("minecraft:stone", count));
        let dirt = Some(ItemStack::new("minecraft:dirt", 1));
        let player = game.player(123).unwrap();
        let mut cases: Vec<_> = (0..=8)
            .chain([40])
            .map(|button| (SWAP, button, 0))
            .collect();
        cases.extend([
            (CLONE, 2, 0),
            (THROW, 0, 0),
            (QUICK_MOVE, 0, 0),
            (PICKUP_ALL, 0, 0),
            (PICKUP, 0, -999),
        ]);
        for (input, button, slot) in cases {
            for flow in [Flow::Cancel, Flow::Continue] {
                let window = player
                    .open_mod_menu(
                        game,
                        mod_api::ModMenuOpen::new(key, mod_api::Text::literal("Click matrix")),
                    )
                    .unwrap()
                    .unwrap();
                let hub = &mut game.lan.as_mut().unwrap().hub;
                hub.guests.update(0, |g| {
                    g.game_mode = i32::from(input == CLONE);
                    g.inventory.fill(None);
                    g.offhand = (button == 40).then(|| dirt.clone()).flatten();
                    if input == SWAP && button <= 8 {
                        g.inventory[button as usize] = dirt.clone();
                    }
                    g.native_menu[0] = (!matches!(input, PICKUP_ALL | PICKUP))
                        .then(|| stone(1))
                        .flatten();
                    g.native_menu[1] = (input == PICKUP_ALL).then(|| stone(2)).flatten();
                    g.carried = matches!(input, PICKUP_ALL | PICKUP)
                        .then(|| stone(2))
                        .flatten();
                });
                hub.resend_menu(1, window, &game.world);
                sent(&rx);
                let before = game.dropped.len();
                send_scoped_click(game, flow, window, slot, button, input);
                let packets = sent(&rx);
                let g = game.lan.as_ref().unwrap().hub.guest(1).unwrap();
                let expected = if matches!(flow, Flow::Cancel) {
                    assert!(
                        packets.iter().any(
                            |p| matches!(p, C::ContainerSetContent(p) if p.container == window)
                        ),
                        "{input}:{button}"
                    );
                    assert_eq!(
                        g.native_menu[0],
                        (!matches!(input, PICKUP_ALL | PICKUP))
                            .then(|| stone(1))
                            .flatten()
                    );
                    assert_eq!(
                        g.native_menu[1],
                        (input == PICKUP_ALL).then(|| stone(2)).flatten()
                    );
                    assert_eq!(
                        g.carried,
                        matches!(input, PICKUP_ALL | PICKUP)
                            .then(|| stone(2))
                            .flatten()
                    );
                    assert_eq!(game.dropped.len(), before);
                    dirt.clone()
                } else {
                    match input {
                        SWAP => assert_eq!(g.native_menu[0], dirt),
                        CLONE => {
                            assert_eq!(g.native_menu[0], stone(1));
                            assert_eq!(g.carried, stone(64));
                        }
                        THROW | PICKUP => {
                            assert_eq!(g.native_menu[0], None);
                            assert_eq!(g.carried, None);
                            assert_eq!(game.dropped.len(), before + 1);
                            assert_eq!(
                                game.dropped[before].stack.count,
                                if input == THROW { 1 } else { 2 }
                            );
                        }
                        QUICK_MOVE => {
                            assert_eq!(g.native_menu[0], None);
                            assert_eq!(g.inventory[8], stone(1));
                        }
                        PICKUP_ALL => {
                            assert_eq!(g.native_menu[1], None);
                            assert_eq!(g.carried, stone(4));
                        }
                        _ => unreachable!(),
                    }
                    stone(1)
                };
                if input == SWAP {
                    if button == 40 {
                        assert_eq!(g.offhand, expected);
                        assert_eq!(g.menu_sent[45], expected);
                        let expected = game.lan.as_mut().unwrap().hub.stack(&expected);
                        assert_eq!(packets.iter().filter(|p| matches!(p, C::SetPlayerInventory(p) if p.slot == 40 && p.item == expected)).count(), 1);
                    } else {
                        assert_eq!(g.inventory[button as usize], expected);
                    }
                }
            }
        }
    });
}

#[test]
#[ignore = "daily"]
fn a_replaced_or_aborted_drag_step_keeps_the_existing_drag() {
    use sunrise_client::lan::menu::input::QUICK_CRAFT;
    with_owned_menu(false, |game, rx, key| {
        let stack = |count| Some(ItemStack::new("minecraft:stone", count));
        let (window, _) = open(game, key);
        game.lan
            .as_mut()
            .unwrap()
            .hub
            .guests
            .update(0, |g| g.carried = stack(6));
        sent(&rx);
        for (slot, step) in [(-999, 0), (0, 1), (1, 1), (-999, 2)] {
            send_scoped_click(game, Flow::Cancel, window, slot, step, QUICK_CRAFT);
            assert!(
                sent(&rx)
                    .iter()
                    .any(|p| matches!(p, C::ContainerSetContent(p) if p.container == window))
            );
            let g = game.lan.as_ref().unwrap().hub.guest(1).unwrap();
            assert_eq!(g.native_menu, [None, None]);
            assert_eq!(g.carried, stack(6));
            // An enclosing abort must discard this step and its publications as well.
            let seal = game.transaction_open();
            send_scoped_click(game, Flow::Continue, window, slot, step, QUICK_CRAFT);
            assert!(sent(&rx).is_empty());
            game.transaction_close(&seal, false);
            assert!(sent(&rx).is_empty());
            send_scoped_click(game, Flow::Continue, window, slot, step, QUICK_CRAFT);
            sent(&rx);
        }
        let g = game.lan.as_ref().unwrap().hub.guest(1).unwrap();
        assert_eq!(g.native_menu, [stack(1), stack(3)]);
        assert_eq!(g.carried, stack(2));
    });
}

fn send_scoped_click(
    game: &mut Game<'_>,
    flow: Flow,
    window: i32,
    slot: i16,
    button: i8,
    input: i32,
) {
    let state = game
        .lan
        .as_ref()
        .unwrap()
        .hub
        .guest(1)
        .unwrap()
        .menu
        .as_ref()
        .unwrap()
        .state;
    SCRIPTED_CLICK.set(Some(flow));
    game.lan_guest_packet(
        1,
        S::ContainerClick(ContainerClick {
            container: window,
            state,
            slot,
            button,
            input,
            changed: Vec::new(),
            carried: None,
        }),
    );
    SCRIPTED_CLICK.set(None);
}

#[test]
#[ignore = "daily"]
fn a_click_refuses_opens_and_closes_only_on_commit() {
    with_owned_menu(true, |game, rx, key| {
        for by_handle in [false, true] {
            let (window, menu) = open(game, key);
            let stored = ItemStack::new("minecraft:stick", 3);
            let carried = ItemStack::new("minecraft:dirt", 2);
            let hub = &mut game.lan.as_mut().unwrap().hub;
            hub.guests.update(0, |g| {
                g.inventory.fill(None);
                g.native_menu[1] = Some(stored.clone());
                g.carried = Some(carried.clone());
            });
            hub.resend_menu(1, window, &game.world);
            sent(&rx);
            CLOSE_NOTICES.set(0);
            WINDOW_REQUEST.set(Some((key, by_handle)));

            let seal = game.transaction_open();
            click(game, window, 0);
            assert_eq!(game.menu(123, window), Some(menu));
            assert!(sent(&rx).is_empty());
            game.transaction_close(&seal, false);
            game.run_native_menu_events();
            assert_eq!(game.menu(123, window), Some(menu));
            let guest = game.lan.as_ref().unwrap().hub.guest(1).unwrap();
            assert_eq!(guest.native_menu, [None, Some(stored.clone())]);
            assert_eq!(guest.carried, Some(carried.clone()));
            assert!(guest.inventory.iter().all(Option::is_none));
            assert!(sent(&rx).is_empty());
            assert_eq!(CLOSE_NOTICES.get(), 0);

            click(game, window, 0);
            WINDOW_REQUEST.set(None);
            assert!(game.menu(123, window).is_none());
            let guest = game.lan.as_ref().unwrap().hub.guest(1).unwrap();
            assert!(guest.carried.is_none());
            assert!(guest.native_menu.is_empty());
            assert_eq!(
                guest.inventory.iter().flatten().collect::<Vec<_>>(),
                [&carried, &stored],
                "the close returned the cursor and detached owned inputs once"
            );
            assert_eq!(CLOSE_NOTICES.get(), 1);
            let packets = sent(&rx);
            assert_eq!(
                packets
                    .iter()
                    .filter(|p| matches!(p, C::ContainerClose(p) if p.container == window))
                    .count(),
                1
            );
            assert!(!packets.iter().any(|p| matches!(p, C::OpenScreen(_))));
        }
    });
}

#[test]
#[ignore = "daily"]
fn owned_slot_queries_use_the_rule_a_click_will_apply() {
    with_owned_menu(false, |game, _rx, key| {
        let (window, menu) = open(game, key);
        let stored = ItemStack::new("minecraft:stick", 1);
        game.lan
            .as_mut()
            .unwrap()
            .hub
            .guests
            .update(0, |g| g.native_menu[0] = Some(stored.clone()));
        LOCK_FIRST_SLOT.set(true);
        assert_eq!(
            menu.slot_holds(game, 0, &stored.clone().try_into_api().unwrap()),
            Some(0)
        );
        assert_eq!(menu.slot_gives(game, 0), Some(false));
        click(game, window, 0);
        LOCK_FIRST_SLOT.set(false);
        let guest = game.lan.as_ref().unwrap().hub.guest(1).unwrap();
        assert_eq!(guest.native_menu[0], Some(stored));
        assert!(guest.carried.is_none());
    });
}

#[derive(Clone, Copy)]
enum ChangeMode {
    Normal,
    OverCap,
    PlayerContainer,
    OutOfRange,
    Panic,
    WriteItemPlaceThenInvalid,
    WriteItemPlaceThenPanic,
}

#[derive(Clone, Copy)]
enum CancelClickMode {
    Continue,
    InsertSlot0Stick,
}

thread_local! {
    static CHANGE_MODE: std::cell::Cell<ChangeMode> = const { std::cell::Cell::new(ChangeMode::Normal) };
    static CANCEL_CLICK_MODE: std::cell::Cell<CancelClickMode> = const { std::cell::Cell::new(CancelClickMode::Continue) };
    static CHANGE_CALLS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    static LAST_CHANGED_SLOTS: std::cell::RefCell<Vec<usize>> = const { std::cell::RefCell::new(Vec::new()) };
    static CLOSED_OWNED_LEN: std::cell::Cell<Option<usize>> = const { std::cell::Cell::new(None) };
}

struct ProcessingPart;
#[mod_api::sunrise_part]
impl MenuPart for ProcessingPart {
    fn screen(
        &self,
        _: &dyn mod_api::Host,
        _: &mut mod_api::ScreenEvent<'_>,
    ) -> Option<mod_api::Screen> {
        Some(mod_api::Screen {
            slots: vec![Some(mod_api::ScreenSlot::default()); 3],
            ..Default::default()
        })
    }

    fn menu_clicked(&self, host: &mut dyn ServerHost, event: &mut MenuClickedEvent<'_>) -> Flow {
        match CANCEL_CLICK_MODE.get() {
            CancelClickMode::Continue => Flow::Continue,
            CancelClickMode::InsertSlot0Stick => {
                let tx = host.transaction_current().expect("click transaction");
                let player = host.player(event.uuid).unwrap();
                let menu = player.container_menu(host).unwrap();
                let stick = mod_api::ItemStack::new(
                    mod_api::__private::item_key("minecraft:stick").unwrap(),
                    1,
                )
                .unwrap();
                let inserted = ItemPlace::MenuSlot { menu, slot: 0 }.insert(
                    host,
                    &tx,
                    &ItemResource::of(&stick),
                    1,
                );
                assert_eq!(inserted, 1);
                Flow::Cancel
            }
        }
    }

    fn menu_slot_changed(&self, host: &mut dyn ServerHost, event: &mut MenuSlotChangedEvent<'_>) {
        assert_eq!(event.menu, MOD_MENU);
        assert_eq!(event.context.player, Some(123));
        assert_eq!(event.context.pos, None);
        CHANGE_CALLS.set(CHANGE_CALLS.get() + 1);
        LAST_CHANGED_SLOTS.with(|slots| {
            *slots.borrow_mut() = event.changed.iter().map(|c| c.slot).collect();
        });
        let stone = |count| {
            mod_api::ItemStack::new(
                mod_api::__private::item_key("minecraft:stone").unwrap(),
                count,
            )
            .unwrap()
        };
        let write_side_effects = |host: &mut dyn ServerHost| {
            let tx = host.transaction_current().expect("active click tx");
            let player = host.player(123).unwrap();
            let reward = mod_api::ItemStack::new(
                mod_api::__private::item_key("minecraft:apple").unwrap(),
                1,
            )
            .unwrap();
            assert_eq!(
                ItemPlace::PlayerSlot {
                    player: 123,
                    slot: 0,
                }
                .insert(host, &tx, &ItemResource::of(&reward), 1),
                1
            );
            let menu = player.container_menu(host).unwrap();
            assert_eq!(
                ItemPlace::MenuSlot { menu, slot: 1 }.insert(
                    host,
                    &tx,
                    &ItemResource::of(&reward),
                    1
                ),
                1
            );
        };
        match CHANGE_MODE.get() {
            ChangeMode::Normal => {
                let a = event.context.slot(MenuContainer::Main, 0);
                let b = event.context.slot(MenuContainer::Main, 1);
                let out = match (a, b) {
                    (Some(_), Some(_)) => Some(stone(2)),
                    _ => None,
                };
                event
                    .outputs
                    .push(MenuSlotWrite::new(MenuContainer::Main, 2, out));
            }
            ChangeMode::OverCap => {
                // Result slot 2 has cap 4.
                event
                    .outputs
                    .push(MenuSlotWrite::new(MenuContainer::Main, 2, Some(stone(5))));
            }
            ChangeMode::PlayerContainer => {
                event
                    .outputs
                    .push(MenuSlotWrite::new(MenuContainer::Player, 0, Some(stone(1))));
            }
            ChangeMode::OutOfRange => {
                event
                    .outputs
                    .push(MenuSlotWrite::new(MenuContainer::Main, 3, Some(stone(1))));
            }
            ChangeMode::Panic => panic!("menu_slot_changed failed"),
            ChangeMode::WriteItemPlaceThenInvalid => {
                write_side_effects(host);
                event
                    .outputs
                    .push(MenuSlotWrite::new(MenuContainer::Main, 2, Some(stone(5))));
            }
            ChangeMode::WriteItemPlaceThenPanic => {
                write_side_effects(host);
                panic!("menu_slot_changed failed after ItemPlace writes");
            }
        }
    }
}

struct ProcessingMod;
#[mod_api::__subscriptions]
impl Hooks for ProcessingMod {
    fn register(&self, r: &mut dyn Registrar, _: &mod_api::RegistrationContext) {
        r.menu_part(MOD_MENU, ProcessingPart);
    }

    fn menu_closed(&self, _: &mut dyn ServerHost, event: &mut MenuClosedEvent<'_>) {
        CLOSED_OWNED_LEN.set(Some(event.owned_slots.len()));
    }
}

fn with_processing_owned_menu(test: impl FnOnce(&mut Game<'_>, mpsc::Receiver<Out>, MenuKey)) {
    let mods: Mods = vec![ModEntry::new("listen", Box::new(ProcessingMod))].into();
    mod_platform::testing::with_mods(mods.clone(), || {
        api_values::install_parts(&mods).unwrap();
        sunrise_client::containers::with_mod_menu_types(
            [(
                MOD_MENU.into(),
                "listen".into(),
                3,
                0,
                vec![2],
                vec![0, 1],
                None,
                None,
                true,
                Some(vec![64, 64, 4]),
            )],
            || {
                with_game(|game| {
                    let (rx, key) = mod_menu_guest(game);
                    CHANGE_MODE.set(ChangeMode::Normal);
                    CANCEL_CLICK_MODE.set(CancelClickMode::Continue);
                    CHANGE_CALLS.set(0);
                    LAST_CHANGED_SLOTS.with(|slots| slots.borrow_mut().clear());
                    CLOSED_OWNED_LEN.set(None);
                    test(game, rx, key);
                });
            },
        );
    });
}

fn raw_click(game: &mut Game<'_>, conn: u32, window: i32, slot: i16, button: i8, input: i32) {
    let state = game
        .lan
        .as_ref()
        .unwrap()
        .hub
        .guest(conn)
        .unwrap()
        .menu
        .as_ref()
        .unwrap()
        .state;
    game.lan_guest_packet(
        conn,
        S::ContainerClick(ContainerClick {
            container: window,
            state,
            slot,
            button,
            input,
            changed: Vec::new(),
            carried: None,
        }),
    );
}

#[test]
#[ignore = "daily"]
fn processing_owned_menu_computes_results_via_slot_changed_and_rejects_invalid_writes() {
    with_processing_owned_menu(|game, rx, key| {
        let player = game.player(123).unwrap();
        let window = game
            .as_guest(1, |game| {
                let mut req = mod_api::ModMenuOpen::new(key, mod_api::Text::literal("Processor"));
                req.pos = Some([5, 64, 5]);
                player.open_mod_menu(game, req)
            })
            .unwrap()
            .unwrap()
            .unwrap();
        sent(&rx);

        let stick = |n| Some(ItemStack::new("minecraft:stick", n));
        let dirt = |n| Some(ItemStack::new("minecraft:dirt", n));
        let stone = |n| Some(ItemStack::new("minecraft:stone", n));

        // Put stick in slot 0: fires once, slot 2 stays empty.
        game.lan
            .as_mut()
            .unwrap()
            .hub
            .guests
            .update(0, |g| g.carried = stick(3));
        click(game, window, 0);
        assert_eq!(CHANGE_CALLS.get(), 1);
        assert_eq!(
            game.lan.as_ref().unwrap().hub.guest(1).unwrap().native_menu,
            [stick(3), None, None]
        );

        // Placing directly into result slot 2 is refused (and does not fire menu_slot_changed).
        game.lan
            .as_mut()
            .unwrap()
            .hub
            .guests
            .update(0, |g| g.carried = dirt(2));
        click(game, window, 2);
        assert_eq!(CHANGE_CALLS.get(), 1);
        assert_eq!(
            game.lan.as_ref().unwrap().hub.guest(1).unwrap().native_menu[2],
            None
        );

        // Put dirt in slot 1: fires once (not re-fired by result output), populates result slot 2.
        sent(&rx);
        click(game, window, 1);
        assert_eq!(
            CHANGE_CALLS.get(),
            2,
            "writing result slot via outputs must not re-fire menu_slot_changed"
        );
        let guest = game.lan.as_ref().unwrap().hub.guest(1).unwrap();
        assert_eq!(guest.native_menu, [stick(3), dirt(2), stone(2)]);
        assert_eq!(guest.carried, None);
        let packets = sent(&rx);
        assert!(packets.iter().any(|p| matches!(
            p,
            C::ContainerSetSlot(s) if s.container == window && s.slot == 2 && s.item.is_some()
        ) || matches!(
            p,
            C::ContainerSetContent(c) if c.container == window && c.items.get(2).is_some_and(Option::is_some)
        )));

        // Removing slot 0 clears result slot 2.
        click(game, window, 0);
        assert_eq!(CHANGE_CALLS.get(), 3);
        let guest = game.lan.as_ref().unwrap().hub.guest(1).unwrap();
        assert_eq!(guest.native_menu, [None, dirt(2), None]);
        assert_eq!(guest.carried, stick(3));

        // Invalid output writes and panics roll back the whole click and resend the menu.
        for bad in [
            ChangeMode::OverCap,
            ChangeMode::PlayerContainer,
            ChangeMode::OutOfRange,
            ChangeMode::Panic,
        ] {
            CHANGE_MODE.set(bad);
            sent(&rx);
            click(game, window, 0);
            let guest = game.lan.as_ref().unwrap().hub.guest(1).unwrap();
            assert_eq!(
                guest.native_menu,
                [None, dirt(2), None],
                "failed menu_slot_changed must roll back transient slots"
            );
            assert_eq!(
                guest.carried,
                stick(3),
                "failed menu_slot_changed must roll back cursor"
            );
            assert!(
                sent(&rx)
                    .iter()
                    .any(|p| matches!(p, C::ContainerSetContent(c) if c.container == window)),
                "aborted click must resend the menu"
            );
        }
    });
}

#[test]
#[ignore = "daily"]
fn owned_menu_slot_changed_fires_on_item_place_writes_and_deduplicates_per_transaction() {
    with_processing_owned_menu(|game, rx, key| {
        let player = game.player(123).unwrap();
        let window = game
            .as_guest(1, |game| {
                player.open_mod_menu(
                    game,
                    mod_api::ModMenuOpen::new(key, mod_api::Text::literal("Processor")),
                )
            })
            .unwrap()
            .unwrap()
            .unwrap();
        let menu = game.player_container_menu(player).unwrap();
        sent(&rx);

        let stick_api = ItemStack::new("minecraft:stick", 1).try_into_api().unwrap();
        let dirt_api = ItemStack::new("minecraft:dirt", 1).try_into_api().unwrap();
        let stick = |n| Some(ItemStack::new("minecraft:stick", n));
        let dirt = |n| Some(ItemStack::new("minecraft:dirt", n));
        let stone = |n| Some(ItemStack::new("minecraft:stone", n));

        // 1. Aborted transaction writing via ItemPlace::MenuSlot must never fire menu_slot_changed.
        let _: Result<(), ()> = game.transaction(|host, tx| {
            assert_eq!(
                ItemPlace::MenuSlot { menu, slot: 0 }.insert(
                    host,
                    tx,
                    &ItemResource::of(&stick_api),
                    2
                ),
                2
            );
            Err(())
        });
        assert_eq!(CHANGE_CALLS.get(), 0);
        assert!(sent(&rx).is_empty());

        // 2. Single committed transaction writing both input slots (slot 1 then slot 0) delivers
        // ONE deduplicated menu_slot_changed call with slots in ascending order [0, 1] and syncs
        // the viewer immediately without duplicate next-tick packets.
        game.transaction(|host, tx| {
            assert_eq!(
                ItemPlace::MenuSlot { menu, slot: 1 }.insert(
                    host,
                    tx,
                    &ItemResource::of(&dirt_api),
                    2
                ),
                2
            );
            assert_eq!(
                ItemPlace::MenuSlot { menu, slot: 0 }.insert(
                    host,
                    tx,
                    &ItemResource::of(&stick_api),
                    3
                ),
                3
            );
            Ok::<_, ()>(())
        })
        .unwrap();
        assert_eq!(CHANGE_CALLS.get(), 1);
        assert_eq!(LAST_CHANGED_SLOTS.with(|s| s.borrow().clone()), vec![0, 1]);
        let guest = game.lan.as_ref().unwrap().hub.guest(1).unwrap();
        assert_eq!(guest.native_menu, [stick(3), dirt(2), stone(2)]);
        let immediate = sent(&rx);
        assert!(
            immediate
                .iter()
                .any(|p| matches!(p, C::ContainerSetSlot(s) if s.container == window && s.slot == 2 && s.item.is_some())),
            "committed host write must sync result slot immediately"
        );
        game.lan.as_mut().unwrap().hub.sync_containers(&game.world);
        assert!(
            !sent(&rx)
                .iter()
                .any(|p| matches!(p, C::ContainerSetSlot(s) if s.container == window)),
            "next tick sync must not duplicate already-synced slot updates"
        );

        // 3. Clearing slot 0 through a menu_clicked Flow::Cancel override -> first empty slot 0,
        // then let menu_clicked insert stick into slot 0 and return Flow::Cancel.
        game.transaction(|host, tx| {
            assert_eq!(
                ItemPlace::MenuSlot { menu, slot: 0 }.extract(
                    host,
                    tx,
                    &ItemResource::of(&stick_api),
                    3
                ),
                3
            );
            Ok::<_, ()>(())
        })
        .unwrap();
        assert_eq!(CHANGE_CALLS.get(), 2);
        assert_eq!(
            game.lan.as_ref().unwrap().hub.guest(1).unwrap().native_menu,
            [None, dirt(2), None]
        );
        sent(&rx);

        CANCEL_CLICK_MODE.set(CancelClickMode::InsertSlot0Stick);
        click(game, window, 0);
        assert_eq!(
            CHANGE_CALLS.get(),
            3,
            "Flow::Cancel menu_clicked edit must fire menu_slot_changed once"
        );
        assert_eq!(
            game.lan.as_ref().unwrap().hub.guest(1).unwrap().native_menu,
            [stick(1), dirt(2), stone(2)]
        );

        // 4. Invalid output write inside menu_slot_changed during Flow::Cancel rolls back
        // menu_clicked's ItemPlace write too.
        game.transaction(|host, tx| {
            assert_eq!(
                ItemPlace::MenuSlot { menu, slot: 0 }.extract(
                    host,
                    tx,
                    &ItemResource::of(&stick_api),
                    1
                ),
                1
            );
            Ok::<_, ()>(())
        })
        .unwrap();
        CHANGE_MODE.set(ChangeMode::OverCap);
        click(game, window, 0);
        assert_eq!(
            game.lan.as_ref().unwrap().hub.guest(1).unwrap().native_menu,
            [None, dirt(2), None],
            "failed menu_slot_changed must roll back Flow::Cancel menu_clicked edits"
        );
    });
}

#[test]
#[ignore = "daily"]
fn owned_menu_slot_changed_batches_multi_slot_drag_and_covers_swap_throw_pickup_all() {
    with_processing_owned_menu(|game, _rx, key| {
        let player = game.player(123).unwrap();
        let window = game
            .as_guest(1, |game| {
                player.open_mod_menu(
                    game,
                    mod_api::ModMenuOpen::new(key, mod_api::Text::literal("Processor")),
                )
            })
            .unwrap()
            .unwrap()
            .unwrap();
        let stick = |n| Some(ItemStack::new("minecraft:stick", n));
        let dirt = |n| Some(ItemStack::new("minecraft:dirt", n));
        let stone = |n| Some(ItemStack::new("minecraft:stone", n));

        // 1. Multi-slot drag across slots 0 and 1 (left drag: mode 5, start=0, add=1, end=2):
        // intermediate add steps do not fire; drag-end fires ONCE with [0, 1] in menu order.
        game.lan
            .as_mut()
            .unwrap()
            .hub
            .guests
            .update(0, |g| g.carried = stick(4));
        raw_click(game, 1, window, -999, 0, 5);
        raw_click(game, 1, window, 1, 1, 5);
        raw_click(game, 1, window, 0, 1, 5);
        assert_eq!(CHANGE_CALLS.get(), 0);
        raw_click(game, 1, window, -999, 2, 5);
        assert_eq!(CHANGE_CALLS.get(), 1);
        assert_eq!(LAST_CHANGED_SLOTS.with(|s| s.borrow().clone()), vec![0, 1]);
        assert_eq!(
            game.lan.as_ref().unwrap().hub.guest(1).unwrap().native_menu,
            [stick(2), stick(2), stone(2)]
        );

        // 2. Number-key swap (mode 2, button 0 -> hotbar 0 into slot 0).
        game.lan
            .as_mut()
            .unwrap()
            .hub
            .guests
            .update(0, |g| g.inventory[0] = dirt(3));
        raw_click(game, 1, window, 0, 0, 2);
        assert_eq!(CHANGE_CALLS.get(), 2);
        assert_eq!(LAST_CHANGED_SLOTS.with(|s| s.borrow().clone()), vec![0]);
        assert_eq!(
            game.lan.as_ref().unwrap().hub.guest(1).unwrap().native_menu,
            [dirt(3), stick(2), stone(2)]
        );

        // 3. Throw single item from slot 0 (mode 4, button 0).
        raw_click(game, 1, window, 0, 0, 4);
        assert_eq!(CHANGE_CALLS.get(), 3);
        assert_eq!(
            game.lan.as_ref().unwrap().hub.guest(1).unwrap().native_menu,
            [dirt(2), stick(2), stone(2)]
        );

        // 4. Pickup-all (mode 6) collecting dirt from slot 0 onto matching cursor -> clears slot 0
        // and clears result slot 2.
        game.lan
            .as_mut()
            .unwrap()
            .hub
            .guests
            .update(0, |g| g.carried = dirt(1));
        raw_click(game, 1, window, 0, 0, 6);
        assert_eq!(CHANGE_CALLS.get(), 4);
        assert_eq!(
            game.lan.as_ref().unwrap().hub.guest(1).unwrap().native_menu,
            [None, stick(2), None]
        );
        assert_eq!(
            game.lan.as_ref().unwrap().hub.guest(1).unwrap().carried,
            dirt(3)
        );
    });
}

#[test]
#[ignore = "daily"]
fn owned_menu_slot_changed_rolls_back_listener_item_place_writes_on_invalid_output_or_panic() {
    with_processing_owned_menu(|game, rx, key| {
        let player = game.player(123).unwrap();
        let window = game
            .as_guest(1, |game| {
                player.open_mod_menu(
                    game,
                    mod_api::ModMenuOpen::new(key, mod_api::Text::literal("Processor")),
                )
            })
            .unwrap()
            .unwrap()
            .unwrap();
        let stick = |n| Some(ItemStack::new("minecraft:stick", n));
        for bad in [
            ChangeMode::WriteItemPlaceThenInvalid,
            ChangeMode::WriteItemPlaceThenPanic,
        ] {
            game.lan.as_mut().unwrap().hub.guests.update(0, |g| {
                g.inventory.fill(None);
                g.carried = stick(3);
            });
            CHANGE_MODE.set(bad);
            sent(&rx);
            click(game, window, 0);
            let guest = game.lan.as_ref().unwrap().hub.guest(1).unwrap();
            assert_eq!(
                guest.native_menu,
                [None, None, None],
                "listener MenuSlot writes must roll back with the click"
            );
            assert!(
                guest.inventory.iter().all(Option::is_none),
                "listener PlayerInventory writes must roll back with the click"
            );
            assert_eq!(guest.carried, stick(3));
        }
    });
}

const BLOCK_PROC_MENU: &str = "minecraft:generic_9x3";
const BLOCK_PROC_POS: (i32, i32, i32) = (1, 64, 1);

thread_local! {
    static BLOCK_CHANGE_CALLS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    static BLOCK_LAST_SLOTS: std::cell::RefCell<Vec<usize>> = const { std::cell::RefCell::new(Vec::new()) };
}

struct BlockProcessingPart;
#[mod_api::sunrise_part]
impl MenuPart for BlockProcessingPart {
    fn menu_slot_changed(&self, _: &mut dyn ServerHost, event: &mut MenuSlotChangedEvent<'_>) {
        assert_eq!(event.menu, BLOCK_PROC_MENU);
        assert_eq!(
            event.context.pos,
            Some([BLOCK_PROC_POS.0, BLOCK_PROC_POS.1, BLOCK_PROC_POS.2])
        );
        BLOCK_CHANGE_CALLS.set(BLOCK_CHANGE_CALLS.get() + 1);
        BLOCK_LAST_SLOTS.with(|s| {
            *s.borrow_mut() = event.changed.iter().map(|c| c.slot).collect();
        });
        let a = event.context.slot(MenuContainer::Main, 0);
        let b = event.context.slot(MenuContainer::Main, 1);
        let out = match (a, b) {
            (Some(a), Some(_)) => Some(
                mod_api::ItemStack::new(
                    mod_api::__private::item_key("minecraft:stone").unwrap(),
                    a.count().min(64),
                )
                .unwrap(),
            ),
            _ => None,
        };
        event
            .outputs
            .push(MenuSlotWrite::new(MenuContainer::Main, 2, out));
    }
}

struct BlockProcessingMod;
#[mod_api::__subscriptions]
impl Hooks for BlockProcessingMod {
    fn register(&self, r: &mut dyn Registrar, _: &mod_api::RegistrationContext) {
        r.menu_part(BLOCK_PROC_MENU, BlockProcessingPart);
    }
}

#[test]
#[ignore = "daily"]
fn block_menu_slot_changed_fires_on_external_container_writes_and_syncs_lan_viewers() {
    use std::sync::{Arc, atomic::AtomicUsize};
    use sunrise_client::{
        chunk::Chunk,
        containers::{BlockEntity, ModBlockEntity, ModBlockEntityFields, ModContainer},
        lan::{PlayerSender, menu::Slots},
    };
    let mods: Mods = vec![ModEntry::new("listen", Box::new(BlockProcessingMod))].into();
    mod_platform::testing::with_mods(mods.clone(), || {
        api_values::install_parts(&mods).unwrap();
        sunrise_client::containers::with_mod_menu_types(
            [(
                BLOCK_PROC_MENU.into(),
                "listen".into(),
                3,
                0,
                vec![2],
                vec![0, 1],
                None,
                None,
                true,
                None,
            )],
            || {
                crate::game::native_guest_turn_tests::with_guests(|game| {
                    BLOCK_CHANGE_CALLS.set(0);
                    BLOCK_LAST_SLOTS.with(|s| s.borrow_mut().clear());

                    game.world
                        .chunks
                        .insert((0, 0), Arc::new(Chunk::empty(0, 0)));
                    let stone_state = game
                        .pack
                        .assets
                        .registry
                        .default_state("minecraft:stone")
                        .unwrap();
                    game.world
                        .set(
                            BLOCK_PROC_POS.0,
                            BLOCK_PROC_POS.1,
                            BLOCK_PROC_POS.2,
                            stone_state,
                        )
                        .unwrap();
                    let container = Arc::new(ModContainer {
                        slots: 3,
                        inputs: vec![0, 1],
                        ..Default::default()
                    });
                    game.world
                        .block_entities
                        .set_mod_containers([(stone_state.0, container)].into_iter().collect());
                    game.world.block_entities.insert(
                        BLOCK_PROC_POS,
                        BlockEntity::Mod(Box::new(ModBlockEntity::new(
                            "listen:proc".into(),
                            ModBlockEntityFields {
                                items: vec![None; 3],
                                ..Default::default()
                            },
                        ))),
                    );

                    // Attach observable packet channels to both LAN guests (12 and 13) and open
                    // the block-backed mod menu for both viewers.
                    let (tx12, rx12) = mpsc::channel();
                    let (tx13, rx13) = mpsc::channel();
                    let host = game.lan.as_mut().unwrap();
                    host.hub.guests.update(0, |g| {
                        g.sender = PlayerSender::new(tx12, Arc::new(AtomicUsize::new(0)));
                    });
                    host.hub.guests.update(1, |g| {
                        g.sender = PlayerSender::new(tx13, Arc::new(AtomicUsize::new(0)));
                    });
                    let title = sunrise_core::nbt::Tag::String("proc".into());
                    host.hub.open_container(
                        12,
                        BLOCK_PROC_MENU,
                        title.clone(),
                        BLOCK_PROC_POS,
                        Slots::Block(BLOCK_PROC_POS),
                        vec![None; 3],
                        None,
                    );
                    let w12 = host.hub.guest(12).unwrap().menu.as_ref().unwrap().window;
                    host.hub.open_container(
                        13,
                        BLOCK_PROC_MENU,
                        title,
                        BLOCK_PROC_POS,
                        Slots::Block(BLOCK_PROC_POS),
                        vec![None; 3],
                        None,
                    );
                    let w13 = host.hub.guest(13).unwrap().menu.as_ref().unwrap().window;
                    sent(&rx12);
                    sent(&rx13);

                    let level = game.level("minecraft:overworld").unwrap();
                    let entity = game
                        .block_entity(
                            level,
                            [BLOCK_PROC_POS.0, BLOCK_PROC_POS.1, BLOCK_PROC_POS.2],
                        )
                        .unwrap();
                    let stick_api = ItemStack::new("minecraft:stick", 1).try_into_api().unwrap();
                    let dirt_api = ItemStack::new("minecraft:dirt", 1).try_into_api().unwrap();

                    // 1. BlockEntityRef::set_item_slot on slot 0 -> fires once across both viewers,
                    // syncs both immediately, no duplicate packets on next tick.
                    game.transaction(|host, tx| {
                        assert_eq!(
                            entity.set_item_slot(host, tx, 0, Some(stick_api.clone())),
                            Ok(true)
                        );
                        Ok::<_, ()>(())
                    })
                    .unwrap();
                    assert_eq!(BLOCK_CHANGE_CALLS.get(), 1);
                    assert_eq!(BLOCK_LAST_SLOTS.with(|s| s.borrow().clone()), vec![0]);
                    assert!(
                        sent(&rx12)
                            .iter()
                            .any(|p| matches!(p, C::ContainerSetSlot(s) if s.container == w12 && s.slot == 0 && s.item.is_some()))
                    );
                    assert!(
                        sent(&rx13)
                            .iter()
                            .any(|p| matches!(p, C::ContainerSetSlot(s) if s.container == w13 && s.slot == 0 && s.item.is_some()))
                    );
                    game.lan.as_mut().unwrap().hub.sync_containers(&game.world);
                    assert!(
                        !sent(&rx12)
                            .iter()
                            .any(|p| matches!(p, C::ContainerSetSlot(s) if s.container == w12))
                    );
                    assert!(
                        !sent(&rx13)
                            .iter()
                            .any(|p| matches!(p, C::ContainerSetSlot(s) if s.container == w13))
                    );

                    // 2. ItemPlace::BlockEntity on slot 1 -> fires once, computes result slot 2 =
                    // stone(1), syncs result slot 2 to BOTH viewers immediately with no tick dupes.
                    game.transaction(|host, tx| {
                        assert_eq!(
                            ItemPlace::BlockEntity {
                                level,
                                pos: [BLOCK_PROC_POS.0, BLOCK_PROC_POS.1, BLOCK_PROC_POS.2],
                                side: None,
                                slot: 1,
                            }
                            .insert(
                                host,
                                tx,
                                &ItemResource::of(&dirt_api),
                                1
                            ),
                            1
                        );
                        Ok::<_, ()>(())
                    })
                    .unwrap();
                    assert_eq!(BLOCK_CHANGE_CALLS.get(), 2);
                    let BlockEntity::Mod(be) =
                        game.world.block_entities.get(&BLOCK_PROC_POS).unwrap()
                    else {
                        panic!("expected mod block entity");
                    };
                    assert_eq!(be.items[2], Some(ItemStack::new("minecraft:stone", 1)));
                    assert!(
                        sent(&rx12)
                            .iter()
                            .any(|p| matches!(p, C::ContainerSetSlot(s) if s.container == w12 && s.slot == 2 && s.item.is_some()))
                    );
                    assert!(
                        sent(&rx13)
                            .iter()
                            .any(|p| matches!(p, C::ContainerSetSlot(s) if s.container == w13 && s.slot == 2 && s.item.is_some()))
                    );
                    game.lan.as_mut().unwrap().hub.sync_containers(&game.world);
                    assert!(
                        !sent(&rx12)
                            .iter()
                            .any(|p| matches!(p, C::ContainerSetSlot(s) if s.container == w12))
                    );
                    assert!(
                        !sent(&rx13)
                            .iter()
                            .any(|p| matches!(p, C::ContainerSetSlot(s) if s.container == w13))
                    );

                    // 3. Hopper ejecting another stick into slot 0 (non-transactional external
                    // write) -> fires once, updates result slot 2 = stone(2), syncs both viewers
                    // immediately with no tick dupes.
                    let hopper_pos = (BLOCK_PROC_POS.0, BLOCK_PROC_POS.1 + 1, BLOCK_PROC_POS.2);
                    let hopper_state = game
                        .pack
                        .assets
                        .registry
                        .default_state("minecraft:hopper")
                        .unwrap();
                    game.world
                        .set(hopper_pos.0, hopper_pos.1, hopper_pos.2, hopper_state)
                        .unwrap();
                    game.world.block_entities.insert(
                        hopper_pos,
                        BlockEntity::Chest(vec![
                            Some(ItemStack::new("minecraft:stick", 1)),
                            None,
                            None,
                            None,
                            None,
                        ]),
                    );
                    game.hopper_try_move_items(hopper_pos);
                    assert_eq!(BLOCK_CHANGE_CALLS.get(), 3);
                    let BlockEntity::Mod(be) =
                        game.world.block_entities.get(&BLOCK_PROC_POS).unwrap()
                    else {
                        panic!("expected mod block entity");
                    };
                    assert_eq!(be.items[0], Some(ItemStack::new("minecraft:stick", 2)));
                    assert_eq!(be.items[2], Some(ItemStack::new("minecraft:stone", 2)));
                    assert!(
                        sent(&rx12)
                            .iter()
                            .any(|p| matches!(p, C::ContainerSetSlot(s) if s.container == w12 && s.slot == 2 && s.item.is_some()))
                    );
                    assert!(
                        sent(&rx13)
                            .iter()
                            .any(|p| matches!(p, C::ContainerSetSlot(s) if s.container == w13 && s.slot == 2 && s.item.is_some()))
                    );
                    game.lan.as_mut().unwrap().hub.sync_containers(&game.world);
                    assert!(
                        !sent(&rx12)
                            .iter()
                            .any(|p| matches!(p, C::ContainerSetSlot(s) if s.container == w12))
                    );
                    assert!(
                        !sent(&rx13)
                            .iter()
                            .any(|p| matches!(p, C::ContainerSetSlot(s) if s.container == w13))
                    );
                });
            },
        );
    });
}

const BLOCK_CRAFT_MENU: &str = "minecraft:generic_3x3";
const BLOCK_CRAFT_POS: (i32, i32, i32) = (2, 64, 2);

thread_local! {
    static CRAFT_CONTEXT_LOG: std::cell::RefCell<
        Vec<(Vec<mod_api::MenuSlotChange>, mod_api::MenuSlotContext)>,
    > = const { std::cell::RefCell::new(Vec::new()) };
}

struct BlockCraftObserverPart;
#[mod_api::sunrise_part]
impl MenuPart for BlockCraftObserverPart {
    fn menu_slot_changed(&self, _: &mut dyn ServerHost, event: &mut MenuSlotChangedEvent<'_>) {
        assert_eq!(event.menu, BLOCK_CRAFT_MENU);
        CRAFT_CONTEXT_LOG.with(|log| {
            log.borrow_mut()
                .push((event.changed.to_vec(), event.context.clone()));
        });
    }
}

struct BlockCraftObserverMod;
#[mod_api::__subscriptions]
impl Hooks for BlockCraftObserverMod {
    fn register(&self, r: &mut dyn Registrar, _: &mod_api::RegistrationContext) {
        r.menu_part(BLOCK_CRAFT_MENU, BlockCraftObserverPart);
    }
}

#[test]
#[ignore = "daily"]
fn block_menu_slot_changed_gives_identical_context_for_click_and_hopper_writes() {
    use std::sync::Arc;
    use sunrise_client::{
        chunk::Chunk,
        containers::{
            BlockEntity, ModBlockEntity, ModBlockEntityFields, ModContainer, ModCrafting,
        },
        lan::menu::Slots,
    };
    let mods: Mods = vec![ModEntry::new("listen", Box::new(BlockCraftObserverMod))].into();
    mod_platform::testing::with_mods(mods.clone(), || {
        api_values::install_parts(&mods).unwrap();
        sunrise_client::containers::with_mod_menu_types(
            [(
                BLOCK_CRAFT_MENU.into(),
                "listen".into(),
                6,
                0,
                Vec::new(),
                vec![5],
                Some(ModCrafting {
                    grid: 0,
                    size: 2,
                    result: 4,
                }),
                None,
                true,
                None,
            )],
            || {
                crate::game::native_guest_turn_tests::with_guests(|game| {
                    CRAFT_CONTEXT_LOG.with(|log| log.borrow_mut().clear());
                    game.world
                        .chunks
                        .insert((0, 0), Arc::new(Chunk::empty(0, 0)));
                    let stone_state = game
                        .pack
                        .assets
                        .registry
                        .default_state("minecraft:stone")
                        .unwrap();
                    game.world
                        .set(
                            BLOCK_CRAFT_POS.0,
                            BLOCK_CRAFT_POS.1,
                            BLOCK_CRAFT_POS.2,
                            stone_state,
                        )
                        .unwrap();
                    let container = Arc::new(ModContainer {
                        slots: 6,
                        inputs: vec![0, 1, 2, 3, 5],
                        ..Default::default()
                    });
                    game.world
                        .block_entities
                        .set_mod_containers([(stone_state.0, container)].into_iter().collect());
                    game.world.block_entities.insert(
                        BLOCK_CRAFT_POS,
                        BlockEntity::Mod(Box::new(ModBlockEntity::new(
                            "listen:crafter".into(),
                            ModBlockEntityFields {
                                items: vec![None; 6],
                                ..Default::default()
                            },
                        ))),
                    );
                    let host = game.lan.as_mut().unwrap();
                    host.hub.open_container(
                        12,
                        BLOCK_CRAFT_MENU,
                        sunrise_core::nbt::Tag::String("crafter".into()),
                        BLOCK_CRAFT_POS,
                        Slots::Block(BLOCK_CRAFT_POS),
                        vec![None; 6],
                        None,
                    );
                    let window = host.hub.guest(12).unwrap().menu.as_ref().unwrap().window;
                    let oak_log = ItemStack::new("minecraft:oak_log", 1);
                    let expected_log = oak_log.try_into_api().unwrap();
                    let expected_planks = ItemStack::new("minecraft:oak_planks", 4)
                        .try_into_api()
                        .unwrap();

                    // 1. Click places oak_log into crafting grid slot 0.
                    game.lan
                        .as_mut()
                        .unwrap()
                        .hub
                        .guests
                        .update(0, |g| g.carried = Some(oak_log.clone()));
                    raw_click(game, 12, window, 0, 0, 0);
                    let (click_changed, click_context) =
                        CRAFT_CONTEXT_LOG.with(|log| log.borrow_mut().pop().unwrap());
                    assert_eq!(click_changed.len(), 1);
                    assert_eq!(click_changed[0].container, MenuContainer::Grid);
                    assert_eq!(click_changed[0].slot, 0);
                    assert_eq!(
                        click_context.slot(MenuContainer::Grid, 0),
                        Some(&expected_log)
                    );
                    assert_eq!(
                        click_context.slot(MenuContainer::Result, 0),
                        Some(&expected_planks),
                        "click path exposes the live crafting result"
                    );

                    // 2. Click removes oak_log back out so the grid is empty again.
                    raw_click(game, 12, window, 0, 0, 0);
                    CRAFT_CONTEXT_LOG.with(|log| log.borrow_mut().clear());

                    // 3. Hopper ejects oak_log into slot 0 -> must produce the exact same
                    // changed list and live MenuSlotContext (including Result = 4 oak planks).
                    let hopper_pos = (BLOCK_CRAFT_POS.0, BLOCK_CRAFT_POS.1 + 1, BLOCK_CRAFT_POS.2);
                    let hopper_state = game
                        .pack
                        .assets
                        .registry
                        .default_state("minecraft:hopper")
                        .unwrap();
                    game.world
                        .set(hopper_pos.0, hopper_pos.1, hopper_pos.2, hopper_state)
                        .unwrap();
                    game.world.block_entities.insert(
                        hopper_pos,
                        BlockEntity::Chest(vec![Some(oak_log), None, None, None, None]),
                    );
                    game.hopper_try_move_items(hopper_pos);
                    let (hopper_changed, hopper_context) =
                        CRAFT_CONTEXT_LOG.with(|log| log.borrow_mut().pop().unwrap());
                    assert_eq!(hopper_changed, click_changed);
                    assert_eq!(
                        hopper_context, click_context,
                        "hopper write must present the exact same live MenuSlotContext as a click"
                    );
                });
            },
        );
    });
}

#[test]
#[ignore = "daily"]
fn processing_owned_menu_returns_inputs_on_close_drops_overflow_and_discards_results() {
    with_processing_owned_menu(|game, rx, key| {
        let player = game.player(123).unwrap();
        let stick = |n| Some(ItemStack::new("minecraft:stick", n));
        let dirt = |n| Some(ItemStack::new("minecraft:dirt", n));
        let stone = |n| Some(ItemStack::new("minecraft:stone", n));

        // 1. Live close with room in inventory: returns non-result inputs, discards result slot 2,
        // and delivers empty owned_slots to menu_closed so items cannot be duplicated.
        let window = game
            .as_guest(1, |game| {
                player.open_mod_menu(
                    game,
                    mod_api::ModMenuOpen::new(key, mod_api::Text::literal("Processor")),
                )
            })
            .unwrap()
            .unwrap()
            .unwrap();
        game.lan.as_mut().unwrap().hub.guests.update(0, |g| {
            g.inventory.fill(None);
            g.carried = None;
            g.native_menu = vec![stick(3), dirt(2), stone(2)];
        });
        game.dropped.clear();
        CLOSED_OWNED_LEN.set(None);
        sent(&rx);
        game.lan_guest_packet(
            1,
            S::ContainerClose(sunrise_protocol::packets::play::ContainerClose {
                container: window,
            }),
        );
        game.run_native_menu_events();
        assert_eq!(CLOSED_OWNED_LEN.get(), Some(0));
        let guest = game.lan.as_ref().unwrap().hub.guest(1).unwrap();
        let count_inv = |g: &sunrise_client::lan::guest::Guest, id: &str| -> u32 {
            g.inventory
                .iter()
                .flatten()
                .filter(|s| &*s.item == id)
                .map(|s| s.count)
                .sum()
        };
        assert_eq!(count_inv(guest, "minecraft:stick"), 3);
        assert_eq!(count_inv(guest, "minecraft:dirt"), 2);
        assert_eq!(
            count_inv(guest, "minecraft:stone"),
            0,
            "result slot must be discarded, never returned"
        );
        assert!(game.dropped.is_empty());

        // 2. Live close with full inventory: drops overflow inputs at player and discards results.
        let window = game
            .as_guest(1, |game| {
                player.open_mod_menu(
                    game,
                    mod_api::ModMenuOpen::new(key, mod_api::Text::literal("Processor")),
                )
            })
            .unwrap()
            .unwrap()
            .unwrap();
        game.lan.as_mut().unwrap().hub.guests.update(0, |g| {
            for slot in g.inventory.iter_mut() {
                *slot = Some(ItemStack::new("minecraft:cobblestone", 64));
            }
            g.carried = None;
            g.native_menu = vec![stick(4), dirt(5), stone(4)];
        });
        game.dropped.clear();
        CLOSED_OWNED_LEN.set(None);
        game.lan_guest_packet(
            1,
            S::ContainerClose(sunrise_protocol::packets::play::ContainerClose {
                container: window,
            }),
        );
        game.run_native_menu_events();
        assert_eq!(CLOSED_OWNED_LEN.get(), Some(0));
        let dropped_count = |game: &Game<'_>, id: &str| -> u32 {
            game.dropped
                .iter()
                .filter(|e| &*e.stack.item == id)
                .map(|e| e.stack.count)
                .sum()
        };
        assert_eq!(dropped_count(game, "minecraft:stick"), 4);
        assert_eq!(dropped_count(game, "minecraft:dirt"), 5);
        assert_eq!(dropped_count(game, "minecraft:stone"), 0);

        // 3. Disconnect drops non-result inputs directly even when inventory has space.
        let _window = game
            .as_guest(1, |game| {
                player.open_mod_menu(
                    game,
                    mod_api::ModMenuOpen::new(key, mod_api::Text::literal("Processor")),
                )
            })
            .unwrap()
            .unwrap()
            .unwrap();
        game.lan.as_mut().unwrap().hub.guests.update(0, |g| {
            g.inventory.fill(None);
            g.carried = None;
            g.native_menu = vec![stick(2), dirt(1), stone(1)];
        });
        game.dropped.clear();
        game.guest_disconnected(1);
        game.run_native_menu_events();
        let guest = game.lan.as_ref().unwrap().hub.guest(1).unwrap();
        assert_eq!(count_inv(guest, "minecraft:stick"), 0);
        assert_eq!(count_inv(guest, "minecraft:dirt"), 0);
        assert_eq!(dropped_count(game, "minecraft:stick"), 2);
        assert_eq!(dropped_count(game, "minecraft:dirt"), 1);
        assert_eq!(dropped_count(game, "minecraft:stone"), 0);
    });
}

#[test]
#[ignore = "daily"]
fn processing_owned_menu_close_on_death_drops_inputs_unlike_live_close() {
    with_processing_owned_menu(|game, _rx, key| {
        let player = game.player(123).unwrap();
        let stick = |n| Some(ItemStack::new("minecraft:stick", n));
        let dirt = |n| Some(ItemStack::new("minecraft:dirt", n));
        let stone = |n| Some(ItemStack::new("minecraft:stone", n));
        let count_inv = |g: &sunrise_client::lan::guest::Guest, id: &str| -> u32 {
            g.inventory
                .iter()
                .flatten()
                .filter(|s| &*s.item == id)
                .map(|s| s.count)
                .sum()
        };
        let dropped_count = |game: &Game<'_>, id: &str| -> u32 {
            game.dropped
                .iter()
                .filter(|e| &*e.stack.item == id)
                .map(|e| e.stack.count)
                .sum()
        };

        // 1. Client ContainerClose while player is dead drops non-result inputs instead of
        // placing them into the dead player's inventory, and discards results.
        let window = game
            .as_guest(1, |game| {
                player.open_mod_menu(
                    game,
                    mod_api::ModMenuOpen::new(key, mod_api::Text::literal("Processor")),
                )
            })
            .unwrap()
            .unwrap()
            .unwrap();
        game.lan.as_mut().unwrap().hub.guests.update(0, |g| {
            g.inventory.fill(None);
            g.carried = None;
            g.dead = true;
            g.native_menu = vec![stick(3), dirt(2), stone(2)];
        });
        game.dropped.clear();
        game.lan_guest_packet(
            1,
            S::ContainerClose(sunrise_protocol::packets::play::ContainerClose {
                container: window,
            }),
        );
        game.run_native_menu_events();
        let guest = game.lan.as_ref().unwrap().hub.guest(1).unwrap();
        assert_eq!(count_inv(guest, "minecraft:stick"), 0);
        assert_eq!(count_inv(guest, "minecraft:dirt"), 0);
        assert_eq!(dropped_count(game, "minecraft:stick"), 3);
        assert_eq!(dropped_count(game, "minecraft:dirt"), 2);
        assert_eq!(dropped_count(game, "minecraft:stone"), 0);

        // 2. Hub-side close_container while dead (e.g. respawn path before dead flag clears)
        // records drop_instead = true so inputs still drop even if g.dead clears before
        // run_native_menu_events runs.
        game.lan.as_mut().unwrap().hub.guests.update(0, |g| {
            g.dead = false;
            g.inventory.fill(None);
        });
        let _window = game
            .as_guest(1, |game| {
                player.open_mod_menu(
                    game,
                    mod_api::ModMenuOpen::new(key, mod_api::Text::literal("Processor")),
                )
            })
            .unwrap()
            .unwrap()
            .unwrap();
        game.lan.as_mut().unwrap().hub.guests.update(0, |g| {
            g.dead = true;
            g.native_menu = vec![stick(5), dirt(4), stone(4)];
        });
        game.dropped.clear();
        game.lan.as_mut().unwrap().hub.close_container(1);
        game.lan.as_mut().unwrap().hub.guests.update(0, |g| {
            g.dead = false;
        });
        game.run_native_menu_events();
        let guest = game.lan.as_ref().unwrap().hub.guest(1).unwrap();
        assert_eq!(count_inv(guest, "minecraft:stick"), 0);
        assert_eq!(count_inv(guest, "minecraft:dirt"), 0);
        assert_eq!(dropped_count(game, "minecraft:stick"), 5);
        assert_eq!(dropped_count(game, "minecraft:dirt"), 4);
        assert_eq!(dropped_count(game, "minecraft:stone"), 0);
    });
}
