//! Server-side `menu_slot_changed` dispatch on `MenuPart`.
struct ProcessingMenuPart;
impl MenuPart for ProcessingMenuPart {
    fn menu_subscribed(&self) -> &'static [Option<HookId>] {
        &[]
    }

    fn menu_slot_changed(&self, _: &mut dyn ServerHost, event: &mut MenuSlotChangedEvent<'_>) {
        assert_eq!(event.menu, "test:processor");
        assert_eq!(event.changed.len(), 2);
        assert_eq!(event.changed[0].container, MenuContainer::Main);
        assert_eq!(event.changed[0].slot, 0);
        assert_eq!(event.changed[1].container, MenuContainer::Main);
        assert_eq!(event.changed[1].slot, 1);
        assert_eq!(event.context.player, Some(7));
        event.outputs.push(MenuSlotWrite {
            container: MenuContainer::Main,
            slot: 2,
            stack: Some(ItemStack {
                item: "result".into(),
                count: 1,
            }),
        });
    }
}

#[test]
fn menu_slot_changed_batches_changes_and_writes_outputs() {
    install(Vec::<ModEntry>::new().into());
    install_parts(Parts {
        menus: vec![Box::leak(Box::new(ProcessingMenuPart))],
        ..Parts::default()
    });
    let changed = [
        MenuSlotChange {
            container: MenuContainer::Main,
            slot: 0,
            before: None,
            after: Some(ItemStack {
                item: "input_a".into(),
                count: 2,
            }),
        },
        MenuSlotChange {
            container: MenuContainer::Main,
            slot: 1,
            before: None,
            after: Some(ItemStack {
                item: "input_b".into(),
                count: 1,
            }),
        },
    ];
    let context = MenuSlotContext {
        player: Some(7),
        pos: None,
        slots: vec![vec![
            Some(ItemStack {
                item: "input_a".into(),
                count: 2,
            }),
            Some(ItemStack {
                item: "input_b".into(),
                count: 1,
            }),
            None,
        ]],
    };
    let mut outputs = Vec::new();
    dispatch_menu_slot_changed(
        &mut TestHost,
        calls::MenuSlotChangedCall {
            menu: "test:processor",
            changed: &changed,
            context: &context,
            outputs: &mut outputs,
        },
    );
    assert_eq!(outputs.len(), 1);
    assert_eq!(outputs[0].slot, 2);
    assert_eq!(outputs[0].stack.as_ref().unwrap().item.as_ref(), "result");
}
