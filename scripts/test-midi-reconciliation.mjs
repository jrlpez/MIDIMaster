import assert from "node:assert/strict";
import { createAppDom } from "./lib/dom_fixture.mjs";
import { createProfilesFeature } from "../src/features/profiles/profiles.js";
import { createMidiFeature } from "../src/features/midi/midi.js";
import { createMidiLearn } from "../src/features/bindings/controllers/midi_learn.js";
import { createBackendEvents } from "../src/app/controllers/backend_events.js";

await createAppDom();
globalThis.localStorage = { getItem: () => null, setItem() {} };
const route = (id, name) => ({ input_device_id: id, output_device_id: `out-${name}`,
  input_device_name: name, output_device_name: name, enabled: true });
const oldRoutes = [route("midi:0", "A"), route("midi:1", "B")];
const newRoutes = [route("midi:1", "A"), route("midi:0", "B")];
const binding = (id, device) => ({ id, device_id: device,
  control: { channel: 0, controller: 7, msg_type: "ControlChange" },
  mute_control: { device_id: device, channel: 0, controller: 16, msg_type: "Note" } });
let bindings = [binding("a", "midi:0"), binding("b", "midi:1")];
let preference = { routes: oldRoutes, configured: true };
const repaired = { name: "Default", bindings: [binding("a", "midi:1"), binding("b", "midi:0")],
  midi_device_preference: { routes: newRoutes }, midi_device_preference_set: true };
const calls = [];
let releaseNative;
let enteredNative;
const entered = new Promise((resolve) => { enteredNative = resolve; });
let busy = false;
const profiles = createProfilesFeature({
  invoke: async (command, args) => {
    calls.push({ command, args: structuredClone(args) });
    if (command === "start_midi_device_routes") {
      enteredNative();
      return new Promise((resolve) => { releaseNative = resolve; });
    }
  },
  getActiveProfileName: () => "Default", getBindings: () => bindings,
  setBindings: (next) => { bindings = next; },
  getActiveProfileMidiPreference: () => preference,
  setActiveProfileMidiPreference: (next) => { preference = next; },
  setMidiReconciliationBusy: (next) => { busy = next; },
});
try {
  const editSave = profiles.saveBindingsForProfile();
  const reconciliation = profiles.reconcileMidiRoutes({ routes: newRoutes, desiredRoutes: newRoutes });
  await entered;
  await editSave;
  assert.equal(busy, true);
  assert.equal(calls[0].command, "set_active_profile_preference");
  assert.equal(calls[1].command, "save_profile", "existing edits must save before reconciliation");
  assert.equal(calls[2].args.expectedProfileName, "Default");
  const lateSave = profiles.saveBindingsForProfile();
  const flushing = profiles.flushProfileSave();
  await Promise.resolve();
  assert.equal(calls.filter((c) => c.command === "save_profile").length, 1);
  releaseNative({ profile: repaired, connectedRoutes: newRoutes, complete: true });
  await reconciliation;
  await flushing;
  await lateSave;
  assert.equal(busy, false);
  const saved = calls.filter((c) => c.command === "save_profile").at(-1).args.profile;
  assert.equal(saved.bindings[0].device_id, "midi:1");
  assert.equal(saved.bindings[0].mute_control.device_id, "midi:1");
  assert.deepEqual(saved.midi_device_preference.routes, newRoutes);

  const subscriptions = new Map();
  await createBackendEvents({ eventSubscriptions: { subscribe: async (name, callback) => subscriptions.set(name, callback) } }).setupListeners();
  subscriptions.get("bindings_migrated")?.({ payload: { count: 2, migrations: [
    { bindingId: "a", previousDeviceId: "midi:0", deviceId: "midi:1" },
    { bindingId: "a", previousDeviceId: "midi:1", deviceId: "midi:0" },
  ] } });
  assert.equal(bindings[0].device_id, "midi:1", "late migration deltas cannot replay a swap");
} finally { await profiles.dispose(); }

// An incomplete apply must retain the backend's original ownership baseline.
let devices = [route("midi:1", "A")];
let attempts = 0;
const midi = createMidiFeature({
  dom: {},
  invoke: async (command) => {
    if (command === "list_midi_devices") return devices.map((r) => ({ id: r.input_device_id, name: r.input_device_name }));
    if (command === "list_midi_output_devices") return devices.map((r) => ({ id: r.output_device_id, name: r.output_device_name }));
    if (command === "get_midi_route_health") return [];
    return null;
  },
  reconcileMidiRoutes: async (args) => {
    attempts += 1;
    assert.equal(args.desiredRoutes.length, 2, "include the missing desired route");
    return devices.length === 1
      ? { connectedRoutes: [], failedRoutes: [{ route: devices[0] }], complete: false,
          profile: { ...repaired, bindings: [binding("a", "midi:0"), binding("b", "midi:1")], midi_device_preference: { routes: oldRoutes } } }
      : { profile: repaired, connectedRoutes: newRoutes, failedRoutes: [], complete: true };
  },
  onProfileDeviceSelected: () => assert.fail("must not save guessed preferences after authoritative response"),
});
try {
  await midi.syncToProfileDevice({ routes: oldRoutes, configured: true });
  assert.deepEqual(midi.getDesiredMidiPreference().routes.map((r) => r.inputDeviceId), ["midi:0", "midi:1"]);
  await midi.checkAvailabilityNow();
  assert.equal(attempts, 2, "deferred device groups must keep retrying");
  devices = newRoutes;
  await midi.checkAvailabilityNow();
  assert.equal(attempts, 3);
  assert.deepEqual(midi.getDesiredMidiPreference().routes.map((r) => r.inputDeviceId), ["midi:1", "midi:0"]);
  await midi.checkAvailabilityNow();
  assert.equal(attempts, 3, "healthy unchanged ownership does not need another apply");
} finally { midi.dispose(); }

const mute = document.createElement("span"), assign = document.createElement("span");
const labels = createMidiLearn({ elements: { bindingConfigMuteLabel: mute, bindingConfigAssignLabel: assign },
  labelForMidiDevice: (id) => id === "midi:1" ? "Deck A (Unavailable)" : "Deck B", t: (key) => key });
labels.renderMuteMappingLabel(repaired.bindings[0]);
labels.renderAssignMappingLabel({ ...repaired.bindings[0], assign_control: repaired.bindings[1].mute_control });
assert.match(mute.textContent, /Deck A \(Unavailable\)/);
assert.match(mute.textContent, /Ch 0 Note 16/);
assert.match(assign.textContent, /Deck B/);
console.log("MIDI reconciliation ownership, save barrier, notification and mapping label tests passed");
