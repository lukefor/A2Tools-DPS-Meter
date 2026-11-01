import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import test from 'node:test';
import vm from 'node:vm';

const source = readFileSync(new URL('../public/src/js/core.js', import.meta.url), 'utf8');
const settle = () => new Promise(resolve => setImmediate(resolve));
function deferred() {
  let resolve, reject;
  const promise = new Promise((done, fail) => { resolve = done; reject = fail; });
  return { promise, resolve, reject };
}
const status = (suffix = 'initial', port = 18731) => ({
  enabled: true, port, urls: [`http://192.0.2.1:${port}/${suffix}`], error: null,
});

function setup({ read = () => status(), configure = (enabled, port) => ({ ...status('configured', port), enabled }),
  newKey = () => status('new-key') } = {}) {
  const calls = [], events = new Map(), values = { 'dpsMeter.streamOverlayName': 'Player' };
  function element() {
    const handlers = new Map(), classes = new Set();
    return {
      handlers, children: [], style: {}, dataset: {}, value: '', checked: false, textContent: '',
      classList: { add: name => classes.add(name), remove: name => classes.delete(name), contains: name => classes.has(name) },
      addEventListener(name, handler) {
        const entries = handlers.get(name) || [];
        entries.push(handler);
        handlers.set(name, entries);
      },
      fire(name) { handlers.get(name)?.forEach(handler => handler({ stopPropagation() {} })); },
      replaceChildren(...children) { this.children = children; },
      append(...children) { this.children.push(...children); },
      appendChild(child) { this.children.push(child); },
      blur() {}, focus() {}, select() {},
    };
  }
  const group = element();
  const controls = Object.fromEntries(['Checkbox', 'Details', 'PortInput', 'NewKeyBtn', 'Urls', 'Status', 'NameInput']
    .map(name => [name, element()]));
  group.querySelector = selector => controls[selector.replace('.streamOverlay', '')];
  const document = {
    readyState: 'loading', activeElement: null, addEventListener() {},
    querySelector: selector => selector === '.streamOverlayGroup' ? group : null,
    createElement: () => element(),
  };
  const window = {
    A2_VIEW: 'settings',
    addEventListener(name, handler) {
      const handlers = events.get(name) || [];
      handlers.push(handler);
      events.set(name, handlers);
    },
    javaBridge: {
      streamOverlayStatus: () => { calls.push(['status']); return Promise.resolve(read()); },
      streamOverlayConfigure: (enabled, port) => { calls.push(['configure', enabled, port]); return Promise.resolve(configure(enabled, port)); },
      streamOverlayNewKey: () => { calls.push(['new-key']); return Promise.resolve(newKey()); },
    },
  };
  const context = vm.createContext({ window, document, console, setTimeout, clearTimeout });
  vm.runInContext(source, context);
  const app = vm.runInContext('Object.create(DpsApp.prototype)', context);
  app.safeGetSetting = key => values[key] ?? null;
  app.safeSetSetting = (key, value) => { calls.push(['setting', key, value]); values[key] = value; };
  app.initStreamOverlaySettings();
  return {
    app, calls, controls, document, group, values,
    hide: () => events.get('settings-hidden')?.forEach(handler => handler()),
    urls: () => controls.Urls.children.map(row => row.children[0].value),
  };
}

test('reopening refreshes OBS URLs and controls without installing duplicate handlers', async () => {
  let current = status();
  const fixture = setup({ read: () => current });
  await settle();
  assert.deepEqual(fixture.urls(), current.urls);
  fixture.hide();
  current = status('after-network-change', 20000);
  fixture.values['dpsMeter.streamOverlayName'] = 'New name';
  for (let i = 0; i < 10; i++) fixture.app.initStreamOverlaySettings();
  await fixture.app.refreshStreamOverlaySettings();
  assert.deepEqual(fixture.urls(), current.urls);
  assert.equal(fixture.controls.PortInput.value, '20000');
  assert.equal(fixture.controls.NameInput.value, 'New name');
  assert.equal(fixture.controls.Checkbox.handlers.get('change').length, 1);
  assert.equal(fixture.controls.PortInput.handlers.get('change').length, 1);
  assert.equal(fixture.controls.NewKeyBtn.handlers.get('click').length, 1);
  assert.equal(fixture.calls.filter(call => call[0] === 'status').length, 2);
});

test('the latest OBS refresh wins over an older pending response', async () => {
  const old = deferred(), latest = deferred();
  let reads = 0;
  const fixture = setup({ read: () => ++reads === 1 ? old.promise : latest.promise });
  const refreshed = fixture.app.refreshStreamOverlaySettings();
  latest.resolve(status('current'));
  await refreshed;
  old.resolve(status('obsolete'));
  await settle();
  assert.deepEqual(fixture.urls(), status('current').urls);
});

test('late OBS responses do not paint a hidden form, and the next refresh can paint it', async () => {
  const old = deferred();
  let reads = 0;
  const fixture = setup({ read: () => ++reads === 1 ? old.promise : status('reopened') });
  await settle();
  fixture.hide();
  old.resolve(status('hidden'));
  await settle();
  assert.deepEqual(fixture.urls(), []);
  await fixture.app.refreshStreamOverlaySettings();
  assert.deepEqual(fixture.urls(), status('reopened').urls);
});

test('a stale OBS request failure cannot overwrite a successful newer response', async () => {
  const old = deferred();
  let reads = 0;
  const fixture = setup({ read: () => ++reads === 1 ? old.promise : status('current') });
  await fixture.app.refreshStreamOverlaySettings();
  old.reject(new Error('obsolete failure'));
  await settle();
  assert.deepEqual(fixture.urls(), status('current').urls);
  assert.equal(fixture.controls.Status.textContent, '');
});

test('OBS refresh preserves a name or port that is currently being edited', async () => {
  const fixture = setup();
  await settle();
  fixture.controls.PortInput.value = '22222';
  fixture.document.activeElement = fixture.controls.PortInput;
  await fixture.app.refreshStreamOverlaySettings();
  assert.equal(fixture.controls.PortInput.value, '22222');
  fixture.controls.NameInput.value = 'Draft name';
  fixture.document.activeElement = fixture.controls.NameInput;
  await fixture.app.refreshStreamOverlaySettings();
  assert.equal(fixture.controls.NameInput.value, 'Draft name');
  fixture.controls.NameInput.fire('change');
  assert.equal(fixture.values['dpsMeter.streamOverlayName'], 'Draft name');
});

test('rapid OBS edits reach the backend in order and a refresh waits for both mutations', async () => {
  const first = deferred(), second = deferred();
  let changes = 0, current = status();
  const fixture = setup({
    read: () => current,
    configure: (enabled, port) => (++changes === 1 ? first.promise : second.promise)
      .then(() => (current = { ...status('configured', port), enabled })),
  });
  await settle();
  fixture.controls.PortInput.value = '20000';
  fixture.controls.PortInput.fire('change');
  fixture.controls.PortInput.value = '21000';
  fixture.controls.Checkbox.checked = false;
  fixture.controls.Checkbox.fire('change');
  const refreshed = fixture.app.refreshStreamOverlaySettings();
  await settle();
  assert.deepEqual(fixture.calls.filter(call => call[0] === 'configure'), [['configure', true, 20000]]);
  assert.equal(fixture.calls.filter(call => call[0] === 'status').length, 1);
  first.resolve();
  await settle();
  assert.deepEqual(fixture.calls.filter(call => call[0] === 'configure'), [
    ['configure', true, 20000], ['configure', false, 21000],
  ]);
  assert.equal(fixture.calls.filter(call => call[0] === 'status').length, 1);
  second.resolve();
  await refreshed;
  assert.equal(fixture.controls.Checkbox.checked, false);
  assert.equal(fixture.controls.PortInput.value, '21000');
  assert.equal(fixture.controls.Details.style.display, 'none');
});

test('a new OBS key supersedes an earlier status response and is applied before the next read', async () => {
  const old = deferred(), rotating = deferred();
  let reads = 0, current = status();
  const fixture = setup({
    read: () => ++reads === 1 ? old.promise : current,
    newKey: () => rotating.promise.then(() => (current = status('rotated'))),
  });
  await settle();
  fixture.controls.NewKeyBtn.fire('click');
  const refreshed = fixture.app.refreshStreamOverlaySettings();
  await settle();
  assert.equal(reads, 1);
  rotating.resolve();
  await refreshed;
  old.resolve(status('old-key'));
  await settle();
  assert.deepEqual(fixture.urls(), status('rotated').urls);
});

test('a rejected OBS mutation does not prevent later edits or refreshes', async () => {
  let changes = 0;
  const fixture = setup({ configure: (enabled, port) => ++changes === 1
    ? Promise.reject(new Error('configure failed')) : status('recovered', port) });
  await settle();
  fixture.controls.PortInput.value = '20000';
  fixture.controls.PortInput.fire('change');
  await settle();
  assert.equal(fixture.controls.Status.textContent, 'configure failed');
  fixture.controls.PortInput.value = '21000';
  fixture.controls.PortInput.fire('change');
  await settle();
  assert.deepEqual(fixture.urls(), status('recovered', 21000).urls);
});

test('an invalid OBS port cannot be erased by an older pending refresh', async () => {
  const old = deferred();
  const fixture = setup({ read: () => old.promise });
  await settle();
  fixture.controls.Checkbox.checked = true;
  fixture.controls.PortInput.value = '99999';
  fixture.controls.PortInput.fire('change');
  old.resolve(status());
  await settle();
  assert.equal(fixture.controls.PortInput.value, '99999');
  assert.ok(fixture.controls.PortInput.classList.contains('isInvalid'));
  assert.equal(fixture.controls.Status.textContent, 'Enter a port between 1024 and 65535.');
  assert.ok(!fixture.calls.some(call => call[0] === 'configure'));
});
