//! Independent client-side XKB interpretation of the actor's emitted stream.
use super::*;
use crate::input::keyboard::{generate_xkb_keymap_from_names, XkbKeymapNames};
use ironrdp_pdu::input::fast_path::SynchronizeFlags;
use xkbcommon::xkb;

#[derive(Default)]
struct GlyphSink {
    keymap: Vec<u8>,
    modifiers: KeyboardModifierState,
    glyphs: Vec<u32>,
    keys: Vec<(u32, bool)>,
    wire_held: Vec<u32>,
    unknown_releases: usize,
    mouse_modifiers: Vec<KeyboardModifierState>,
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
        ).expect("announced keymap compiles in independent oracle");
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
            let character = self.client_state().key_get_utf32(xkb::Keycode::new(evdev_key + 8));
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
        self.modifiers = state;
    }

    fn keymap(&mut self, data: &[u8]) -> bool {
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
        }).expect("test keymap compiles");
        let tracker = KeyboardStateTracker::new(&keymap).expect("tracker loads");
        let mut sink = GlyphSink::default();
        assert!(sink.keymap(&keymap));
        sink.modifiers(tracker.modifier_state());
        Self { tracker, held: HeldInput::default(), sink, keymap, epoch: Instant::now() }
    }

    fn command(&mut self, command: InputCommand) {
        handle_command(&mut self.tracker, &mut self.keymap, &mut self.held,
            &mut self.sink, &self.epoch, command);
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
    assert_eq!(d.sink.glyphs, [u32::from('ф')], "Unicode output mismatch: wrong group");
    assert_eq!(d.tracker.modifier_state(), raw_before, "Unicode changed raw XKB state");
    assert_eq!(d.sink.modifiers, raw_before, "Unicode did not restore raw modifiers");
    assert!(d.sink.wire_held.is_empty());
    assert_eq!(d.sink.unknown_releases, 0, "actor emitted an unowned key release");
}

#[test]
fn unicode_selects_the_keymaps_altgr_level() {
    let mut d = Driver::new("de");
    let raw_before = d.tracker.modifier_state();
    d.character('€');
    assert_eq!(d.sink.glyphs, [u32::from('€')], "Unicode output mismatch: wrong level");
    assert_eq!(d.tracker.modifier_state(), raw_before, "Unicode changed raw XKB state");
    assert_eq!(d.sink.modifiers, raw_before, "Unicode did not restore raw modifiers");
    assert!(d.sink.wire_held.is_empty());
    assert_eq!(d.sink.unknown_releases, 0, "actor emitted an unowned key release");
}

#[test]
fn unicode_is_exact_with_caps_lock() {
    let mut d = Driver::new("us");
    d.keyboard(KeyboardEvent::Synchronize(SynchronizeFlags::CAPS_LOCK));
    let raw_before = d.tracker.modifier_state();
    d.character('a');
    assert_eq!(d.sink.glyphs, [u32::from('a')], "Unicode output mismatch: CapsLock leaked");
    assert_eq!(d.tracker.modifier_state(), raw_before, "Unicode changed raw XKB state");
    assert_eq!(d.sink.modifiers, raw_before, "Unicode did not restore raw modifiers");
    assert!(d.sink.wire_held.is_empty());
    assert_eq!(d.sink.unknown_releases, 0, "actor emitted an unowned key release");
}

#[test]
fn unicode_is_exact_with_a_held_shift() {
    let mut d = Driver::new("us");
    d.keyboard(KeyboardEvent::Pressed { code: 0x2a, extended: false });
    let raw_before = d.tracker.modifier_state();
    d.character('a');
    assert_eq!(d.sink.glyphs, [u32::from('a')], "Unicode output mismatch: raw Shift leaked");
    assert_eq!(d.tracker.modifier_state(), raw_before, "Unicode changed raw XKB state");
    assert_eq!(d.sink.modifiers, raw_before, "Unicode did not restore raw modifiers");
    assert!(d.sink.wire_held.contains(&42));
    d.keyboard(KeyboardEvent::Released { code: 0x2a, extended: false });
    assert!(d.sink.wire_held.is_empty());
    assert_eq!(d.sink.unknown_releases, 0, "actor emitted an unowned key release");
}

#[test]
fn unicode_selects_us_text_while_the_raw_group_is_russian() {
    let mut d = Driver::new("us,ru");
    d.keyboard(KeyboardEvent::Pressed { code: 0x38, extended: false });
    d.keyboard(KeyboardEvent::Pressed { code: 0x2a, extended: false });
    d.keyboard(KeyboardEvent::Released { code: 0x2a, extended: false });
    d.keyboard(KeyboardEvent::Released { code: 0x38, extended: false });
    let raw_before = d.tracker.modifier_state();
    assert_eq!(raw_before.group, 1, "raw group toggle precondition");
    d.character('a');
    assert_eq!(d.sink.glyphs, [u32::from('a')], "Unicode output mismatch: wrong group");
    assert_eq!(d.tracker.modifier_state(), raw_before, "Unicode changed raw XKB state");
    assert_eq!(d.sink.modifiers, raw_before, "Unicode did not restore raw modifiers");
    assert!(d.sink.wire_held.is_empty());
    assert_eq!(d.sink.unknown_releases, 0, "actor emitted an unowned key release");
}

#[test]
fn unicode_does_not_turn_into_a_control_character() {
    let mut d = Driver::new("us");
    d.keyboard(KeyboardEvent::Pressed { code: 0x1d, extended: false });
    let raw_before = d.tracker.modifier_state();
    d.character('a');
    assert_eq!(d.sink.glyphs, [u32::from('a')], "Unicode output mismatch: Control leaked");
    assert_eq!(d.tracker.modifier_state(), raw_before, "Unicode changed raw XKB state");
    assert_eq!(d.sink.modifiers, raw_before, "Unicode did not restore raw modifiers");
    assert!(d.sink.wire_held.contains(&29));
    d.keyboard(KeyboardEvent::Released { code: 0x1d, extended: false });
    assert!(d.sink.wire_held.is_empty());
    assert_eq!(d.sink.unknown_releases, 0, "actor emitted an unowned key release");
}
