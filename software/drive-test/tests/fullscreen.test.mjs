// SPDX-License-Identifier: GPL-3.0-only
// Test the actual page script with a small DOM/Fullscreen API model. No browser
// process or network is started; native browser rendering is checked manually.
import test from 'node:test';
import assert from 'node:assert/strict';
import {readFileSync} from 'node:fs';
import vm from 'node:vm';

const source = readFileSync(new URL('../src/app.js', import.meta.url), 'utf8');

class Target {
  listeners = new Map();
  addEventListener(name, callback) {
    if (!this.listeners.has(name)) this.listeners.set(name, []);
    this.listeners.get(name).push(callback);
  }
  dispatch(name, event = {}) {
    for (const callback of this.listeners.get(name) || []) callback(event);
  }
}

function page({supported = true, rejected = false} = {}) {
  const document = new Target();
  const window = new Target();
  const nodes = new Map();
  const element = id => {
    if (!nodes.has(id)) {
      const node = new Target();
      const classes = new Set();
      Object.assign(node, {
        disabled:false, hidden:false, value:'80', textContent:'', attributes:new Map(),
        classList:{
          toggle(name, enabled) { if (enabled) classes.add(name); else classes.delete(name); },
          contains(name) { return classes.has(name); }
        },
        setAttribute(name, value) { this.attributes.set(name, value); },
        focus() { document.activeElement = node; },
        matches() { return false; }
      });
      nodes.set(id, node);
    }
    return nodes.get(id);
  };
  Object.assign(document, {
    body:element('body'), fullscreenEnabled:supported, fullscreenElement:null,
    getElementById:element, querySelectorAll:() => [], hasFocus:() => true,
    async exitFullscreen() {
      this.fullscreenElement = null;
      this.dispatch('fullscreenchange');
    }
  });
  const view = element('drive-view');
  view.requestFullscreen = async () => {
    if (rejected) throw new Error('Permission denied');
    document.fullscreenElement = view;
    document.dispatch('fullscreenchange');
  };
  const sockets = [];
  class Socket {
    static OPEN = 1;
    readyState = 1;
    sent = [];
    constructor() { sockets.push(this); }
    send(message) { this.sent.push(JSON.parse(message)); }
    close() { this.readyState = 3; }
  }
  const context = vm.createContext({
    document, window, WebSocket:Socket, URL, URLSearchParams,
    location:{hash:'', href:'http://localhost/', pathname:'/', search:''},
    sessionStorage:{getItem:() => ''}, performance:{now:() => 1000},
    setInterval:() => 0, setTimeout:() => 0,
    fetch:() => Promise.resolve({}), console:{info() {}}, navigator:{}
  });
  vm.runInContext(source, context);
  const evaluate = text => vm.runInContext(text, context);
  function activate() {
    evaluate(`active = true; serverStatus = {serial_ready:true, camera_required:false,
      camera_ready:true, controlling:true}; controlSocket = new WebSocket(); keys.add('KeyW');`);
    return sockets.at(-1);
  }
  async function key(code, repeat = false) {
    const event = {code, repeat, target:document.body, defaultPrevented:false,
      preventDefault() { this.defaultPrevented = true; }};
    window.dispatch('keydown', event);
    await Promise.resolve();
    return event;
  }
  return {document, window, element, view, evaluate, activate, key};
}

test('entering and leaving fullscreen preserves the control socket and neutralizes held throttle', async () => {
  const p = page();
  const socket = p.activate();
  await p.evaluate('toggleFullscreen()');
  assert.equal(p.document.fullscreenElement, p.view);
  assert.equal(p.view.classList.contains('immersive'), true);
  assert.equal(p.document.body.classList.contains('view-open'), true);
  assert.equal(p.element('fullscreen').attributes.get('aria-pressed'), 'true');
  assert.equal(p.evaluate('active && paused'), true);
  assert.equal(p.evaluate('axes().throttle'), 0);
  assert.deepEqual(socket.sent, [{type:'pause'}]);
  await p.evaluate('leaveFullscreen()');
  assert.equal(p.document.fullscreenElement, null);
  assert.equal(p.view.classList.contains('immersive'), false);
  assert.equal(p.document.body.classList.contains('view-open'), false);
  assert.equal(p.evaluate('active'), true);
  assert.equal(socket.readyState, 1);
  assert.equal(p.element('fullscreen').disabled, false);
});

test('Escape stops driving and exits fullscreen', async () => {
  const p = page();
  const socket = p.activate();
  await p.evaluate('toggleFullscreen()');
  const event = await p.key('Escape');
  assert.equal(event.defaultPrevented, true);
  assert.equal(p.evaluate('active'), false);
  assert.equal(p.document.fullscreenElement, null);
  assert.equal(p.evaluate('immersive'), false);
  assert.ok(socket.sent.some(message => message.type === 'stop' && message.reason === 'Stop requested from keyboard'));
});

test('native Escape exit still stops when the browser does not deliver keydown', async () => {
  const p = page();
  const socket = p.activate();
  await p.evaluate('toggleFullscreen()');
  await p.document.exitFullscreen();
  assert.equal(p.evaluate('active'), false);
  assert.equal(p.evaluate('immersive'), false);
  assert.ok(socket.sent.some(message => message.type === 'stop' && message.reason === 'Full screen exited by browser'));
});

for (const options of [{supported:false}, {rejected:true}]) {
  test(`expanded view stays usable when fullscreen is ${options.supported === false ? 'unsupported' : 'denied'}`, async () => {
    const p = page(options);
    await p.evaluate('toggleFullscreen()');
    assert.equal(p.evaluate('immersive'), true);
    assert.equal(p.document.fullscreenElement, null);
    assert.equal(p.element('view-notice').hidden, false);
    assert.equal(p.element('fullscreen').disabled, false);
    await p.evaluate('leaveFullscreen()');
    assert.equal(p.evaluate('immersive'), false);
    assert.equal(p.element('view-notice').hidden, true);
  });
}

test('F toggles the view once and suppresses repeated typing defaults', async () => {
  const p = page();
  assert.equal((await p.key('KeyF')).defaultPrevented, true);
  assert.equal(p.evaluate('immersive'), true);
  assert.equal((await p.key('KeyF', true)).defaultPrevented, true);
  assert.equal(p.evaluate('immersive'), true);
  await p.key('KeyF');
  assert.equal(p.evaluate('immersive'), false);
});

test('leaving the browser still stops control while fullscreen is active', async () => {
  const p = page();
  const socket = p.activate();
  await p.evaluate('toggleFullscreen()');
  p.window.dispatch('blur');
  assert.equal(p.evaluate('active'), false);
  assert.ok(socket.sent.some(message => message.type === 'stop' && message.reason === 'Window lost focus'));
});
