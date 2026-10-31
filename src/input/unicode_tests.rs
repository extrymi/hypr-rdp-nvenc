//! Independent client-side XKB interpretation of the actor's emitted stream.
use super::*;
use crate::input::keyboard::{generate_xkb_keymap_from_names, XkbKeymapNames};
use ironrdp_pdu::input::fast_path::SynchronizeFlags;
use xkbcommon::xkb;

#[derive(Default, Debug)]
struct GlyphSink {
    keymap: Vec<u8>,
    modifiers: KeyboardModifierState,
    glyphs: Vec<u32>,
    keys: Vec<(u32, bool)>,
    wire_held: Vec<u32>,
    unknown_releases: usize,
    mouse_modifiers: Vec<KeyboardModifierState>,
    modifier_packets: usize,
    reject_keymaps: bool,
}

impl GlyphSink {
    // KeyboardSink is Send, whereas XKB objects are not. Create the independent
    // client state locally from the exact announced keymap and modifier packet.
    fn client_state(&self) -> xkb::State {
        let bytes = self.keymap.strip_suffix(&[0]).unwrap_or(&self.keymap);
        let text = String::from_utf8(bytes.to_vec()).expect("oracle keymap is UTF-8");
        let context = xkb::Context::new(xkb::CONTEXT_NO_FLAGS);
        let keymap = xkb::Keymap::new_from_string(
            &context,
            text,
            xkb::KEYMAP_FORMAT_TEXT_V1,
            xkb::KEYMAP_COMPILE_NO_FLAGS,
        )
        .expect("announced keymap compiles in independent oracle");
        let mut state = xkb::State::new(&keymap);
        let m = self.modifiers;
        state.update_mask(m.depressed, m.latched, m.locked, 0, 0, m.group);
        state
    }
}

impl KeyboardSink for GlyphSink {
    fn key(&mut self, _time: u32, evdev_key: u32, pressed: bool) {
        self.keys.push((evdev_key, pressed));
        if pressed {
            let character = self
                .client_state()
                .key_get_utf32(xkb::Keycode::new(evdev_key + 8));
            if character != 0 {
                self.glyphs.push(character);
            }
            if !self.wire_held.contains(&evdev_key) {
                self.wire_held.push(evdev_key);
            }
        } else if self.wire_held.contains(&evdev_key) {
            self.wire_held.retain(|key| *key != evdev_key);
        } else {
            self.unknown_releases += 1;
        }
    }

    fn modifiers(&mut self, state: KeyboardModifierState) {
        self.modifier_packets += 1;
        self.modifiers = state;
    }

    fn keymap(&mut self, data: &[u8]) -> bool {
        if self.reject_keymaps { return false; }
        self.keymap = data.to_vec();
        true
    }

    fn flush(&mut self) {}
}

impl InputBackend for GlyphSink {
    fn mouse(&mut self, _time: u32, _event: MouseEvent) {
        self.mouse_modifiers.push(self.modifiers);
    }
}

struct Driver {
    tracker: KeyboardStateTracker,
    held: HeldInput,
    sink: GlyphSink,
    keymap: Vec<u8>,
    epoch: Instant,
}

impl Driver {
    fn new(layout: &str) -> Self {
        let keymap = generate_xkb_keymap_from_names(&XkbKeymapNames {
            layout: Some(layout.into()),
            options: Some("grp:alt_shift_toggle".into()),
            ..Default::default()
        })
        .expect("test keymap compiles");
        Self::from_keymap(keymap)
    }

    fn from_keymap(keymap: Vec<u8>) -> Self {
        let tracker = KeyboardStateTracker::new(&keymap).expect("tracker loads");
        let mut sink = GlyphSink::default();
        assert!(sink.keymap(&keymap));
        sink.modifiers(tracker.modifier_state());
        Self {
            tracker,
            held: HeldInput::default(),
            sink,
            keymap,
            epoch: Instant::now(),
        }
    }

    fn command(&mut self, command: InputCommand) {
        handle_command(
            &mut self.tracker,
            &mut self.keymap,
            &mut self.held,
            &mut self.sink,
            &self.epoch,
            command,
        );
    }

    fn keyboard(&mut self, event: KeyboardEvent) {
        self.command(InputCommand::Keyboard(event));
    }

    fn character(&mut self, character: char) {
        self.keyboard(KeyboardEvent::UnicodePressed(character as u16));
        self.keyboard(KeyboardEvent::UnicodeReleased(character as u16));
    }
}

#[test]
fn unicode_selects_the_requested_group_in_the_emitted_stream() {
    let mut d = Driver::new("us,ru");
    let raw_before = d.tracker.modifier_state();
    d.character('ф');
    assert_eq!(
        d.sink.glyphs,
        [u32::from('ф')],
        "Unicode output mismatch: wrong group"
    );
    assert_eq!(
        d.tracker.modifier_state(),
        raw_before,
        "Unicode changed raw XKB state"
    );
    assert_eq!(
        d.sink.modifiers, raw_before,
        "Unicode did not restore raw modifiers"
    );
    assert!(d.sink.wire_held.is_empty());
    assert_eq!(
        d.sink.unknown_releases, 0,
        "actor emitted an unowned key release"
    );
}

#[test]
fn unicode_selects_the_keymaps_altgr_level() {
    let mut d = Driver::new("de");
    let raw_before = d.tracker.modifier_state();
    d.character('€');
    assert_eq!(
        d.sink.glyphs,
        [u32::from('€')],
        "Unicode output mismatch: wrong level"
    );
    assert_eq!(
        d.tracker.modifier_state(),
        raw_before,
        "Unicode changed raw XKB state"
    );
    assert_eq!(
        d.sink.modifiers, raw_before,
        "Unicode did not restore raw modifiers"
    );
    assert!(d.sink.wire_held.is_empty());
    assert_eq!(
        d.sink.unknown_releases, 0,
        "actor emitted an unowned key release"
    );
}

#[test]
fn unicode_is_exact_with_caps_lock() {
    let mut d = Driver::new("us");
    d.keyboard(KeyboardEvent::Synchronize(SynchronizeFlags::CAPS_LOCK));
    let raw_before = d.tracker.modifier_state();
    d.character('a');
    assert_eq!(
        d.sink.glyphs,
        [u32::from('a')],
        "Unicode output mismatch: CapsLock leaked"
    );
    assert_eq!(
        d.tracker.modifier_state(),
        raw_before,
        "Unicode changed raw XKB state"
    );
    assert_eq!(
        d.sink.modifiers, raw_before,
        "Unicode did not restore raw modifiers"
    );
    assert!(d.sink.wire_held.is_empty());
    assert_eq!(
        d.sink.unknown_releases, 0,
        "actor emitted an unowned key release"
    );
}

#[test]
fn unicode_is_exact_with_a_held_shift() {
    let mut d = Driver::new("us");
    d.keyboard(KeyboardEvent::Pressed {
        code: 0x2a,
        extended: false,
    });
    let raw_before = d.tracker.modifier_state();
    d.character('a');
    assert_eq!(
        d.sink.glyphs,
        [u32::from('a')],
        "Unicode output mismatch: raw Shift leaked"
    );
    assert_eq!(
        d.tracker.modifier_state(),
        raw_before,
        "Unicode changed raw XKB state"
    );
    assert_eq!(
        d.sink.modifiers, raw_before,
        "Unicode did not restore raw modifiers"
    );
    assert!(d.sink.wire_held.contains(&42));
    d.keyboard(KeyboardEvent::Released {
        code: 0x2a,
        extended: false,
    });
    assert!(d.sink.wire_held.is_empty());
    assert_eq!(
        d.sink.unknown_releases, 0,
        "actor emitted an unowned key release"
    );
}

#[test]
fn unicode_selects_us_text_while_the_raw_group_is_russian() {
    let mut d = Driver::new("us,ru");
    d.keyboard(KeyboardEvent::Pressed {
        code: 0x38,
        extended: false,
    });
    d.keyboard(KeyboardEvent::Pressed {
        code: 0x2a,
        extended: false,
    });
    d.keyboard(KeyboardEvent::Released {
        code: 0x2a,
        extended: false,
    });
    d.keyboard(KeyboardEvent::Released {
        code: 0x38,
        extended: false,
    });
    let raw_before = d.tracker.modifier_state();
    assert_eq!(raw_before.group, 1, "raw group toggle precondition");
    d.character('a');
    assert_eq!(
        d.sink.glyphs,
        [u32::from('a')],
        "Unicode output mismatch: wrong group"
    );
    assert_eq!(
        d.tracker.modifier_state(),
        raw_before,
        "Unicode changed raw XKB state"
    );
    assert_eq!(
        d.sink.modifiers, raw_before,
        "Unicode did not restore raw modifiers"
    );
    assert!(d.sink.wire_held.is_empty());
    assert_eq!(
        d.sink.unknown_releases, 0,
        "actor emitted an unowned key release"
    );
}

#[test]
fn unicode_does_not_turn_into_a_control_character() {
    let mut d = Driver::new("us");
    d.keyboard(KeyboardEvent::Pressed {
        code: 0x1d,
        extended: false,
    });
    let raw_before = d.tracker.modifier_state();
    d.character('a');
    assert_eq!(
        d.sink.glyphs,
        [u32::from('a')],
        "Unicode output mismatch: Control leaked"
    );
    assert_eq!(
        d.tracker.modifier_state(),
        raw_before,
        "Unicode changed raw XKB state"
    );
    assert_eq!(
        d.sink.modifiers, raw_before,
        "Unicode did not restore raw modifiers"
    );
    assert!(d.sink.wire_held.contains(&29));
    d.keyboard(KeyboardEvent::Released {
        code: 0x1d,
        extended: false,
    });
    assert!(d.sink.wire_held.is_empty());
    assert_eq!(
        d.sink.unknown_releases, 0,
        "actor emitted an unowned key release"
    );
}


#[test]
fn unicode_repeats_and_aliases_keep_each_remaining_owner() {
    for reverse in [false, true] {
        let mut d = Driver::new("us,ru");
        let raw_before = d.tracker.modifier_state();
        d.keyboard(KeyboardEvent::UnicodePressed('a' as u16));
        d.keyboard(KeyboardEvent::UnicodePressed('ф' as u16));
        let (first, last) = if reverse { ('ф', 'a') } else { ('a', 'ф') };
        d.keyboard(KeyboardEvent::UnicodeReleased(first as u16));
        assert!(d.sink.wire_held.contains(&30));
        assert_eq!(d.sink.client_state().key_get_utf32(xkb::Keycode::new(38)), u32::from(last));
        d.keyboard(KeyboardEvent::UnicodeReleased(last as u16));
        assert_eq!(d.sink.glyphs, [u32::from('a'), u32::from('ф')]);
        assert_eq!(d.sink.keys.iter().filter(|key| **key == (30, false)).count(), 1);
        assert!(d.sink.wire_held.is_empty());
        assert_eq!(d.sink.unknown_releases, 0);
        assert_eq!(d.tracker.modifier_state(), raw_before);
        assert_eq!(d.sink.modifiers, raw_before);
    }
    let mut d = Driver::new("us");
    d.keyboard(KeyboardEvent::UnicodePressed('A' as u16));
    d.keyboard(KeyboardEvent::UnicodePressed('A' as u16));
    d.keyboard(KeyboardEvent::UnicodeReleased('A' as u16));
    assert_eq!(d.sink.glyphs, [u32::from('A'), u32::from('A')]);
    assert_eq!(d.sink.keys, [(30, true), (30, true), (30, false)]);
    assert!(d.sink.wire_held.is_empty());
    assert_eq!(d.sink.modifiers, KeyboardModifierState::default());
}

#[test]
fn unicode_does_not_release_a_shared_raw_key_in_either_order() {
    for unicode_first in [false, true] {
        let mut d = Driver::new("us");
        let raw_down = KeyboardEvent::Pressed { code: 0x1e, extended: false };
        let raw_up = KeyboardEvent::Released { code: 0x1e, extended: false };
        if unicode_first {
            d.keyboard(KeyboardEvent::UnicodePressed('a' as u16));
            d.keyboard(raw_down);
            d.keyboard(raw_up);
            assert!(d.sink.wire_held.contains(&30));
            d.keyboard(KeyboardEvent::UnicodeReleased('a' as u16));
        } else {
            d.keyboard(raw_down);
            d.keyboard(KeyboardEvent::UnicodePressed('a' as u16));
            d.keyboard(KeyboardEvent::UnicodeReleased('a' as u16));
            assert!(d.sink.wire_held.contains(&30));
            d.keyboard(raw_up);
        }
        assert_eq!(d.sink.glyphs, [u32::from('a'), u32::from('a')]);
        assert_eq!(d.sink.keys.iter().filter(|key| **key == (30, false)).count(), 1);
        assert!(d.sink.wire_held.is_empty());
        assert_eq!(d.sink.unknown_releases, 0);
    }
}

#[test]
fn unicode_projection_preserves_raw_ctrl_click_and_latest_repeat_owner() {
    let mut d = Driver::new("us,ru");
    d.keyboard(KeyboardEvent::Pressed { code: 0x1d, extended: false });
    let ctrl = d.tracker.modifier_state();
    d.keyboard(KeyboardEvent::UnicodePressed('ф' as u16));
    d.command(InputCommand::Mouse(MouseEvent::ButtonRel {
        x: 0, y: 0, button: MouseButton::Left, pressed: true,
    }));
    assert_eq!(d.sink.mouse_modifiers.last(), Some(&ctrl));
    assert_eq!(d.sink.client_state().key_get_utf32(xkb::Keycode::new(38)), u32::from('ф'));
    d.keyboard(KeyboardEvent::Released { code: 0x1d, extended: false });
    d.keyboard(KeyboardEvent::Pressed { code: 0x30, extended: false });
    assert_eq!(d.sink.client_state().key_get_utf32(xkb::Keycode::new(56)), u32::from('b'));
    d.keyboard(KeyboardEvent::Released { code: 0x30, extended: false });
    assert_eq!(d.sink.client_state().key_get_utf32(xkb::Keycode::new(38)), u32::from('ф'));
    d.keyboard(KeyboardEvent::UnicodeReleased('ф' as u16));
    d.command(InputCommand::ReleaseHeldInput);
    assert_eq!(d.sink.glyphs, [u32::from('ф'), u32::from('b')]);
    assert!(d.sink.wire_held.is_empty());
    assert_eq!(d.sink.unknown_releases, 0);
    assert_eq!(d.sink.modifiers, KeyboardModifierState::default());
}

#[test]
fn unicode_ignores_own_feedback_and_restores_a_new_raw_group() {
    let mut d = Driver::new("us,ru");
    d.keyboard(KeyboardEvent::UnicodePressed('ф' as u16));
    d.command(InputCommand::SetLockedLayout {
        layout_name: "English (US)".into(), from_own_keyboard: true,
    });
    assert_eq!(d.tracker.modifier_state().group, 0);
    assert_eq!(d.sink.modifiers.group, 1);
    let packets = d.sink.modifier_packets;
    d.command(InputCommand::SetLockedLayout {
        layout_name: "Russian".into(), from_own_keyboard: true,
    });
    assert_eq!(d.sink.modifier_packets, packets);
    d.keyboard(KeyboardEvent::Pressed { code: 0x38, extended: false });
    d.keyboard(KeyboardEvent::Pressed { code: 0x2a, extended: false });
    d.keyboard(KeyboardEvent::Released { code: 0x2a, extended: false });
    d.keyboard(KeyboardEvent::Released { code: 0x38, extended: false });
    assert_eq!(d.tracker.modifier_state().group, 1);
    let raw_now = d.tracker.modifier_state();
    d.keyboard(KeyboardEvent::UnicodeReleased('ф' as u16));
    assert_eq!(d.sink.modifiers, raw_now);
    assert!(d.sink.wire_held.is_empty());
    assert_eq!(d.sink.unknown_releases, 0);
}

#[test]
fn unicode_keymap_replacement_clears_old_owners_and_rejection_restores_projection() {
    for reject in [false, true] {
        let mut d = Driver::new("us,ru");
        let character = if reject { 'ф' } else { 'z' };
        d.keyboard(KeyboardEvent::UnicodePressed(character as u16));
        d.sink.reject_keymaps = reject;
        let german = generate_xkb_keymap_from_names(&XkbKeymapNames {
            layout: Some("de".into()), ..Default::default()
        }).unwrap();
        d.command(InputCommand::ApplyKeymap { keymap_data: german, keymap_source: "test" });
        if reject {
            assert!(d.sink.wire_held.contains(&30));
            assert_eq!(d.sink.client_state().key_get_utf32(xkb::Keycode::new(38)), u32::from(character));
        } else {
            assert!(d.sink.wire_held.is_empty());
        }
        let count = d.sink.keys.len();
        d.keyboard(KeyboardEvent::UnicodeReleased(character as u16));
        if !reject { assert_eq!(d.sink.keys.len(), count); }
        d.character(character);
        assert_eq!(d.sink.glyphs, [u32::from(character), u32::from(character)]);
        assert!(d.sink.wire_held.is_empty());
        assert_eq!(d.sink.unknown_releases, 0);
        assert_eq!(d.sink.modifiers, d.tracker.modifier_state());
    }
}

#[test]
fn unicode_cleanup_deduplicates_aliases_for_sync_and_session_end() {
    for synchronize in [false, true] {
        let mut d = Driver::new("us,ru");
        d.keyboard(KeyboardEvent::Pressed { code: 0x1d, extended: false });
        d.keyboard(KeyboardEvent::UnicodePressed('A' as u16));
        d.keyboard(KeyboardEvent::UnicodePressed('ф' as u16));
        if synchronize {
            d.keyboard(KeyboardEvent::Synchronize(SynchronizeFlags::CAPS_LOCK));
        } else {
            d.command(InputCommand::ReleaseHeldInput);
        }
        assert_eq!(d.sink.keys.iter().filter(|key| **key == (30, false)).count(), 1);
        assert!(d.sink.wire_held.is_empty());
        assert_eq!(d.sink.unknown_releases, 0);
        assert_eq!(d.sink.modifiers, d.tracker.modifier_state());
        let key_count = d.sink.keys.len();
        d.keyboard(KeyboardEvent::UnicodeReleased('A' as u16));
        d.keyboard(KeyboardEvent::UnicodeReleased('ф' as u16));
        assert_eq!(d.sink.keys.len(), key_count);
    }
}

#[test]
fn unicode_invalid_units_and_unmatched_ups_have_no_wire_effects() {
    for overlay in [false, true] {
        let mut d = Driver::new("us,ru");
        d.keyboard(KeyboardEvent::Pressed { code: 0x1d, extended: false });
        if overlay { d.keyboard(KeyboardEvent::UnicodePressed('A' as u16)); }
        let packets = d.sink.modifier_packets;
        let keys = d.sink.keys.len();
        let glyphs = d.sink.glyphs.clone();
        let raw = d.tracker.modifier_state();
        for unit in [0, 0xd800, 0xdc00, 0xffff, 0x4e00] {
            d.keyboard(KeyboardEvent::UnicodePressed(unit));
            d.keyboard(KeyboardEvent::UnicodeReleased(unit));
        }
        d.keyboard(KeyboardEvent::UnicodeReleased('q' as u16));
        assert_eq!(d.sink.modifier_packets, packets);
        assert_eq!(d.sink.keys.len(), keys);
        assert_eq!(d.sink.glyphs, glyphs);
        assert_eq!(d.tracker.modifier_state(), raw);
        d.command(InputCommand::ReleaseHeldInput);
        assert!(d.sink.wire_held.is_empty());
        assert_eq!(d.sink.unknown_releases, 0);
    }
}

#[test]
fn unicode_projection_survives_a_nonrepeating_raw_lock_key() {
    let mut d = Driver::new("us,ru");
    d.keyboard(KeyboardEvent::UnicodePressed('ф' as u16));
    d.keyboard(KeyboardEvent::Pressed { code: 0x3a, extended: false });
    d.keyboard(KeyboardEvent::Released { code: 0x3a, extended: false });
    let raw_now = d.tracker.modifier_state();
    assert_ne!(raw_now.locked, 0);
    assert_eq!(d.sink.client_state().key_get_utf32(xkb::Keycode::new(38)), u32::from('ф'));
    d.keyboard(KeyboardEvent::UnicodeReleased('ф' as u16));
    assert_eq!(d.sink.modifiers, raw_now);
    assert!(d.sink.wire_held.is_empty());
    assert_eq!(d.sink.unknown_releases, 0);
}


#[test]
fn unicode_restores_raw_alt_super_right_shift_and_altgr_owners() {
    for (code, extended, evdev) in [(0x38, false, 56), (0x5b, true, 125),
        (0x36, false, 54), (0x38, true, 100)]
    {
        let mut d = Driver::new("us");
        d.keyboard(KeyboardEvent::Pressed { code, extended });
        let raw_before = d.tracker.modifier_state();
        d.keyboard(KeyboardEvent::UnicodePressed('a' as u16));
        assert_eq!(d.sink.glyphs, [u32::from('a')]);
        let state = d.sink.client_state();
        for name in ["Control", "Mod1", "Mod4"] {
            assert!(!state.mod_name_is_active(name, xkb::STATE_MODS_EFFECTIVE));
        }
        d.keyboard(KeyboardEvent::UnicodeReleased('a' as u16));
        assert_eq!(d.tracker.modifier_state(), raw_before);
        assert_eq!(d.sink.modifiers, raw_before);
        assert!(d.sink.wire_held.contains(&evdev));
        d.keyboard(KeyboardEvent::Released { code, extended });
        assert!(d.sink.wire_held.is_empty());
        assert_eq!(d.sink.unknown_releases, 0);
    }
}

#[test]
fn unicode_uses_a_keymaps_mod3_selector_instead_of_guessing_shift() {
    let mut data = br#"xkb_keymap {
        xkb_keycodes { include "evdev+aliases(qwerty)" };
        xkb_types {
            include "complete"
            type "CUSTOM_MOD3" {
                modifiers = Mod3;
                map[None] = Level1;
                map[Mod3] = Level2;
            };
        };
        xkb_compatibility { include "complete" };
        xkb_symbols {
            include "pc+us"
            replace key <AC01> {
                type[Group1] = "CUSTOM_MOD3",
                symbols[Group1] = [a, EuroSign]
            };
        };
    };"#.to_vec();
    data.push(0);
    let mut d = Driver::from_keymap(data);
    let raw_before = d.tracker.modifier_state();
    d.keyboard(KeyboardEvent::UnicodePressed('€' as u16));
    assert_eq!(d.sink.glyphs, [u32::from('€')]);
    let state = d.sink.client_state();
    assert!(state.mod_name_is_active("Mod3", xkb::STATE_MODS_EFFECTIVE));
    assert!(!state.mod_name_is_active("Shift", xkb::STATE_MODS_EFFECTIVE));
    d.keyboard(KeyboardEvent::UnicodeReleased('€' as u16));
    assert_eq!(d.tracker.modifier_state(), raw_before);
    assert_eq!(d.sink.modifiers, raw_before);
    assert!(d.sink.wire_held.is_empty());
    assert_eq!(d.sink.unknown_releases, 0);
}

struct SharedGlyphSink(std::sync::Arc<std::sync::Mutex<GlyphSink>>);

impl KeyboardSink for SharedGlyphSink {
    fn key(&mut self, time: u32, key: u32, pressed: bool) {
        self.0.lock().unwrap().key(time, key, pressed);
    }
    fn modifiers(&mut self, state: KeyboardModifierState) {
        self.0.lock().unwrap().modifiers(state);
    }
    fn keymap(&mut self, data: &[u8]) -> bool { self.0.lock().unwrap().keymap(data) }
    fn flush(&mut self) {}
}

impl InputBackend for SharedGlyphSink {
    fn mouse(&mut self, time: u32, event: MouseEvent) {
        self.0.lock().unwrap().mouse(time, event);
    }
}

#[test]
fn unicode_actor_shutdown_releases_wire_owners_and_projection() {
    let keymap = generate_xkb_keymap_from_names(&XkbKeymapNames {
        layout: Some("us,ru".into()), ..Default::default()
    }).unwrap();
    let (sender, receiver) = mpsc::channel();
    sender.send(InputCommand::Keyboard(KeyboardEvent::Pressed {
        code: 0x1d, extended: false,
    })).unwrap();
    sender.send(InputCommand::Keyboard(KeyboardEvent::UnicodePressed('A' as u16))).unwrap();
    sender.send(InputCommand::Keyboard(KeyboardEvent::UnicodePressed('ф' as u16))).unwrap();
    drop(sender);
    let trace = std::sync::Arc::new(std::sync::Mutex::new(GlyphSink::default()));
    run_input_actor(receiver, keymap, "test", Instant::now(), SharedGlyphSink(std::sync::Arc::clone(&trace)));
    let sink = trace.lock().unwrap();
    assert_eq!(sink.glyphs, [u32::from('A'), u32::from('ф')]);
    assert!(sink.wire_held.is_empty());
    assert_eq!(sink.unknown_releases, 0);
    assert_eq!(sink.modifiers, KeyboardModifierState::default());
    assert!(!sink.keys.contains(&(42, true)), "Unicode invented a raw Shift owner");
}
